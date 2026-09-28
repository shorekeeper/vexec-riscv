//! Deferred procedure call routine type.
//!
//! The queue itself is in queue.rs and is generated from a proof. It stores an
//! address rather than a function pointer, because a queue that never calls
//! what it holds has no reason to know what a function value is, and keeping
//! it out leaves the proof free of that question.
//!
//! The cast back happens at the point of the call, in the kernel image, where
//! it is one unsafe expression with the invariant stated beside it.

/// Routine a deferred call runs.
///
/// Cannot be a closure: there is no allocator to hold one and no lifetime that
/// outlives the queue.
pub type DpcRoutine = fn(context: usize);

/// Deferred calls run in one drain before the loop gives up.
///
/// A routine may queue further calls and those run in the same drain, which
/// terminates only because a routine that queues unconditionally is a defect.
/// The bound turns that defect into a diagnosable stall rather than a hang.
pub const MAX_DRAIN: usize = 1024;