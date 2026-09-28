//! Object table, with proof.
//!
//! Verified standalone: `verus spec/objects.rs --crate-type=lib`.
//!
//! A fixed array of slots reached through handles. Every kernel resource lives
//! in one of these; the table is generic in the payload because the proof never
//! inspects it, so one proof serves devices, drivers, ports and timers.
//!
//! Handle layout, matching exec-abi:
//!
//! ```text
//!  bits  0..15   slot index
//!  bits 16..31   generation, incremented when a slot is released
//!  bits 32..39   type tag
//!  bits 40..63   reserved, zero
//! ```
//!
//! What is proved:
//!
//!   1. No panic and no arithmetic overflow on any path.
//!   2. A handle returned by `open` resolves, and every handle that resolved
//!      before still does, so opening disturbs nothing.
//!   3. `replace` and `reference` change no handle's resolvability.
//!   4. Generation never decreases, at any index, under any operation.
//!
//! Point 4 is why the others are stated the way they are. Resolution requires
//! an exact generation match, so a generation that only ever rises means a
//! handle that has stopped resolving can never start again: not for the next
//! caller, not after the slot is reused, not ever. That is assembled into
//! `lemma_stale_stays_stale` at the bottom.
//!
//! The cost is stated rather than hidden: sixteen bits give a slot 65535 uses,
//! after which it is retired and never allocated again. Wrapping instead would
//! make a sufficiently old handle valid once more, which is a fault that
//! appears under load, in production, and nowhere else. A leaked slot is
//! arithmetic; a resurrected handle is not.
//!
//! No field update syntax anywhere. Writing `Slot { x, ..old }` leaves the
//! solver without the equalities for the fields it did not see written, and
//! every postcondition here is about a field that did not change.

use vstd::prelude::*;

verus! {

broadcast use vstd::array::group_array_axioms;

/// Slots in one table.
pub const CAPACITY: usize = 256;

/// Generation value of a slot that is out of service.
///
/// Reached by a slot released 65535 times. Such a slot is never allocated
/// again, so no handle bearing this generation is ever issued.
pub const RETIRED: u16 = 0xffff;

// ---------------------------------------------------------------- encoding

/// Handle bits for the parts.
pub open spec fn encode_spec(index: u64, generation: u64, tag: u64) -> u64 {
    index | (generation << 16) | (tag << 32)
}

/// Slot index a handle names.
pub open spec fn index_of(handle: u64) -> u64 {
    handle & 0xffff
}

/// Generation a handle was issued with.
pub open spec fn generation_of(handle: u64) -> u64 {
    (handle >> 16) & 0xffff
}

/// Type tag a handle carries.
pub open spec fn tag_of(handle: u64) -> u64 {
    (handle >> 32) & 0xff
}

/// The parts come back out as they went in.
///
/// The only property of the encoding anything needs. Proved by the bit vector
/// solver, which is the right tool here and the wrong one for anything above:
/// it reasons about bits and knows nothing about slots.
pub proof fn lemma_roundtrip(index: u64, generation: u64, tag: u64)
    requires
        index < 0x10000,
        generation < 0x10000,
        tag < 0x100,
    ensures
        index_of(encode_spec(index, generation, tag)) == index,
        generation_of(encode_spec(index, generation, tag)) == generation,
        tag_of(encode_spec(index, generation, tag)) == tag,
{
    assert(((index | (generation << 16) | (tag << 32)) & 0xffff) == index)
        by (bit_vector)
        requires index < 0x10000, generation < 0x10000, tag < 0x100;

    assert((((index | (generation << 16) | (tag << 32)) >> 16) & 0xffff) == generation)
        by (bit_vector)
        requires index < 0x10000, generation < 0x10000, tag < 0x100;

    assert((((index | (generation << 16) | (tag << 32)) >> 32) & 0xff) == tag)
        by (bit_vector)
        requires index < 0x10000, generation < 0x10000, tag < 0x100;
}

/// Builds a handle.
pub fn encode(index: u16, generation: u16, tag: u8) -> (result: u64)
    ensures
        result == encode_spec(index as u64, generation as u64, tag as u64),
{
    (index as u64) | ((generation as u64) << 16) | ((tag as u64) << 32)
}

/// Splits a handle into index, generation and tag.
///
/// One place where the masks appear, so every entry point decodes identically
/// and the narrowing casts are justified once. Each field is masked to its
/// width before the cast, which is what makes the cast lossless.
pub fn decode(handle: u64) -> (result: (usize, u16, u8))
    ensures
        result.0 as u64 == index_of(handle),
        result.1 as u64 == generation_of(handle),
        result.2 as u64 == tag_of(handle),
{
    assert((handle & 0xffff) < 0x10000) by (bit_vector);
    assert(((handle >> 16) & 0xffff) < 0x10000) by (bit_vector);
    assert(((handle >> 32) & 0xff) < 0x100) by (bit_vector);

    (
        (handle & 0xffff) as usize,
        ((handle >> 16) & 0xffff) as u16,
        ((handle >> 32) & 0xff) as u8,
    )
}

// ---------------------------------------------------------------- table

/// One slot.
#[derive(Clone, Copy)]
pub struct Slot<T> {
    payload: T,
    generation: u16,
    live: bool,
    refs: u32,
}

/// Fixed array of objects reached by handle.
pub struct ObjectTable<T> {
    slots: [Slot<T>; CAPACITY],
    tag: u8,
    live_count: usize,
    cursor: usize,
}

impl<T: Copy> ObjectTable<T> {
    /// The structural invariant.
    ///
    /// The bound on the cursor keeps the scan in range; the bound on the live
    /// count keeps it meaningful. Nothing here mentions generation, because a
    /// generation of any value is well formed. What matters is how it changes,
    /// which is stated on each operation instead.
    pub closed spec fn well_formed(&self) -> bool {
        &&& self.cursor < CAPACITY
        &&& self.live_count <= CAPACITY
    }

    /// Generation held by a slot.
    pub closed spec fn generation_at(&self, index: int) -> int {
        self.slots@[index].generation as int
    }

    /// True when a slot holds an object.
    pub closed spec fn live_at(&self, index: int) -> bool {
        self.slots@[index].live
    }

    /// True when a handle names a live object of this table.
    ///
    /// Four conditions, and the generation is the one doing the work. The tag
    /// rejects a handle from another table before anything is indexed; the
    /// bound rejects a malformed one; the generation rejects a handle whose
    /// object is gone, even though the slot may now hold another.
    pub closed spec fn resolves(&self, handle: u64) -> bool {
        &&& tag_of(handle) == self.tag as u64
        &&& index_of(handle) < CAPACITY as u64
        &&& self.live_at(index_of(handle) as int)
        &&& self.generation_at(index_of(handle) as int) == generation_of(handle) as int
    }

    /// Empty table.
    ///
    /// `fill` occupies every slot and is never read: a slot that is not live is
    /// not resolvable, so its payload is unreachable. It exists because an
    /// array has to be initialised and there is no allocator to avoid that.
    ///
    /// const, because the executive is a static.
    pub const fn new(tag: u8, fill: T) -> (result: ObjectTable<T>)
        ensures
            result.well_formed(),
    {
        ObjectTable {
            slots: [Slot { payload: fill, generation: 0, live: false, refs: 0 }; CAPACITY],
            tag,
            live_count: 0,
            cursor: 0,
        }
    }

    /// Objects held.
    pub fn live_count(&self) -> (result: usize)
        requires
            self.well_formed(),
        ensures
            result <= CAPACITY,
    {
        self.live_count
    }

    /// Places an object and returns a handle to it.
    ///
    /// Next fit rather than first fit, for the same reason as the page
    /// allocator: a scan that restarts at zero costs the whole table once the
    /// low slots are taken.
    ///
    /// A slot whose generation has reached RETIRED is skipped permanently. That
    /// is what bounds the generation, which is what makes a stale handle stale
    /// forever rather than for the next 65535 uses.
    pub fn open(&mut self, payload: T) -> (result: Option<u64>)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            // Generation never falls. Stated on every mutating operation,
            // because it is the conjunction over all of them that makes a
            // released handle permanently dead.
            forall|i: int| #![trigger final(self).generation_at(i)]
                0 <= i < CAPACITY ==>
                    final(self).generation_at(i) >= old(self).generation_at(i),
            result matches Some(handle) ==> final(self).resolves(handle),
            // Opening an object disturbs no other.
            forall|h: u64| #![trigger final(self).resolves(h)]
                old(self).resolves(h) ==> final(self).resolves(h),
    {
        let mut examined: usize = 0;

        while examined < CAPACITY
            invariant
                self.well_formed(),
                self.tag == old(self).tag,
                // The whole array, not the fields separately. It is what
                // connects the local copy of a slot below to the state the
                // postconditions are stated against.
                self.slots@ == old(self).slots@,
            decreases CAPACITY - examined,
        {
            let index = self.cursor;
            let slot = self.slots[index];

            if !slot.live && slot.generation < RETIRED {
                self.slots[index] = Slot {
                    payload,
                    generation: slot.generation,
                    live: true,
                    refs: 1,
                };
                if self.live_count < CAPACITY {
                    self.live_count = self.live_count + 1;
                }

                let handle = encode(index as u16, slot.generation, self.tag);

                proof {
                    lemma_roundtrip(index as u64, slot.generation as u64, self.tag as u64);

                    assert forall|h: u64| #![trigger self.resolves(h)]
                        old(self).resolves(h) implies self.resolves(h)
                    by {
                        if index_of(h) == index as u64 {
                            // A handle that resolved named a live slot. This
                            // one was not live, so the case is empty.
                            assert(old(self).live_at(index as int));
                            assert(!slot.live);
                            assert(false);
                        }
                        // Otherwise the slot is untouched, so both the live
                        // flag and the generation are what they were.
                    }
                }

                return Some(handle);
            }

            self.cursor = self.cursor + 1;
            if self.cursor >= CAPACITY {
                self.cursor = 0;
            }
            examined = examined + 1;
        }

        None
    }

    /// Returns the object a handle names.
    pub fn lookup(&self, handle: u64) -> (result: Option<T>)
        requires
            self.well_formed(),
        ensures
            result is Some <==> self.resolves(handle),
    {
        let (index, generation, tag) = decode(handle);

        if tag != self.tag {
            return None;
        }
        if index >= CAPACITY {
            return None;
        }
        let slot = self.slots[index];
        if !slot.live {
            return None;
        }
        if slot.generation != generation {
            return None;
        }
        Some(slot.payload)
    }

    /// Replaces the object a handle names.
    ///
    /// The way a payload is modified. A borrow out of the table would need a
    /// lifetime argument the proof does not carry, and every payload here is
    /// small enough to copy.
    pub fn replace(&mut self, handle: u64, payload: T) -> (result: bool)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            result == old(self).resolves(handle),
            forall|i: int| #![trigger final(self).generation_at(i)]
                0 <= i < CAPACITY ==>
                    final(self).generation_at(i) == old(self).generation_at(i),
            forall|h: u64| #![trigger final(self).resolves(h)]
                old(self).resolves(h) == final(self).resolves(h),
    {
        let (index, generation, tag) = decode(handle);

        if tag != self.tag || index >= CAPACITY {
            return false;
        }
        let slot = self.slots[index];
        if !slot.live || slot.generation != generation {
            return false;
        }

        self.slots[index] = Slot {
            payload,
            generation: slot.generation,
            live: slot.live,
            refs: slot.refs,
        };

        proof {
            // Resolution reads the live flag and the generation, and this wrote
            // both back unchanged, so no handle changed status.
            assert(self.slots@[index as int].generation
                == old(self).slots@[index as int].generation);
            assert(self.slots@[index as int].live
                == old(self).slots@[index as int].live);
        }

        true
    }

    /// Takes an additional reference.
    ///
    /// A handle is one reference. This is for a kernel structure holding an
    /// object beyond the life of the handle that produced it, which is what
    /// stops a driver from being unloaded while an interrupt path still points
    /// at it.
    pub fn reference(&mut self, handle: u64) -> (result: bool)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            forall|i: int| #![trigger final(self).generation_at(i)]
                0 <= i < CAPACITY ==>
                    final(self).generation_at(i) == old(self).generation_at(i),
            forall|h: u64| #![trigger final(self).resolves(h)]
                old(self).resolves(h) == final(self).resolves(h),
    {
        let (index, generation, tag) = decode(handle);

        if tag != self.tag || index >= CAPACITY {
            return false;
        }
        let slot = self.slots[index];
        if !slot.live || slot.generation != generation {
            return false;
        }
        if slot.refs == 0xffff_ffff {
            // Saturated. Refusing is right: a count that wraps frees an object
            // that is still in use, which is the fault this counter exists to
            // prevent.
            return false;
        }

        self.slots[index] = Slot {
            payload: slot.payload,
            generation: slot.generation,
            live: slot.live,
            refs: slot.refs + 1,
        };

        proof {
            assert(self.slots@[index as int].generation
                == old(self).slots@[index as int].generation);
            assert(self.slots@[index as int].live
                == old(self).slots@[index as int].live);
        }

        true
    }

    /// Drops a reference, freeing the object when the last one goes.
    ///
    /// The generation is raised when the slot is freed, which is what makes
    /// every handle to the object that was there stop resolving.
    pub fn release(&mut self, handle: u64) -> (result: bool)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            forall|i: int| #![trigger final(self).generation_at(i)]
                0 <= i < CAPACITY ==>
                    final(self).generation_at(i) >= old(self).generation_at(i),
            result == old(self).resolves(handle),
    {
        let (index, generation, tag) = decode(handle);

        if tag != self.tag || index >= CAPACITY {
            return false;
        }
        let slot = self.slots[index];
        if !slot.live || slot.generation != generation {
            return false;
        }

        if slot.refs > 1 {
            self.slots[index] = Slot {
                payload: slot.payload,
                generation: slot.generation,
                live: slot.live,
                refs: slot.refs - 1,
            };
            return true;
        }

        // The last reference. The slot is freed and its generation raised, so
        // this handle and every copy of it stops resolving. RETIRED is a
        // ceiling rather than a wrap: a slot that reaches it is never
        // allocated again, which is what keeps a stale handle stale.
        let next = if slot.generation < RETIRED {
            slot.generation + 1
        } else {
            RETIRED
        };
        self.slots[index] = Slot {
            payload: slot.payload,
            generation: next,
            live: false,
            refs: 0,
        };
        if self.live_count > 0 {
            self.live_count = self.live_count - 1;
        }

        true
    }

    /// A handle that has stopped resolving never resolves again.
    ///
    /// The property the whole design rests on, assembled from the
    /// postconditions rather than proved separately. Resolution requires the
    /// slot generation to equal the one in the handle. Every operation leaves
    /// the generation at each index no lower than it was. So once the
    /// generation at an index has passed the value a handle carries, no
    /// sequence of operations brings it back, and the handle is dead for the
    /// life of the machine.
    ///
    /// The hypothesis is the state after a release: the generation has risen
    /// above what the handle holds. Inside the impl because it reads the closed
    /// specifications, which do not unfold outside it.
    pub proof fn lemma_stale_stays_stale(before: &Self, after: &Self, handle: u64)
        requires
            index_of(handle) < CAPACITY as u64,
            before.generation_at(index_of(handle) as int) > generation_of(handle) as int,
            forall|i: int| #![trigger after.generation_at(i)]
                0 <= i < CAPACITY ==> after.generation_at(i) >= before.generation_at(i),
        ensures
            !after.resolves(handle),
    {
        // Instantiates the hypothesis at the index the handle names. The rest
        // is arithmetic: the generation there is at least what it was, what it
        // was exceeds what the handle carries, and resolution wants equality.
        assert(after.generation_at(index_of(handle) as int)
            >= before.generation_at(index_of(handle) as int));
    }
}

} // verus!