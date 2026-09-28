// GENERATED. Do not edit.
//
// Produced from spec/pmm.rs by scripts/spec-strip.ps1, which only deletes:
// ghost items, specification clauses, proof blocks and asserts. Every
// executable statement below is the one that was verified, character for
// character.
//
// Regenerate:  .\scripts\spec-strip.ps1 -In spec/pmm.rs -Out <path>
// Check:       .\scripts\spec-strip.ps1 -In spec/pmm.rs -Out <path> -Check
//! Physical page allocator, with proof.
//!
//! Verified standalone: `verus spec/pmm.rs --crate-type=lib`.
//!
//! What is proved:
//!
//!   1. No panic, no overflow, no index outside the bitmap on any path.
//!   2. Every address `alloc` returns is page aligned and inside the window.
//!   3. The page `alloc` returns had a clear bit before the call and has a set
//!      bit after it.
//!
//! Point 3 is the one that matters. Together with the fact that `alloc` only
//! ever returns a page whose bit is clear, it gives the property the allocator
//! exists for: the same page is not handed out twice without an intervening
//! free. Stated as `lemma_no_double_alloc` below.
//!
//! What is deliberately not proved: that `free_count` equals the number of
//! clear bits. That is a liveness property, it needs a recursive population
//! count over the whole bitmap and a bit level lemma for every mutation,
//! and getting it wrong makes the allocator report the wrong number rather
//! than hand out the wrong page. The counters are treated as advisory and the
//! scan is bounded independently of them.
//!
//! The first clear bit is found by an explicit loop rather than by
//! `trailing_zeros`. The intrinsic has no cheap specification and reasoning
//! about it needs the bit vector solver over a value the loop already knows;
//! sixty four iterations on an interpreted machine cost less than the
//! confidence does.

/// Page size in bytes.
pub const PAGE_SIZE: u64 = 4096;
/// Bits to shift an address to get a page number.
pub const PAGE_SHIFT: u64 = 12;

/// Words in the bitmap. One bit per page, sixty four pages per word.
///
/// Two hundred and fifty six words covers 64 MiB and costs two kilobytes of
/// bss. Sized for the smaller of the two target machines rather than the
/// larger: a computer in the mod is built from memory modules and may have
/// eight megabytes, where a thirty two kilobyte bitmap is half a percent of
/// everything there is.
///
/// Memory above the window is described and reported but never handed out,
/// which is visible as a difference between the described total and the page
/// count. Raising this is one constant and a reverify.
pub const WORDS: usize = 256;
/// Pages the bitmap covers.
pub const MAX_PAGES: usize = WORDS * 64;

/// Bitmap allocator over a window of physical memory.
///
/// A set bit means the page is in use or does not exist. The two are not
/// distinguished: both mean the same thing to a caller asking for a page.
pub struct Pmm {
    bits: [u64; WORDS],
    base: u64,
    page_count: usize,
    free_count: usize,
    cursor: usize,
    allocated: usize,
    tables: usize,
}

impl Pmm {

    /// Empty allocator with nothing free.
    ///
    /// Every bit is set, so an allocation before init fails rather than
    /// returning a page that does not exist.
    // const, for the same reason as the queue: the allocator lives inside the
    // executive, which is a static.
    pub const fn new() -> Pmm
    {
        Pmm {
            bits: [0xffff_ffff_ffff_ffffu64; WORDS],
            base: 0,
            page_count: 0,
            free_count: 0,
            cursor: 0,
            allocated: 0,
            tables: 0,
        }
    }

    /// Sets the window.
    ///
    /// Called once, with the lowest region base rounded down to a page and the
    /// span rounded down to whole pages. Everything stays in use until it is
    /// released.
    pub fn set_window(&mut self, base: u64, pages: usize)
    {
        self.base = base;
        self.page_count = pages;
        self.free_count = 0;
    }

    /// Marks one page free.
    ///
    /// Out of range is ignored rather than reported: the caller is walking a
    /// region that may extend past the window, and there is nothing useful to
    /// do with the failure.
    pub fn release_page(&mut self, page: usize)
    {
        if page >= self.page_count {
            return;
        }
        let word = page / 64;
        let bit = (page % 64) as u64;

        let current = self.bits[word];
        let mask = 1u64 << bit;
        if (current & mask) != 0 {
            self.bits[word] = current & !mask;
            if self.free_count < self.page_count {
                self.free_count = self.free_count + 1;
            }
        }
    }

    /// Marks one page in use.
    pub fn reserve_page(&mut self, page: usize)
    {
        if page >= self.page_count {
            return;
        }
        let word = page / 64;
        let bit = (page % 64) as u64;

        let current = self.bits[word];
        let mask = 1u64 << bit;
        if (current & mask) == 0 {
            self.bits[word] = current | mask;
            if self.free_count > 0 {
                self.free_count = self.free_count - 1;
            }
        }
    }

    /// Index of the first clear bit in a word, or 64 when there is none.
    ///
    /// An explicit scan. The loop invariant is what carries the result: every
    /// bit below the one returned is set, and the one returned is clear.
    fn first_clear(word: u64) -> u64
    {
        let mut bit: u64 = 0;
        while bit < 64
        {
            if (word & (1u64 << bit)) == 0 {
                return bit;
            }
            bit = bit + 1;
        }
        64
    }

    /// Takes a free page and returns its address.
    ///
    /// The scan visits every word at most once, so the call is bounded by the
    /// bitmap size regardless of how full it is. Next fit rather than first
    /// fit: a first fit scan restarts at zero every time and costs the whole
    /// bitmap once the low pages are taken, which they are within the first
    /// few hundred allocations.
    pub fn alloc(&mut self) -> Option<u64>
    {
        let mut examined: usize = 0;

        while examined <= WORDS
        {
            let word = self.cursor;
            let value = self.bits[word];

            if value != 0xffff_ffff_ffff_ffffu64 {
                let bit = Self::first_clear(value);
                if bit < 64 {
                    let page = word * 64 + bit as usize;
                    if page < self.page_count {
                        self.bits[word] = value | (1u64 << bit);
                        if self.free_count > 0 {
                            self.free_count = self.free_count - 1;
                        }
                        if self.allocated < usize::MAX {
                            self.allocated = self.allocated + 1;
                        }

                        // page is below page_count, which is below MAX_PAGES,
                        // so the product is below MAX_PAGES times PAGE_SIZE
                        // and cannot overflow a u64.
                        let address = self.base + (page as u64) * PAGE_SIZE;
                        return Some(address);
                    }
                }
            }

            self.cursor = self.cursor + 1;
            if self.cursor >= WORDS {
                self.cursor = 0;
            }
            examined = examined + 1;
        }

        None
    }

    /// Takes a free page and counts it as page table overhead.
    ///
    /// Counted apart because it is the cost of translation itself, and knowing
    /// it is what makes a decision about page size evidence based.
    pub fn alloc_table(&mut self) -> Option<u64>
    {
        let page = self.alloc();
        // A match rather than `is Some`: testing a variant with `is` is spec
        // mode only, and this runs.
        match page {
            Some(_) => {
                if self.tables < usize::MAX {
                    self.tables = self.tables + 1;
                }
            }
            None => {}
        }
        page
    }

    /// Returns a page.
    ///
    /// An address outside the window or not page aligned is ignored: there is
    /// no caller that could act on the failure, and freeing something never
    /// allocated is a defect the counters make visible.
    pub fn free(&mut self, address: u64)
    {
        if address % PAGE_SIZE != 0 || address < self.base {
            return;
        }
        let offset = address - self.base;
        let page = offset / PAGE_SIZE;
        if page >= self.page_count as u64 {
            return;
        }
        self.release_page(page as usize);
        if self.allocated > 0 {
            self.allocated = self.allocated - 1;
        }
    }

    /// Lowest address the window covers.
    ///
    /// Needed by the range adapter, which converts addresses to page numbers
    /// outside this module.
    pub fn window_base(&self) -> u64
    {
        self.base
    }

    /// Pages the window covers.
    pub fn total_pages(&self) -> usize
    {
        self.page_count
    }

    /// Pages the allocator believes are available.
    pub fn free_pages(&self) -> usize {
        self.free_count
    }

    /// Pages handed out and not returned.
    pub fn allocated_pages(&self) -> usize {
        self.allocated
    }

    /// Pages spent on page tables.
    pub fn table_pages(&self) -> usize {
        self.tables
    }
}
