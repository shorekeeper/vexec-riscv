// GENERATED. Do not edit.
//
// Produced from spec/window.rs by scripts/spec-strip.ps1, which only deletes:
// ghost items, specification clauses, proof blocks and asserts. Every
// executable statement below is the one that was verified, character for
// character.
//
// Regenerate:  .\scripts\spec-strip.ps1 -In spec/window.rs -Out <path>
// Check:       .\scripts\spec-strip.ps1 -In spec/window.rs -Out <path> -Check
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

/// A set of windows.
pub struct WindowSet {
    windows: [Window; MAX_WINDOWS],
    count: usize,
}

impl WindowSet {

    /// Empty set, permitting nothing.
    ///
    /// const, because a world table is a static.
    pub const fn new() -> WindowSet
    {
        WindowSet { windows: [Window::EMPTY; MAX_WINDOWS], count: 0 }
    }

    /// Windows held.
    pub fn len(&self) -> usize
    {
        self.count
    }

    /// Adds a window.
    ///
    /// A region whose end does not fit in a u64 is refused rather than
    /// truncated. Refusing costs a machine nothing that exists; truncating
    /// produces a window whose stated extent and real extent differ, which is
    /// the one property everything here depends on.
    pub fn add(&mut self, base: u64, size: u64, writable: bool) -> bool
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
    {
        self.count = 0;
    }

    /// True when a range lies inside a window with the access it needs.
    ///
    /// The whole point of the module. Called once per address a device is
    /// given, so it is a scan over at most four entries and no more.
    pub fn contains(&self, addr: u64, len: u64, write: bool) -> bool
    {
        if len == 0 {
            return false;
        }

        let mut i: usize = 0;

        while i < self.count
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
                        return true;
                    }
                }
            }

            i = i + 1;
        }

        false
    }
}
