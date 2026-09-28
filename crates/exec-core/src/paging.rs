//! Sv39 address translation.
//!
//! Three levels of five hundred and twelve entries, each table one page.
//! A virtual address is split into three nine bit indices and a twelve bit
//! offset; a leaf may sit at any level, giving pages of 4 KiB, 2 MiB or 1 GiB.
//!
//! ```text
//!   38..30  index into the root table
//!   29..21  index into the level one table
//!   20..12  index into the level zero table
//!   11..0   offset within the page
//! ```
//!
//! All arithmetic here is in this crate and free of unsafe. Reaching physical
//! memory is a trait, implemented by the HAL, so the logic that decides what
//! is mapped where can be verified without a model of a memory access.
//!
//! Invariant I1 from the architecture, that no mapping grants write and
//! execute together, is enforced by the shape of `Perm` rather than by a
//! check. There is no value of that type that carries both, so a mapping that
//! violates it cannot be constructed, let alone installed.

use crate::pmm::{Pmm, PAGE_SHIFT, PAGE_SIZE};

/// Entries in one table.
pub const ENTRIES: usize = 512;

/// Bits of virtual address each level covers.
///
/// u64 to match PAGE_SHIFT, which comes from the generated allocator. The two
/// are added together to reach a level, so they have to agree.
const LEVEL_SHIFT: u64 = 9;
/// Deepest level index. Level two is the root.
const ROOT_LEVEL: usize = 2;

/// Individual bits of a table entry.
pub mod pte {
    /// The entry is valid. Clear means the rest is ignored.
    pub const V: u64 = 1 << 0;
    /// Readable.
    pub const R: u64 = 1 << 1;
    /// Writable.
    pub const W: u64 = 1 << 2;
    /// Executable.
    pub const X: u64 = 1 << 3;
    /// Reachable from user mode.
    pub const U: u64 = 1 << 4;
    /// Global. Not flushed by an address space switch.
    pub const G: u64 = 1 << 5;
    /// Accessed.
    pub const A: u64 = 1 << 6;
    /// Dirty.
    pub const D: u64 = 1 << 7;
    /// Bits that make an entry a leaf. An entry with none of them is a
    /// pointer to the next table.
    pub const LEAF: u64 = R | W | X;
}

/// Size of one mapping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageSize {
    /// 4 KiB, a leaf at level zero.
    Small,
    /// 2 MiB, a leaf at level one.
    Mega,
}

impl PageSize {
    /// Length in bytes.
    pub const fn bytes(self) -> u64 {
        match self {
            PageSize::Small => PAGE_SIZE,
            PageSize::Mega => PAGE_SIZE << LEVEL_SHIFT,
        }
    }

    /// Level a leaf of this size sits at.
    pub const fn level(self) -> usize {
        match self {
            PageSize::Small => 0,
            PageSize::Mega => 1,
        }
    }
}

/// What a mapping permits.
///
/// Three cases, and the one that would grant write and execute together does
/// not exist. That is invariant I1 expressed as a type: not a rule the code
/// follows, a state the code cannot represent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Perm {
    /// Read only. Constants and anything measured.
    ReadOnly,
    /// Read and write. Data, stacks, device registers.
    ReadWrite,
    /// Read and execute. Code.
    ReadExecute,
}

impl Perm {
    /// Entry bits for this permission.
    pub const fn bits(self) -> u64 {
        match self {
            Perm::ReadOnly => pte::R,
            Perm::ReadWrite => pte::R | pte::W,
            Perm::ReadExecute => pte::R | pte::X,
        }
    }

    /// True when the permission grants execute.
    pub const fn is_executable(self) -> bool {
        matches!(self, Perm::ReadExecute)
    }

    /// True when the permission grants write.
    pub const fn is_writable(self) -> bool {
        matches!(self, Perm::ReadWrite)
    }
}

/// Why a mapping could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
    /// An address is not aligned to the page size asked for.
    Misaligned,
    /// No page was available for an intermediate table.
    OutOfMemory,
    /// Something is already mapped there.
    AlreadyMapped,
    /// The virtual address does not fit in thirty nine bits.
    OutOfRange,
    /// A leaf was found where a table was expected, which means a large page
    /// covers the range and would have to be split.
    Blocked,
}

/// Access to physical memory, for reading and writing table entries.
///
/// Implemented by the HAL. Everything above it is arithmetic.
pub trait PhysMem {
    /// Reads eight bytes at a physical address.
    fn read_u64(&self, pa: u64) -> u64;
    /// Writes eight bytes at a physical address.
    fn write_u64(&mut self, pa: u64, value: u64);
    /// Fills a page with zeroes.
    fn zero_page(&mut self, pa: u64);
}

/// Index into the table at a level.
const fn index_at(va: u64, level: usize) -> usize {
    let shift = PAGE_SHIFT + LEVEL_SHIFT * level as u64;
    ((va >> shift) & 0x1ff) as usize
}

/// Address of an entry within a table.
const fn entry_address(table: u64, index: usize) -> u64 {
    table + (index as u64) * 8
}

/// Builds a leaf entry.
///
/// Accessed and dirty are set at creation. Hardware that updates them itself
/// may do so, and hardware that faults instead never has to, which removes a
/// fault path that would otherwise appear only under load.
const fn leaf_entry(pa: u64, perm: Perm, global: bool) -> u64 {
    let ppn = pa >> PAGE_SHIFT;
    let mut bits = (ppn << 10) | perm.bits() | pte::V | pte::A | pte::D;
    if global {
        bits |= pte::G;
    }
    bits
}

/// Builds an entry pointing at the next table.
const fn branch_entry(pa: u64) -> u64 {
    ((pa >> PAGE_SHIFT) << 10) | pte::V
}

/// Physical address a branch entry points at.
const fn branch_target(entry: u64) -> u64 {
    ((entry >> 10) & 0xfff_ffff_ffff) << PAGE_SHIFT
}

/// True when an entry is a leaf rather than a pointer.
const fn is_leaf(entry: u64) -> bool {
    entry & pte::LEAF != 0
}

/// Creates a root table.
pub fn create_root(pmm: &mut Pmm, mem: &mut impl PhysMem) -> Option<u64> {
    let page = pmm.alloc_table()?;
    mem.zero_page(page);
    Some(page)
}

/// Maps one page.
///
/// The virtual and physical addresses must both be aligned to the page size.
/// Intermediate tables are created as needed and are never leaves, so a table
/// found where a leaf is expected, or the reverse, is reported rather than
/// worked around: both mean the caller mapped overlapping ranges.
pub fn map(
    root: u64,
    va: u64,
    pa: u64,
    size: PageSize,
    perm: Perm,
    global: bool,
    pmm: &mut Pmm,
    mem: &mut impl PhysMem,
) -> Result<(), MapError> {
    let length = size.bytes();
    if va % length != 0 || pa % length != 0 {
        return Err(MapError::Misaligned);
    }
    // Sv39 covers thirty nine bits. Higher addresses need the upper bits to be
    // a sign extension of bit thirty eight, which nothing here produces.
    if va >= (1u64 << 39) {
        return Err(MapError::OutOfRange);
    }

    let target = size.level();
    let mut table = root;
    let mut level = ROOT_LEVEL;

    // Descend to the level the leaf goes at, creating tables on the way.
    while level > target {
        let address = entry_address(table, index_at(va, level));
        let entry = mem.read_u64(address);

        if entry & pte::V == 0 {
            let next = pmm.alloc_table().ok_or(MapError::OutOfMemory)?;
            mem.zero_page(next);
            mem.write_u64(address, branch_entry(next));
            table = next;
        } else if is_leaf(entry) {
            // A larger page already covers this range. Splitting it is
            // possible and is not done: every caller here maps disjoint
            // ranges, so this means the caller is wrong.
            return Err(MapError::Blocked);
        } else {
            table = branch_target(entry);
        }
        level -= 1;
    }

    let address = entry_address(table, index_at(va, target));
    if mem.read_u64(address) & pte::V != 0 {
        return Err(MapError::AlreadyMapped);
    }
    mem.write_u64(address, leaf_entry(pa, perm, global));
    Ok(())
}

/// Maps a range, choosing the largest page that fits.
///
/// The range is rounded outward to whole pages. A large page is used only when
/// both addresses are aligned to it and the whole of it is inside the range,
/// so nothing outside the range is ever mapped by accident.
pub fn map_range(
    root: u64,
    va: u64,
    pa: u64,
    length: u64,
    perm: Perm,
    global: bool,
    pmm: &mut Pmm,
    mem: &mut impl PhysMem,
) -> Result<usize, MapError> {
    if length == 0 {
        return Ok(0);
    }

    let mega = PageSize::Mega.bytes();
    let start_va = va & !(PAGE_SIZE - 1);
    let start_pa = pa & !(PAGE_SIZE - 1);
    let end_va = (va + length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let mut offset = 0u64;
    let mut pages = 0usize;
    let span = end_va - start_va;

    while offset < span {
        let current_va = start_va + offset;
        let current_pa = start_pa + offset;
        let remaining = span - offset;

        let size = if current_va % mega == 0 && current_pa % mega == 0 && remaining >= mega {
            PageSize::Mega
        } else {
            PageSize::Small
        };

        map(root, current_va, current_pa, size, perm, global, pmm, mem)?;
        offset += size.bytes();
        pages += 1;
    }

    Ok(pages)
}

/// What a virtual address resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Translation {
    /// Physical address.
    pub pa: u64,
    /// Size of the page it falls in.
    pub size: PageSize,
    /// Permission bits of the leaf entry.
    pub bits: u64,
}

impl Translation {
    /// True when the mapping permits execution.
    pub const fn is_executable(&self) -> bool {
        self.bits & pte::X != 0
    }

    /// True when the mapping permits writing.
    pub const fn is_writable(&self) -> bool {
        self.bits & pte::W != 0
    }
}

/// Resolves a virtual address through a table.
///
/// Reads the tables the same way the hardware does, which is what makes it
/// useful as a check before the tables are installed: a mapping this cannot
/// resolve is one the hardware will fault on, and finding that out before the
/// translation is live is the difference between an error message and a
/// machine that stops with no console.
pub fn translate(root: u64, va: u64, mem: &impl PhysMem) -> Option<Translation> {
    if va >= (1u64 << 39) {
        return None;
    }

    let mut table = root;
    let mut level = ROOT_LEVEL;

    loop {
        let entry = mem.read_u64(entry_address(table, index_at(va, level)));
        if entry & pte::V == 0 {
            return None;
        }

        if is_leaf(entry) {
            let size = match level {
                0 => PageSize::Small,
                1 => PageSize::Mega,
                // A leaf at the root is a gigabyte page, which nothing here
                // creates. Treated as unresolvable rather than guessed at.
                _ => return None,
            };
            let page = branch_target(entry);
            let offset = va & (size.bytes() - 1);
            return Some(Translation {
                pa: page + offset,
                size,
                bits: entry,
            });
        }

        if level == 0 {
            // A level zero entry that is not a leaf points at a fourth table,
            // which Sv39 does not have.
            return None;
        }
        table = branch_target(entry);
        level -= 1;
    }
}

/// Value for satp that selects Sv39 with a root table.
///
/// Mode eight is Sv39. The address space identifier is zero: there is one
/// address space until there are tasks, and a nonzero value would have to be
/// tracked against a flush.
pub const fn satp_value(root: u64) -> u64 {
    (8u64 << 60) | (root >> PAGE_SHIFT)
}