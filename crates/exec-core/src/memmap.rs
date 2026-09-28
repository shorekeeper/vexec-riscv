//! Range operations over the page allocator.
//!
//! The allocator is proved and works one page at a time: a bitmap, an index,
//! a bit. This turns a machine description into those calls. It is not
//! verified and does not need to be, because every statement in it is a loop
//! that ends in a call whose bounds are already established.
//!
//! Rounding is asymmetric and deliberately so. A range being released is
//! rounded inward, so a page holding part of something else is not handed out.
//! A range being reserved is rounded outward, so a page holding any part of
//! something reserved is kept.

use crate::platform::MemoryRegion;
use crate::pmm::{Pmm, MAX_PAGES, PAGE_SIZE};

/// Page number of an address within the window, or None when outside it.
fn page_of(pmm: &Pmm, address: u64) -> Option<usize> {
    let base = pmm.window_base();
    if address < base {
        return None;
    }
    let index = (address - base) / PAGE_SIZE;
    if index >= pmm.total_pages() as u64 {
        return None;
    }
    Some(index as usize)
}

/// Sets the window from a memory map and frees every region in it.
///
/// The window starts at the lowest region base rounded down to a page. Memory
/// above what the bitmap covers is described but never handed out, which is
/// visible as a difference between the described total and `total_pages`.
pub fn init(pmm: &mut Pmm, regions: &[MemoryRegion], count: usize) {
    let mut lowest = u64::MAX;
    let mut highest = 0u64;

    let mut i = 0;
    while i < count && i < regions.len() {
        let region = regions[i];
        if region.size != 0 {
            if region.base < lowest {
                lowest = region.base;
            }
            if region.end() > highest {
                highest = region.end();
            }
        }
        i += 1;
    }
    if lowest == u64::MAX {
        return;
    }

    let base = lowest & !(PAGE_SIZE - 1);
    let span = highest.saturating_sub(base);
    let mut pages = (span / PAGE_SIZE) as usize;
    if pages > MAX_PAGES {
        pages = MAX_PAGES;
    }

    // The allocator requires the top of the window to fit in a u64. Both
    // target machines put memory at 0x80000000 and clear this by a wide
    // margin; a machine that does not is one the allocator cannot describe.
    if base > u64::MAX - (MAX_PAGES as u64) * PAGE_SIZE {
        return;
    }

    pmm.set_window(base, pages);

    // Every page starts taken, so freeing the regions is what makes memory
    // available and leaves holes between them unavailable.
    let mut i = 0;
    while i < count && i < regions.len() {
        release(pmm, regions[i].base, regions[i].size);
        i += 1;
    }
}

/// Frees every page wholly inside a range.
///
/// Rounded inward: a partly covered page at either end stays taken, because
/// handing out a page that is partly something else produces a fault much
/// later and somewhere else.
pub fn release(pmm: &mut Pmm, base: u64, size: u64) {
    if size == 0 {
        return;
    }
    let start = (base + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let end = (base + size) & !(PAGE_SIZE - 1);

    let mut address = start;
    while address < end {
        if let Some(page) = page_of(pmm, address) {
            pmm.release_page(page);
        }
        address += PAGE_SIZE;
    }
}

/// Marks every page touching a range as taken.
///
/// Rounded outward, the opposite of release.
pub fn reserve(pmm: &mut Pmm, base: u64, size: u64) {
    if size == 0 {
        return;
    }
    let start = base & !(PAGE_SIZE - 1);
    let end = (base + size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let mut address = start;
    while address < end {
        if let Some(page) = page_of(pmm, address) {
            pmm.reserve_page(page);
        }
        address += PAGE_SIZE;
    }
}