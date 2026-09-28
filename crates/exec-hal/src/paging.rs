//! Translation control.
//!
//! Physical memory access for the table walker, and the two instructions that
//! make a set of tables live. Everything above this is arithmetic in exec-core.
//!
//! Physical access is direct because translation is off while the tables are
//! being built, and identity mapped afterwards. A kernel mapped at a different
//! virtual address would need an offset here and nothing else.

use core::arch::asm;

use exec_core::paging::PhysMem;
use exec_core::pmm::PAGE_SIZE;

use crate::mmio;

/// Reads and writes physical memory directly.
///
/// Zero sized. Correct while the kernel is identity mapped, which the boot
/// mapping guarantees for every address it hands out.
pub struct Direct;

impl PhysMem for Direct {
    fn read_u64(&self, pa: u64) -> u64 {
        // Safety: the address comes from the allocator or from a table entry
        // the allocator produced, so it names a page of usable memory.
        unsafe { (pa as *const u64).read_volatile() }
    }

    fn write_u64(&mut self, pa: u64, value: u64) {
        // Safety: as above.
        unsafe { (pa as *mut u64).write_volatile(value) }
    }

    fn zero_page(&mut self, pa: u64) {
        let mut offset = 0;
        while offset < PAGE_SIZE {
            // Safety: as above, and the loop stays inside one page.
            unsafe { ((pa + offset) as *mut u64).write_volatile(0) };
            offset += 8;
        }
    }
}

/// Installs a set of tables and flushes the old translations.
///
/// The fence after the write is what makes the change take effect: without it
/// the hart may keep using cached translations, and which ones is not defined.
///
/// # Safety
///
/// The tables must map every address the code executing across this call
/// touches, at minimum its own instructions, its stack, and the trap vector.
/// Getting that wrong faults immediately with no working handler.
pub unsafe fn activate(satp: u64) {
    asm!(
        "csrw satp, {0}",
        "sfence.vma zero, zero",
        in(reg) satp,
        options(nostack)
    );
}

/// Flushes every translation.
///
/// Needed after changing a table that is already live.
pub fn flush_all() {
    unsafe {
        asm!("sfence.vma zero, zero", options(nostack));
    }
}

/// Flushes the translation of one address.
pub fn flush_address(va: u64) {
    unsafe {
        asm!("sfence.vma {0}, zero", in(reg) va, options(nostack));
    }
}

/// True when translation is enabled.
pub fn is_active() -> bool {
    // Mode zero in the top four bits means bare, with no translation.
    (crate::csr::satp_read() >> 60) != 0
}

/// Reads a byte through the current mapping.
///
/// Used by the self check to confirm that a mapping resolves to the memory it
/// is supposed to before anything depends on it.
///
/// # Safety
///
/// The address must be mapped readable.
pub unsafe fn probe_read(address: usize) -> u8 {
    mmio::read8(address)
}