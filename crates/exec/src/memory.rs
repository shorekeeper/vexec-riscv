//! Boot memory map.
//!
//! Builds the page tables the kernel runs under and installs them. Runs once,
//! after the device tree is read and before interrupts are enabled.
//!
//! The map is an identity map: every virtual address equals its physical
//! address. That removes an entire class of obligation from anything built on
//! top, because a pointer means the same thing on both sides of the table
//! walk, and it is why the kernel can hand a physical address to the firmware
//! without translating it.
//!
//! Permissions are exact, which is the point of doing this at all:
//!
//! ```text
//!   text      read execute
//!   rodata    read
//!   data bss  read write
//!   free ram  read write
//!   devices   read write
//!   firmware  not mapped
//! ```
//!
//! The firmware region below the kernel is left out. Nothing in S mode has any
//! business reaching it, and an unmapped range is a stronger statement than a
//! read only one.
//!
//! Every mapping goes through the security layer rather than straight to the
//! page tables. This is the only place in the system that maps anything, which
//! is what makes the policy a property of the code rather than of everybody
//! remembering it.

use exec_abi::esl::WorldId;
use exec_abi::EslMode;
use exec_core::esl::Esl;
use exec_core::memmap;
use exec_core::paging::{self, PageSize, Perm, PhysMem, Translation};
use exec_core::platform::Platform;
use exec_core::pmm::{Pmm, PAGE_SIZE};
use exec_hal::kprintln;
use exec_hal::paging::Direct;

extern "C" {
    static __image_start: u8;
    static __image_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __bss_end: u8;
}

/// Address of a linker symbol.
///
/// Read as an address and never dereferenced, which is what makes taking the
/// reference sound: the object it appears to name has no contents.
fn symbol(s: &'static u8) -> u64 {
    s as *const u8 as u64
}

/// Where the kernel image sits.
pub struct ImageLayout {
    /// First byte of the image.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    /// Executable range.
    pub text: (u64, u64),
    /// Read only range.
    pub rodata: (u64, u64),
    /// Writable range, data and bss together.
    pub data: (u64, u64),
}

/// Reads the layout from the linker symbols.
pub fn image_layout() -> ImageLayout {
    unsafe {
        ImageLayout {
            start: symbol(&__image_start),
            end: symbol(&__image_end),
            text: (symbol(&__text_start), symbol(&__text_end)),
            rodata: (symbol(&__rodata_start), symbol(&__rodata_end)),
            data: (symbol(&__data_start), symbol(&__bss_end)),
        }
    }
}

/// Why the map could not be built.
#[derive(Clone, Copy, Debug)]
pub enum MemoryError {
    /// The allocator has no memory to build tables from.
    NoMemory,
    /// The security layer refused a mapping.
    Esl(exec_abi::esl::EslError),
    /// A mapping was made and does not resolve to what it should.
    ///
    /// The tables were not installed. This is the case the self check exists
    /// for: after the tables are live a wrong mapping faults with no console
    /// and no handler, so it has to be caught while the old translation is
    /// still in effect.
    SelfCheck(&'static str),
    /// The hart did not accept the value written to satp.
    ///
    /// satp is write any read legal: a mode the hart does not implement is
    /// discarded and the register keeps what it had, so translation stays off
    /// with no fault to notice. The tables are built and the map is identity,
    /// so the machine runs unprotected rather than stopping.
    SatpRejected,
}

/// What the map turned out to be.
#[derive(Clone, Copy, Debug)]
pub struct MapSummary {
    /// Physical address of the root table.
    pub root: u64,
    /// Value written to satp.
    pub satp: u64,
    /// Leaf entries created.
    pub pages: usize,
    /// Pages spent on tables.
    pub tables: usize,
    /// Free pages after the map is built.
    pub free_pages: usize,
    /// Total pages the allocator covers.
    pub total_pages: usize,
    /// Memory described by the tree and outside the allocator window.
    pub uncovered_mib: u64,
}

/// Prepares the allocator from the memory the tree described.
///
/// Everything below the end of the kernel image is reserved: it holds the
/// firmware and the kernel, and neither can be handed out. The device tree
/// blob is reserved separately because the firmware places it wherever it
/// likes, usually near the top of memory.
pub fn init_allocator(pmm: &mut Pmm, platform: &Platform, dtb: u64, dtb_size: u64) {
    memmap::init(pmm, &platform.memory, platform.memory_count);

    let layout = image_layout();

    // From the base of every memory region up to the end of the image. On both
    // targets there is one region and the firmware sits at its start.
    let mut i = 0;
    while i < platform.memory_count {
        let region = platform.memory[i];
        if region.base < layout.end {
            let size = layout.end - region.base;
            memmap::reserve(pmm, region.base, size);
        }
        i += 1;
    }

    if dtb != 0 && dtb_size != 0 {
        memmap::reserve(pmm, dtb, dtb_size);
    }

    let mut i = 0;
    while i < platform.reserved_count {
        let region = platform.reserved[i];
        memmap::reserve(pmm, region.base, region.size);
        i += 1;
    }
}

/// Maps a range through the security layer, one page at a time.
///
/// A free function rather than a closure: the layer, the allocator and the
/// page count are all borrowed mutably, and threading three of those through a
/// closure is what borrow checking is for.
///
/// The largest page that fits is used, which is what keeps the leaf count down
/// on a machine with a hundred megabytes of free memory. A large page is
/// chosen only when both addresses are aligned to it and the whole of it lies
/// inside the range, so nothing outside the range is mapped by accident.
#[allow(clippy::too_many_arguments)]
fn map_range(
    esl: &mut Esl,
    pmm: &mut Pmm,
    root: u64,
    va: u64,
    len: u64,
    perm: Perm,
    pages: &mut usize,
) -> Result<(), MemoryError> {
    if len == 0 {
        return Ok(());
    }

    // Global on every kernel mapping. It is the same in every address space,
    // so a switch does not have to flush it.
    const GLOBAL: bool = true;

    let mega = PageSize::Mega.bytes();
    let start = va & !(PAGE_SIZE - 1);
    let end = (va + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let mut current = start;
    while current < end {
        let remaining = end - current;
        let size = if current % mega == 0 && remaining >= mega {
            PageSize::Mega
        } else {
            PageSize::Small
        };

        esl.map(
            WorldId::KERNEL,
            root,
            current,
            current,
            size,
            perm,
            GLOBAL,
            pmm,
            &mut Direct,
        )
        .map_err(MemoryError::Esl)?;

        current += size.bytes();
        *pages += 1;
    }

    Ok(())
}

/// Builds the kernel map and installs it.
///
/// Returns without installing anything if any step fails, so a machine this
/// cannot map keeps running with translation off rather than stopping.
pub fn build_and_activate(
    esl: &mut Esl,
    pmm: &mut Pmm,
    platform: &Platform,
) -> Result<MapSummary, MemoryError> {
    let mem = Direct;
    let layout = image_layout();

    // Text is sealed before it is mapped, because the policy grants execute
    // only to a region that has been measured. The order is the policy: a
    // region that is executable and was never measured is the state this is
    // arranged to make unreachable.
    if esl.mode() != EslMode::Off {
        esl.seal(
            WorldId::KERNEL,
            layout.text.0,
            layout.text.1 - layout.text.0,
            &mem,
        )
        .map_err(MemoryError::Esl)?;
    }

    let root = {
        let mut direct = Direct;
        paging::create_root(pmm, &mut direct).ok_or(MemoryError::NoMemory)?
    };
    let mut pages = 0usize;

    // Kernel image, one range per permission. The linker aligns each to a
    // page, so no range shares a page with another and the permissions do not
    // have to be widened to fit.
    map_range(
        esl,
        pmm,
        root,
        layout.text.0,
        layout.text.1 - layout.text.0,
        Perm::ReadExecute,
        &mut pages,
    )?;
    map_range(
        esl,
        pmm,
        root,
        layout.rodata.0,
        layout.rodata.1 - layout.rodata.0,
        Perm::ReadOnly,
        &mut pages,
    )?;
    map_range(
        esl,
        pmm,
        root,
        layout.data.0,
        layout.data.1 - layout.data.0,
        Perm::ReadWrite,
        &mut pages,
    )?;

    // Memory above the image. Writable and not executable: it becomes stacks,
    // page tables and buffers, and nothing that runs.
    let mut i = 0;
    while i < platform.memory_count {
        let region = platform.memory[i];
        let start = if region.base < layout.end {
            layout.end
        } else {
            region.base
        };
        let end = region.end();
        if start < end {
            map_range(esl, pmm, root, start, end - start, Perm::ReadWrite, &mut pages)?;
        }
        i += 1;
    }

    // Devices. Writable, never executable.
    if platform.plic.is_present() {
        map_range(
            esl,
            pmm,
            root,
            platform.plic.base,
            platform.plic.size,
            Perm::ReadWrite,
            &mut pages,
        )?;
    }
    if platform.uart.device.is_present() {
        let mmio = platform.uart.device.mmio;
        map_range(esl, pmm, root, mmio.base, mmio.size, Perm::ReadWrite, &mut pages)?;
    }
    let mut i = 0;
    while i < platform.virtio_count {
        let mmio = platform.virtio[i].mmio;
        map_range(esl, pmm, root, mmio.base, mmio.size, Perm::ReadWrite, &mut pages)?;
        i += 1;
    }

    // Everything that has to work across the instruction that installs the
    // tables, checked while the old translation is still in effect.
    self_check(root, &layout, platform, &mem)?;

    let satp = paging::satp_value(root);
    let described = platform.memory_total();
    let covered = pmm.total_pages() as u64 * PAGE_SIZE;
    let summary = MapSummary {
        root,
        satp,
        pages,
        tables: pmm.table_pages(),
        free_pages: pmm.free_pages(),
        total_pages: pmm.total_pages(),
        uncovered_mib: described.saturating_sub(covered) / (1024 * 1024),
    };

    // Safety: the check above confirmed that the code executing here, its
    // stack, the trap vector and the console all resolve to themselves.
    unsafe { exec_hal::paging::activate(satp) };

    // Read back rather than assume. A hart that implements a wider scheme and
    // not this one discards the write, and the difference between translation
    // on and translation off is invisible under an identity map.
    if exec_hal::csr::satp_read() as u64 != satp {
        return Err(MemoryError::SatpRejected);
    }

    Ok(summary)
}

/// Confirms the map before it is installed.
///
/// Every address checked is one that has to work for the kernel to survive the
/// instruction after the tables go live. A failure here is recoverable; the
/// same failure a moment later is not.
fn self_check(
    root: u64,
    layout: &ImageLayout,
    platform: &Platform,
    mem: &impl PhysMem,
) -> Result<(), MemoryError> {
    // The code doing the installing, and the code returning from it.
    expect(root, layout.text.0, Perm::ReadExecute, mem, "text start")?;
    expect(root, layout.text.1 - 4, Perm::ReadExecute, mem, "text end")?;

    // Constants, including the strings any later diagnostic prints.
    if layout.rodata.1 > layout.rodata.0 {
        expect(root, layout.rodata.0, Perm::ReadOnly, mem, "rodata")?;
    }

    // The executive, the stack, and everything else that is written.
    expect(root, layout.data.0, Perm::ReadWrite, mem, "data")?;
    expect(root, layout.data.1 - 8, Perm::ReadWrite, mem, "bss end")?;

    // The stack in use right now. Taken as the address of a local, which is
    // the only way to name it without a symbol for it.
    let local = 0u64;
    let sp = &local as *const u64 as u64;
    expect(root, sp, Perm::ReadWrite, mem, "current stack")?;

    // The console, or the failure has nowhere to be reported.
    if platform.uart.device.is_present() {
        expect(
            root,
            platform.uart.device.mmio.base,
            Perm::ReadWrite,
            mem,
            "uart",
        )?;
    }

    // The interrupt controller, which the first interrupt after this touches.
    if platform.plic.is_present() {
        expect(root, platform.plic.base, Perm::ReadWrite, mem, "plic")?;
    }

    Ok(())
}

/// Checks that one address resolves to itself with the permission expected.
fn expect(
    root: u64,
    va: u64,
    perm: Perm,
    mem: &impl PhysMem,
    what: &'static str,
) -> Result<(), MemoryError> {
    let t: Translation = match paging::translate(root, va, mem) {
        Some(t) => t,
        None => return Err(MemoryError::SelfCheck(what)),
    };
    // Identity, so anything else means the arithmetic is wrong rather than the
    // permission.
    if t.pa != va {
        return Err(MemoryError::SelfCheck(what));
    }
    if perm.is_executable() && !t.is_executable() {
        return Err(MemoryError::SelfCheck(what));
    }
    if perm.is_writable() && !t.is_writable() {
        return Err(MemoryError::SelfCheck(what));
    }
    // Invariant I1, checked rather than assumed. The type makes it impossible
    // to ask for, the layer refuses it, and this confirms nothing produced it
    // by accident. Three independent reasons for one property, which is the
    // right number for the property everything else rests on.
    if t.is_writable() && t.is_executable() {
        return Err(MemoryError::SelfCheck("write and execute together"));
    }
    Ok(())
}

/// Prints the map.
pub fn report(summary: &MapSummary) {
    kprintln!("memory");
    kprintln!(
        "  pages     {} total, {} free ({} MiB)",
        summary.total_pages,
        summary.free_pages,
        (summary.free_pages as u64 * PAGE_SIZE) / (1024 * 1024)
    );
    // Memory the tree described that the bitmap does not cover. Silent until
    // it is nonzero, and worth saying loudly when it is: the machine has more
    // memory than the kernel can hand out.
    if summary.uncovered_mib != 0 {
        kprintln!(
            "  uncovered {} MiB beyond the allocator window",
            summary.uncovered_mib
        );
    }
    kprintln!(
        "  mappings  {} leaves, {} table pages",
        summary.pages,
        summary.tables
    );
    kprintln!("  root      {:#012x}", summary.root);
    kprintln!("  satp      {:#018x}  sv39", summary.satp);

    let layout = image_layout();
    kprintln!(
        "  text      {:#012x}..{:#012x}  r-x",
        layout.text.0,
        layout.text.1
    );
    kprintln!(
        "  rodata    {:#012x}..{:#012x}  r--",
        layout.rodata.0,
        layout.rodata.1
    );
    kprintln!(
        "  data      {:#012x}..{:#012x}  rw-",
        layout.data.0,
        layout.data.1
    );
    kprintln!(
        "  firmware  {:#012x}..{:#012x}  unmapped",
        0x8000_0000u64,
        layout.start
    );
}