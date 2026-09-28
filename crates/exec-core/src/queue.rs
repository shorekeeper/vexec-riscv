// GENERATED. Do not edit.
//
// Produced from spec/queue.rs by scripts/spec-strip.ps1, which only deletes:
// ghost items, specification clauses, proof blocks and asserts. Every
// executable statement below is the one that was verified, character for
// character.
//
// Regenerate:  .\scripts\spec-strip.ps1 -In spec/queue.rs -Out <path>
// Check:       .\scripts\spec-strip.ps1 -In spec/queue.rs -Out <path> -Check
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

impl<T: Copy> Ring<T> {

    /// Empty queue.
    ///
    /// `fill` occupies every slot and is never read: a slot outside the
    /// occupied range is not part of `contents`. It exists because an array
    /// has to be initialised and there is no allocator to avoid that.
    // const, because the executive is a static initialised at compile time and
    // there is no allocator to build one at run time instead.
    pub const fn new(fill: T) -> Ring<T>
    {
        Ring { entries: [fill; CAPACITY], head: 0, len: 0, dropped: 0 }
    }

    /// Elements held.
    pub fn len(&self) -> usize
    {
        self.len
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool
    {
        self.len == 0
    }

    /// True when no further element fits.
    pub fn is_full(&self) -> bool
    {
        self.len == CAPACITY
    }

    /// Elements rejected for want of room.
    pub fn dropped(&self) -> u64
    {
        self.dropped
    }

    /// Appends an element. Returns false when the queue was full.
    ///
    /// The oldest is kept and the newest rejected. For console input that is
    /// the right way round, because what was typed first is what the reader is
    /// waiting on; for deferred calls it means the work already scheduled runs
    /// rather than being displaced by newer work.
    pub fn push(&mut self, value: T) -> bool
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

        self.entries[slot] = value;
        self.len = self.len + 1;

        true
    }

    /// Removes and returns the oldest element.
    pub fn pop(&mut self) -> Option<T>
    {
        if self.len == 0 {
            return None;
        }

        let value = self.entries[self.head];

        // Read by the proof below, which the compiler does not see.
        #[allow(unused_variables)]
        let old_head = self.head;
        self.head = self.head + 1;
        if self.head == CAPACITY {
            self.head = 0;
        }
        self.len = self.len - 1;

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
