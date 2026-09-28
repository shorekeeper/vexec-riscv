//! Physical memory windows, with proof.
//!
//! Verified standalone: `verus spec/window.rs --crate-type=lib`.
//!
//! A world owns a small set of physical regions. Every address a device is
//! given on that world's behalf is checked against them before the device is
//! allowed to proceed. This is what isolation reduces to on a machine with no
//! IOMMU: the hardware will not stop a device from writing anywhere, so the
//! addresses it is handed have to be right before it sees them.
//!
//! What is proved:
//!
//!   1. No panic and no arithmetic overflow on any path.
//!   2. `contains` returns true exactly when the range lies inside some window
//!      of the set with the access it needs.
//!
//! Both directions of point 2 matter, and for different reasons. A false
//! positive lets a device write outside its world, which is the failure this
//! exists to prevent. A false negative refuses a legitimate transfer, which
//! kills a driver that is doing nothing wrong and is the failure that makes
//! people turn the mechanism off.
//!
//! Not one addition appears in the check. The obvious form, `addr + len <=
//! base + size`, is wrong on a range that wraps, and a wrapped range passes.
//! Rearranged to `len <= size - (addr - base)` with both subtractions ordered
//! by earlier tests, there is nothing left to overflow. The specification is
//! written in unbounded integers, so the proof connects the safe form to the
//! meaning rather than to another copy of itself.

use vstd::prelude::*;

verus! {

broadcast use vstd::array::group_array_axioms;

/// Windows one world may hold.
///
/// A split virtqueue needs three regions and its data one more. Four covers a
/// driver with one queue; a driver with several shares a window across them,
/// which is what a contiguous allocation gives anyway.
pub const MAX_WINDOWS: usize = 4;

/// One physical region a world may reach.
#[derive(Clone, Copy)]
pub struct Window {
    /// First byte.
    pub base: u64,
    /// Length. Zero means the entry is unused.
    pub size: u64,
    /// Whether a device may write here.
    ///
    /// A queue a device only reads from does not need to be writable, and a
    /// window that grants more than the transfer requires is a window that
    /// will eventually be used for more.
    pub writable: bool,
}

impl Window {
    /// An unused entry.
    pub const EMPTY: Window = Window { base: 0, size: 0, writable: false };
}

/// True when a range lies inside a region, in unbounded arithmetic.
///
/// The meaning. Everything below is proved to agree with it.
///
/// A zero length range is not contained in anything. It names no bytes, so
/// admitting it would be admitting an address without checking it, which is
/// the shape a probe takes.
pub open spec fn region_holds(base: u64, size: u64, addr: u64, len: u64) -> bool {
    &&& len > 0
    &&& base <= addr
    &&& addr as int + len as int <= base as int + size as int
}

/// A set of windows.
pub struct WindowSet {
    windows: [Window; MAX_WINDOWS],
    count: usize,
}

impl WindowSet {
    /// The structural invariant.
    pub closed spec fn well_formed(&self) -> bool {
        self.count <= MAX_WINDOWS
    }

    /// Windows held.
    pub closed spec fn count_spec(&self) -> int {
        self.count as int
    }

    /// True when the window at an index holds the range with the access asked.
    pub closed spec fn holds_at(&self, i: int, addr: u64, len: u64, write: bool) -> bool {
        &&& region_holds(self.windows@[i].base, self.windows@[i].size, addr, len)
        &&& (write ==> self.windows@[i].writable)
    }

    /// True when some window holds the range.
    ///
    /// What a caller means by permitted. The trigger names holds_at, which is
    /// the only term a proof about this ever mentions.
    pub open spec fn permits(&self, addr: u64, len: u64, write: bool) -> bool {
        exists|i: int| #![trigger self.holds_at(i, addr, len, write)]
            0 <= i < self.count_spec() && self.holds_at(i, addr, len, write)
    }

    /// Empty set, permitting nothing.
    ///
    /// const, because a world table is a static.
    pub const fn new() -> (result: WindowSet)
        ensures
            result.well_formed(),
            result.count_spec() == 0,
    {
        WindowSet { windows: [Window::EMPTY; MAX_WINDOWS], count: 0 }
    }

    /// Windows held.
    pub fn len(&self) -> (result: usize)
        requires
            self.well_formed(),
        ensures
            result == self.count_spec(),
    {
        self.count
    }

    /// Adds a window.
    ///
    /// A region whose end does not fit in a u64 is refused rather than
    /// truncated. Refusing costs a machine nothing that exists; truncating
    /// produces a window whose stated extent and real extent differ, which is
    /// the one property everything here depends on.
    pub fn add(&mut self, base: u64, size: u64, writable: bool) -> (result: bool)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
    {
        if self.count >= MAX_WINDOWS {
            return false;
        }
        if size == 0 {
            return false;
        }
        // base + size must not wrap. Written as a subtraction because the
        // addition is what is being guarded against.
        if base > u64::MAX - size {
            return false;
        }

        self.windows[self.count] = Window { base, size, writable };
        self.count = self.count + 1;
        true
    }

    /// Removes every window.
    pub fn clear(&mut self)
        requires
            old(self).well_formed(),
        ensures
            final(self).well_formed(),
            final(self).count_spec() == 0,
    {
        self.count = 0;
    }

    /// True when a range lies inside a window with the access it needs.
    ///
    /// The whole point of the module. Called once per address a device is
    /// given, so it is a scan over at most four entries and no more.
    pub fn contains(&self, addr: u64, len: u64, write: bool) -> (result: bool)
        requires
            self.well_formed(),
        ensures
            result == self.permits(addr, len, write),
    {
        if len == 0 {
            proof {
                // Nothing holds a range of no bytes, so the specification is
                // false here and false is the right answer.
                assert forall|i: int| 0 <= i < self.count_spec()
                    implies !self.holds_at(i, addr, len, write)
                by {
                }
            }
            return false;
        }

        let mut i: usize = 0;

        while i < self.count
            invariant
                self.well_formed(),
                i <= self.count,
                len > 0,
                // Nothing examined so far holds it. Carried forward so that
                // falling out of the loop denies the existential.
                forall|j: int| #![trigger self.holds_at(j, addr, len, write)]
                    0 <= j < i ==> !self.holds_at(j, addr, len, write),
            decreases self.count - i,
        {
            let window = self.windows[i];

            // Ordered so that each subtraction is guarded by the test above
            // it. addr - base is safe because base <= addr; size - offset is
            // safe because offset < size.
            if window.size > 0 && window.base <= addr && (!write || window.writable) {
                let offset = addr - window.base;
                if offset < window.size {
                    let room = window.size - offset;
                    if len <= room {
                        proof {
                            // The safe form and the meaning agree:
                            //   len <= size - (addr - base)
                            // is addr + len <= base + size over the integers,
                            // and both sides are within range because the
                            // window was checked when it was added.
                            assert(addr as int + len as int
                                <= window.base as int + window.size as int);
                            assert(self.holds_at(i as int, addr, len, write));
                        }
                        return true;
                    }
                }
            }

            proof {
                // This window does not hold it, by the negation of whichever
                // test failed above.
                assert(!self.holds_at(i as int, addr, len, write));
            }

            i = i + 1;
        }

        false
    }
}

} // verus!