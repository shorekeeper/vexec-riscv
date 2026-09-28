//! Bounded queue, with proof.
//!
//! Verified standalone: `verus spec/queue.rs --crate-type=lib`.
//!
//! Generic in the element type, because the proof never inspects an element.
//! It reads a slot, writes a slot, and moves indices; what is in the slot is
//! irrelevant to every obligation. That makes the console receive ring and the
//! deferred call queue two uses of one proof rather than two proofs.
//!
//! What is proved:
//!
//!   1. No panic and no arithmetic overflow on any path.
//!   2. `well_formed` holds after every operation, given it held before.
//!   3. `push` appends exactly one element when there is room and changes
//!      nothing when there is not.
//!   4. `pop` removes exactly the oldest element and returns it.
//!
//! Points 3 and 4 are stated against `contents`, an abstract sequence, so a
//! caller reasons about a queue of values and never about slots.
//!
//! No modular arithmetic, in the code or in the specification. A wrapping
//! index written as a remainder forces the solver to reason about
//! divisibility, which is slow and brittle. Written as a conditional subtract
//! it is linear arithmetic, and the specification is written the same way so
//! the two match without a lemma bridging them.
//!
//! One capacity for every instantiation. Per use sizing needs a const generic
//! parameter, which is a separate thing to validate; until then the deferred
//! call queue is larger than it needs to be, at sixteen bytes an entry.

use vstd::prelude::*;

verus! {

broadcast use vstd::array::group_array_axioms;

/// Elements a queue holds.
pub const CAPACITY: usize = 256;

/// Fixed size queue.
///
/// Occupied slots run from `head`, wrapping, for `len` entries. The explicit
/// length removes the full and empty ambiguity a head and tail pair has on its
/// own, at the cost of one word.
pub struct Ring<T> {
    entries: [T; CAPACITY],
    head: usize,
    len: usize,
    dropped: u64,
}

/// Array slot holding logical entry `i`.
///
/// Defined for `0 <= head < CAPACITY` and `0 <= i < CAPACITY`, the only range
/// it is used in. A conditional subtract rather than a remainder, for the
/// reason in the module comment.
pub open spec fn slot_of(head: int, i: int) -> int {
    if head + i < CAPACITY as int {
        head + i
    } else {
        head + i - CAPACITY as int
    }
}

/// Distinct logical positions occupy distinct slots.
///
/// The injectivity that makes this a queue rather than a set. Without it,
/// writing one entry would be indistinguishable from overwriting another.
///
/// Three cases, all linear: both positions before the wrap, both after, or one
/// of each. The mixed case is the only one worth stating: `head + i` is below
/// CAPACITY and `head + j` is not, so they are equal only when `j` equals
/// `i + CAPACITY`, which the bound on `j` rules out.
pub proof fn lemma_slot_injective(head: int, i: int, j: int)
    requires
        0 <= head < CAPACITY as int,
        0 <= i < CAPACITY as int,
        0 <= j < CAPACITY as int,
        i != j,
    ensures
        slot_of(head, i) != slot_of(head, j),
{
}

/// Every slot is inside the array.
pub proof fn lemma_slot_bounds(head: int, i: int)
    requires
        0 <= head < CAPACITY as int,
        0 <= i < CAPACITY as int,
    ensures
        0 <= slot_of(head, i) < CAPACITY as int,
{
}

/// Advancing the head by one shifts every position down by one.
///
/// What makes `pop` a removal of the first entry rather than an arbitrary
/// rearrangement.
pub proof fn lemma_slot_shift(head: int, i: int)
    requires
        0 <= head < CAPACITY as int,
        0 <= i,
        i + 1 < CAPACITY as int,
    ensures
        slot_of(if head + 1 == CAPACITY as int { 0int } else { head + 1 }, i)
            == slot_of(head, i + 1),
{
}

impl<T: Copy> Ring<T> {
    /// The structural invariant.
    ///
    /// Assumed and re-established by every operation. It is what makes every
    /// array index in this file provably in range.
    ///
    /// Closed because the fields are private and a caller has no business
    /// knowing what it says. It comes from `new` and is carried forward.
    pub closed spec fn well_formed(&self) -> bool {
        &&& self.head < CAPACITY
        &&& self.len <= CAPACITY
    }

    /// The elements held, oldest first.
    ///
    /// The abstract value of the queue. Callers reason about this and never
    /// about slots, which is what lets the representation change without
    /// changing anything that depends on it.
    pub closed spec fn contents(&self) -> Seq<T> {
        Seq::new(
            self.len as nat,
            |i: int| self.entries@[slot_of(self.head as int, i)],
        )
    }

    /// Empty queue.
    ///
    /// `fill` occupies every slot and is never read: a slot outside the
    /// occupied range is not part of `contents`. It exists because an array
    /// has to be initialised and there is no allocator to avoid that.
    // const, because the executive is a static initialised at compile time and
    // there is no allocator to build one at run time instead.
    pub const fn new(fill: T) -> (result: Ring<T>)
        ensures
            result.well_formed(),
            result.contents() =~= Seq::<T>::empty(),
            result.dropped_count() == 0,
    {
        Ring { entries: [fill; CAPACITY], head: 0, len: 0, dropped: 0 }
    }

    /// Elements held.
    pub fn len(&self) -> (result: usize)
        requires
            self.well_formed(),
        ensures
            result == self.contents().len(),
    {
        self.len
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> (result: bool)
        requires
            self.well_formed(),
        ensures
            result == (self.contents().len() == 0),
    {
        self.len == 0
    }

    /// True when no further element fits.
    pub fn is_full(&self) -> (result: bool)
        requires
            self.well_formed(),
        ensures
            result == (self.contents().len() == CAPACITY),
    {
        self.len == CAPACITY
    }

    /// Elements rejected for want of room.
    pub closed spec fn dropped_count(&self) -> nat {
        self.dropped as nat
    }

    /// Elements rejected for want of room.
    pub fn dropped(&self) -> (result: u64)
        ensures
            result == self.dropped_count(),
    {
        self.dropped
    }

    /// Appends an element. Returns false when the queue was full.
    ///
    /// The oldest is kept and the newest rejected. For console input that is
    /// the right way round, because what was typed first is what the reader is
    /// waiting on; for deferred calls it means the work already scheduled runs
    /// rather than being displaced by newer work.
    pub fn push(&mut self, value: T) -> (result: bool)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            result == (old(self).contents().len() < CAPACITY),
            result ==> final(self).contents() =~= old(self).contents().push(value),
            !result ==> final(self).contents() =~= old(self).contents(),
    {
        if self.len == CAPACITY {
            // Saturating, written out. A wrapped counter reports fewer losses
            // than occurred, which is worse than a stuck maximum.
            if self.dropped < u64::MAX {
                self.dropped = self.dropped + 1;
            }
            return false;
        }

        // head is below CAPACITY and len is at most CAPACITY, so the sum is
        // below twice CAPACITY and one conditional subtract is enough.
        let mut slot = self.head + self.len;
        if slot >= CAPACITY {
            slot = slot - CAPACITY;
        }

        proof {
            lemma_slot_bounds(self.head as int, self.len as int);
        }
        assert(slot as int == slot_of(self.head as int, self.len as int));

        self.entries[slot] = value;
        self.len = self.len + 1;

        proof {
            // The trigger is stated rather than inferred. Both sides mention
            // slot_of, so the solver has two candidates and picks by
            // heuristic; pinning it means a later edit cannot silently change
            // which one is used.
            assert forall|i: int| #![trigger slot_of(self.head as int, i)]
                0 <= i < old(self).len implies
                self.entries@[slot_of(self.head as int, i)]
                    == old(self).entries@[slot_of(old(self).head as int, i)]
            by {
                lemma_slot_injective(self.head as int, i, old(self).len as int);
                lemma_slot_bounds(self.head as int, i);
            }
            assert(self.contents() =~= old(self).contents().push(value));
        }

        true
    }

    /// Removes and returns the oldest element.
    pub fn pop(&mut self) -> (result: Option<T>)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            old(self).contents().len() == 0 ==> {
                &&& result is None
                &&& final(self).contents() =~= old(self).contents()
            },
            old(self).contents().len() > 0 ==> {
                &&& result == Some(old(self).contents()[0])
                &&& final(self).contents() =~= old(self).contents().drop_first()
            },
    {
        if self.len == 0 {
            return None;
        }

        assert(self.head as int == slot_of(self.head as int, 0));
        let value = self.entries[self.head];

        // Read by the proof below, which the compiler does not see.
        #[allow(unused_variables)]
        let old_head = self.head;
        self.head = self.head + 1;
        if self.head == CAPACITY {
            self.head = 0;
        }
        self.len = self.len - 1;

        proof {
            // The array is untouched, so each remaining entry keeps its value
            // and only moves down one position.
            assert forall|i: int| #![trigger slot_of(self.head as int, i)]
                0 <= i < self.len implies
                self.entries@[slot_of(self.head as int, i)]
                    == old(self).entries@[slot_of(old_head as int, i + 1)]
            by {
                lemma_slot_shift(old_head as int, i);
            }
            assert(self.contents() =~= old(self).contents().drop_first());
        }

        Some(value)
    }
}

/// Console receive queue.
pub type ByteRing = Ring<u8>;

/// One deferred call.
///
/// The routine is an address rather than a function pointer. The queue never
/// calls it and a plain integer keeps the proof free of any question about
/// what a function value is; the kernel casts at the point of the call, which
/// is outside what is verified.
#[derive(Clone, Copy)]
pub struct Dpc {
    /// Address of the routine.
    pub routine: usize,
    /// Value passed to it. Usually an index into a driver table, not a
    /// pointer: nothing here can establish that a pointer is still valid by
    /// the time the routine runs.
    pub context: usize,
}

/// Deferred call queue.
pub type DpcQueue = Ring<Dpc>;

} // verus!