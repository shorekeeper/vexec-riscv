// GENERATED. Do not edit.
//
// Produced from spec/objects.rs by scripts/spec-strip.ps1, which only deletes:
// ghost items, specification clauses, proof blocks and asserts. Every
// executable statement below is the one that was verified, character for
// character.
//
// Regenerate:  .\scripts\spec-strip.ps1 -In spec/objects.rs -Out <path>
// Check:       .\scripts\spec-strip.ps1 -In spec/objects.rs -Out <path> -Check
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

/// Slots in one table.
pub const CAPACITY: usize = 256;

/// Generation value of a slot that is out of service.
///
/// Reached by a slot released 65535 times. Such a slot is never allocated
/// again, so no handle bearing this generation is ever issued.
pub const RETIRED: u16 = 0xffff;

// ---------------------------------------------------------------- encoding

/// Builds a handle.
pub fn encode(index: u16, generation: u16, tag: u8) -> u64
{
    (index as u64) | ((generation as u64) << 16) | ((tag as u64) << 32)
}

/// Splits a handle into index, generation and tag.
///
/// One place where the masks appear, so every entry point decodes identically
/// and the narrowing casts are justified once. Each field is masked to its
/// width before the cast, which is what makes the cast lossless.
pub fn decode(handle: u64) -> (usize, u16, u8)
{

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

    /// Empty table.
    ///
    /// `fill` occupies every slot and is never read: a slot that is not live is
    /// not resolvable, so its payload is unreachable. It exists because an
    /// array has to be initialised and there is no allocator to avoid that.
    ///
    /// const, because the executive is a static.
    pub const fn new(tag: u8, fill: T) -> ObjectTable<T>
    {
        ObjectTable {
            slots: [Slot { payload: fill, generation: 0, live: false, refs: 0 }; CAPACITY],
            tag,
            live_count: 0,
            cursor: 0,
        }
    }

    /// Objects held.
    pub fn live_count(&self) -> usize
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
    pub fn open(&mut self, payload: T) -> Option<u64>
    {
        let mut examined: usize = 0;

        while examined < CAPACITY
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
    pub fn lookup(&self, handle: u64) -> Option<T>
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
    pub fn replace(&mut self, handle: u64, payload: T) -> bool
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

        true
    }

    /// Takes an additional reference.
    ///
    /// A handle is one reference. This is for a kernel structure holding an
    /// object beyond the life of the handle that produced it, which is what
    /// stops a driver from being unloaded while an interrupt path still points
    /// at it.
    pub fn reference(&mut self, handle: u64) -> bool
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

        true
    }

    /// Drops a reference, freeing the object when the last one goes.
    ///
    /// The generation is raised when the slot is freed, which is what makes
    /// every handle to the object that was there stop resolving.
    pub fn release(&mut self, handle: u64) -> bool
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

}
