//! Envisor security layer.
//!
//! The boundary between the executive and everything below it, and eventually
//! between the executive and the tasks above. Three configurations, chosen at
//! build time:
//!
//! ```text
//!   Off        no layer. The executive owns the machine.
//!   Integrity  the layer owns the page tables and the seal policy, in the
//!              same privilege level as the executive.
//!   Isolated   the executive runs in U, the layer in S.
//! ```
//!
//! Only Off and Integrity are implemented here. Isolated needs worlds with
//! separate address spaces, and building those before there is anything to put
//! in them is machinery for its own sake.
//!
//! What Integrity is and is not, stated plainly because the difference is
//! easy to lose: the layer runs in S alongside the executive, so any code in
//! S can write satp and undo everything the layer decides. It defends against
//! a defect in the executive, not against an executive that has been
//! subverted. The value is real and the bound is exact.
//!
//! One property is enforced identically in every mode, because it costs
//! nothing and is the one that matters most in practice: no mapping grants
//! write and execute together. The type system already makes it unstateable;
//! this checks it anyway, on the principle that an invariant worth having is
//! worth two independent reasons to believe.

use exec_abi::esl::{
    encode_map_flags, flags_global, flags_permission, flags_size, EslCounter, EslError, EslResult,
    EslService, WorldId, ESL_INTERFACE_VERSION,
};
use exec_abi::EslMode;

use crate::paging::{self, MapError, PageSize, Perm, PhysMem};
use crate::pmm::Pmm;
use crate::window::WindowSet;

/// Regions the layer records as sealed.
///
/// A region becomes executable only after it appears here. Sized for the
/// kernel text plus a handful of driver images.
pub const MAX_SEALED: usize = 16;

/// A region that has been measured and made unwritable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SealedRegion {
    /// First byte.
    pub base: u64,
    /// Length.
    pub size: u64,
    /// Value recorded at the time of sealing.
    ///
    /// Not cryptographic. See the measure function: this detects a region that
    /// has changed, not a region that has been changed by someone who did not
    /// want it noticed.
    pub measurement: u64,
    /// World the region belongs to.
    pub world: WorldId,
}

impl SealedRegion {
    /// An unused entry.
    pub const EMPTY: SealedRegion = SealedRegion {
        base: 0,
        size: 0,
        measurement: 0,
        world: WorldId::KERNEL,
    };

    /// True when an address falls inside.
    pub const fn contains(&self, address: u64) -> bool {
        self.size != 0 && address >= self.base && address - self.base < self.size
    }
}

/// One isolation domain.
///
/// Not Copy. WindowSet is generated from the proved source and carries no
/// derives, and adding them there would be an edit to a generated file. The
/// worlds are only ever reached in place, so nothing needs to copy one.
pub struct World {
    /// Physical regions a device may touch on this world's behalf.
    pub windows: WindowSet,
    /// Root page table, or zero when the world shares the executive's.
    ///
    /// Zero in Off and Integrity, where there is one address space. Set in
    /// Isolated, where each world has its own.
    pub root: u64,
    /// Whether the entry is in use.
    pub live: bool,
}

impl World {
    /// An unused entry.
    ///
    /// const, so the table is a static and the layer allocates nothing.
    pub const EMPTY: World = World {
        windows: WindowSet::new(),
        root: 0,
        live: false,
    };
}

// The table is a fixed array and its length is part of the interface, so a
// change to one without the other is caught here rather than at the first
// index that goes out of range.
const _: () = assert!(WorldId::MAX == 8);

/// Counters.
#[derive(Clone, Copy, Default, Debug)]
pub struct EslStats {
    /// Mappings made.
    pub mapped: u64,
    /// Mappings refused by policy.
    pub refused: u64,
    /// Window checks that passed.
    pub window_pass: u64,
    /// Window checks that failed.
    pub window_fail: u64,
    /// Regions sealed.
    pub sealed: u64,
}

/// The layer.
pub struct Esl {
    mode: EslMode,
    worlds: [World; WorldId::MAX],
    sealed: [SealedRegion; MAX_SEALED],
    sealed_count: usize,
    /// Memory belonging to the layer itself.
    ///
    /// Recorded so that a mapping request naming it can be refused. Vacuous in
    /// Integrity, where the layer is part of the kernel image; the check is
    /// present so that the Isolated mode is a configuration and not a rewrite.
    own_base: u64,
    own_size: u64,
    stats: EslStats,
}

impl Esl {
    /// A layer that is switched off.
    ///
    /// const, because the executive is a static. The mode is set at boot from
    /// the value the image was built with.
    pub const fn new() -> Self {
        Esl {
            mode: EslMode::Off,
            // Written out because World is not Copy and the repeat form needs
            // it. Eight entries; a machine of this class runs a handful of
            // drivers and the table is sized for that.
            worlds: [
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
                World::EMPTY,
            ],
            sealed: [SealedRegion::EMPTY; MAX_SEALED],
            sealed_count: 0,
            own_base: 0,
            own_size: 0,
            stats: EslStats {
                mapped: 0,
                refused: 0,
                window_pass: 0,
                window_fail: 0,
                sealed: 0,
            },
        }
    }

    /// Brings the layer up.
    ///
    /// World zero is created and, in Off and Integrity, given a window over
    /// all of physical memory. The mechanism runs in every mode and the policy
    /// is what differs: a permissive window exercises the same code that a
    /// narrow one will, so the path is not first taken on the day it matters.
    pub fn init(&mut self, mode: EslMode, ram_base: u64, ram_size: u64) {
        self.mode = mode;

        let kernel = &mut self.worlds[WorldId::KERNEL.0 as usize];
        kernel.live = true;
        kernel.root = 0;
        kernel.windows.clear();

        match mode {
            EslMode::Off | EslMode::Integrity => {
                // Everything, writable. The executive is trusted in both, and
                // a window it cannot exceed would only describe what it is
                // already permitted to do.
                kernel.windows.add(ram_base, ram_size, true);
            }
            EslMode::Isolated => {
                // The executive gets no default window: in Isolated it does no
                // device work of its own, and a driver that needs one receives
                // it when its world is created.
            }
        }
    }

    /// Records the memory the layer occupies.
    pub fn set_own_region(&mut self, base: u64, size: u64) {
        self.own_base = base;
        self.own_size = size;
    }

    /// The mode in effect.
    pub const fn mode(&self) -> EslMode {
        self.mode
    }

    /// Counters.
    pub const fn stats(&self) -> EslStats {
        self.stats
    }

    /// Windows of a world, for a caller that needs to add one.
    pub fn windows_mut(&mut self, world: WorldId) -> Option<&mut WindowSet> {
        if !world.is_valid() {
            return None;
        }
        let entry = &mut self.worlds[world.0 as usize];
        if !entry.live {
            return None;
        }
        Some(&mut entry.windows)
    }

    // ------------------------------------------------------------ policy

    /// True when a range overlaps a sealed region.
    fn overlaps_sealed(&self, base: u64, size: u64) -> bool {
        if size == 0 {
            return false;
        }
        let mut i = 0;
        while i < self.sealed_count && i < MAX_SEALED {
            let region = self.sealed[i];
            if region.size != 0 {
                // Two ranges miss each other when one ends at or before the
                // other begins. Written with subtraction so neither end has to
                // be computed as a sum that could wrap.
                let a_before_b = size <= region.base.wrapping_sub(base) && base <= region.base;
                let b_before_a = region.size <= base.wrapping_sub(region.base)
                    && region.base <= base;
                if !(a_before_b || b_before_a) {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    /// True when a range overlaps the layer's own memory.
    fn overlaps_own(&self, base: u64, size: u64) -> bool {
        if self.own_size == 0 || size == 0 {
            return false;
        }
        let ends_before = size <= self.own_base.wrapping_sub(base) && base <= self.own_base;
        let starts_after =
            self.own_size <= base.wrapping_sub(self.own_base) && self.own_base <= base;
        !(ends_before || starts_after)
    }

    /// Decides whether a mapping is permitted.
    ///
    /// The gate every mapping passes through. In Off it enforces the one rule
    /// that costs nothing; in Integrity it enforces the rest.
    fn permit_map(
        &self,
        world: WorldId,
        va: u64,
        pa: u64,
        size: PageSize,
        perm: Perm,
    ) -> Result<(), EslError> {
        if !world.is_valid() || !self.worlds[world.0 as usize].live {
            return Err(EslError::NoSuchWorld);
        }

        // I1, in every mode. The permission type cannot express write and
        // execute together, so reaching this means something reconstructed a
        // permission from bits, and that is exactly when a second check earns
        // its place.
        if perm.is_writable() && perm.is_executable() {
            return Err(EslError::Refused);
        }

        if self.mode == EslMode::Off {
            return Ok(());
        }

        let length = size.bytes();

        // I3. The layer's memory is not mapped for anyone else.
        if world != WorldId::KERNEL && self.overlaps_own(pa, length) {
            return Err(EslError::Refused);
        }

        // I2. Execute is granted only to what has been sealed, and a sealed
        // region is not made writable again.
        if perm.is_executable() {
            let mut covered = false;
            let mut i = 0;
            while i < self.sealed_count && i < MAX_SEALED {
                if self.sealed[i].contains(pa) {
                    covered = true;
                    break;
                }
                i += 1;
            }
            if !covered {
                return Err(EslError::Refused);
            }
        }

        if perm.is_writable() && self.overlaps_sealed(pa, length) {
            return Err(EslError::Sealed);
        }

        let _ = va;
        Ok(())
    }

    // ------------------------------------------------------------ services

    /// Maps one page, subject to policy.
    ///
    /// Every mapping in the system goes through here. The allocator and the
    /// physical memory accessor are arguments rather than state because the
    /// layer does not own them: it decides, the caller supplies the means.
    #[allow(clippy::too_many_arguments)]
    pub fn map(
        &mut self,
        world: WorldId,
        root: u64,
        va: u64,
        pa: u64,
        size: PageSize,
        perm: Perm,
        global: bool,
        pmm: &mut Pmm,
        mem: &mut impl PhysMem,
    ) -> Result<(), EslError> {
        if let Err(error) = self.permit_map(world, va, pa, size, perm) {
            self.stats.refused = self.stats.refused.saturating_add(1);
            return Err(error);
        }

        match paging::map(root, va, pa, size, perm, global, pmm, mem) {
            Ok(()) => {
                self.stats.mapped = self.stats.mapped.saturating_add(1);
                Ok(())
            }
            Err(MapError::OutOfMemory) => Err(EslError::NoMemory),
            Err(MapError::AlreadyMapped) => Err(EslError::AlreadyMapped),
            Err(_) => Err(EslError::BadArgument),
        }
    }

    /// Checks a physical range against a world's windows.
    ///
    /// Called before a device is given an address. The check itself is proved;
    /// what is here is the lookup of which world is asking and the counters.
    pub fn window_check(&mut self, world: WorldId, addr: u64, len: u64, write: bool) -> bool {
        if !world.is_valid() {
            self.stats.window_fail = self.stats.window_fail.saturating_add(1);
            return false;
        }
        let entry = &self.worlds[world.0 as usize];
        if !entry.live {
            self.stats.window_fail = self.stats.window_fail.saturating_add(1);
            return false;
        }

        let permitted = entry.windows.contains(addr, len, write);
        if permitted {
            self.stats.window_pass = self.stats.window_pass.saturating_add(1);
        } else {
            self.stats.window_fail = self.stats.window_fail.saturating_add(1);
        }
        permitted
    }

    /// Measures a region and records it.
    ///
    /// After this the region may be made executable and may not be made
    /// writable. Idempotent for a region already sealed at the same extent,
    /// so a caller that seals twice is not an error.
    pub fn seal(
        &mut self,
        world: WorldId,
        base: u64,
        size: u64,
        mem: &impl PhysMem,
    ) -> Result<u64, EslError> {
        if !world.is_valid() || !self.worlds[world.0 as usize].live {
            return Err(EslError::NoSuchWorld);
        }
        if size == 0 || base > u64::MAX - size {
            return Err(EslError::BadArgument);
        }

        let mut i = 0;
        while i < self.sealed_count && i < MAX_SEALED {
            if self.sealed[i].base == base && self.sealed[i].size == size {
                return Ok(self.sealed[i].measurement);
            }
            i += 1;
        }

        if self.sealed_count >= MAX_SEALED {
            return Err(EslError::NoResources);
        }

        let measurement = measure(base, size, mem);
        self.sealed[self.sealed_count] = SealedRegion {
            base,
            size,
            measurement,
            world,
        };
        self.sealed_count += 1;
        self.stats.sealed = self.stats.sealed.saturating_add(1);
        Ok(measurement)
    }

    /// Recomputes the measurement of a sealed region and compares it.
    ///
    /// What an equivalent of PatchGuard calls. Returns false when the region
    /// has changed since it was sealed, which under the stated policy cannot
    /// happen through a mapping and therefore means it happened another way.
    pub fn verify_seal(&self, base: u64, mem: &impl PhysMem) -> Option<bool> {
        let mut i = 0;
        while i < self.sealed_count && i < MAX_SEALED {
            let region = self.sealed[i];
            if region.base == base && region.size != 0 {
                return Some(measure(region.base, region.size, mem) == region.measurement);
            }
            i += 1;
        }
        None
    }

    /// Sealed regions.
    pub const fn sealed_count(&self) -> usize {
        self.sealed_count
    }

    /// A sealed region by index.
    pub fn sealed_at(&self, index: usize) -> Option<SealedRegion> {
        if index >= self.sealed_count || index >= MAX_SEALED {
            return None;
        }
        Some(self.sealed[index])
    }

    // ------------------------------------------------------------ dispatch

    /// Handles a call by service number.
    ///
    /// The single entry point the Isolated mode's ecall will land in. Present
    /// and exercised in every mode so that the transport is the only thing
    /// that changes when the layer moves to another privilege level.
    ///
    /// Services that need an allocator are not here: they take arguments this
    /// interface cannot carry, and a layer that owns page tables owns an
    /// allocator too. That is part of the Isolated work.
    pub fn dispatch(&mut self, service: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> EslResult {
        let service = match EslService::from_raw(service) {
            Some(service) => service,
            None => return EslResult::err(EslError::UnknownService),
        };

        match service {
            EslService::Version => EslResult::ok(ESL_INTERFACE_VERSION),

            EslService::Mode => EslResult::ok(self.mode as u64),

            EslService::WindowCheck => {
                let world = WorldId(a0 as u16);
                if self.window_check(world, a1, a2, a3 != 0) {
                    EslResult::ok(1)
                } else {
                    EslResult::err(EslError::OutsideWindow)
                }
            }

            EslService::Stats => {
                let value = match a0 {
                    x if x == EslCounter::Mapped as u64 => self.stats.mapped,
                    x if x == EslCounter::Refused as u64 => self.stats.refused,
                    x if x == EslCounter::WindowPass as u64 => self.stats.window_pass,
                    x if x == EslCounter::WindowFail as u64 => self.stats.window_fail,
                    x if x == EslCounter::Sealed as u64 => self.stats.sealed,
                    _ => return EslResult::err(EslError::BadArgument),
                };
                EslResult::ok(value)
            }

            // Map, Unmap and Seal reach physical memory and the allocator,
            // which this signature cannot carry. They are called directly
            // until the layer owns both, which is the Isolated work.
            EslService::Map | EslService::Unmap | EslService::Seal => {
                let _ = (a0, a1, a2, a3);
                EslResult::err(EslError::UnknownService)
            }
        }
    }
}

impl Default for Esl {
    fn default() -> Self {
        Self::new()
    }
}

/// Measures a region.
///
/// FNV-1a over the bytes, read eight at a time. This detects a region that has
/// changed and does not detect a region that was changed by someone who did not
/// want it noticed: the function is not cryptographic, has no collision
/// resistance worth the name, and can be inverted by anyone who cares to.
///
/// Named a measurement anyway because the mechanism around it is the point of
/// this iteration and is identical either way: measure, record, seal, and admit
/// execute only for what has been through it. Substituting a real hash is a
/// change to this function and nothing else.
fn measure(base: u64, size: u64, mem: &impl PhysMem) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    // The length goes in first, so two regions differing only in extent do not
    // measure the same when one is a prefix of the other.
    hash ^= size;
    hash = hash.wrapping_mul(PRIME);

    let mut offset = 0u64;
    while offset + 8 <= size {
        let word = mem.read_u64(base + offset);
        hash ^= word;
        hash = hash.wrapping_mul(PRIME);
        offset += 8;
    }

    // A tail shorter than eight bytes is read as the word containing it, with
    // the bytes past the end masked off. Reading the whole word is safe: a
    // region is page aligned at both ends by every caller here.
    if offset < size {
        let remaining = size - offset;
        let word = mem.read_u64(base + offset);
        let mask = if remaining >= 8 {
            u64::MAX
        } else {
            (1u64 << (remaining * 8)) - 1
        };
        hash ^= word & mask;
        hash = hash.wrapping_mul(PRIME);
    }

    hash
}

/// Encodes a permission for the map flags argument.
pub const fn permission_code(perm: Perm) -> u8 {
    match perm {
        Perm::ReadOnly => 0,
        Perm::ReadWrite => 1,
        Perm::ReadExecute => 2,
    }
}

/// Decodes a permission from the map flags argument.
pub const fn permission_from_code(code: u8) -> Option<Perm> {
    match code {
        0 => Some(Perm::ReadOnly),
        1 => Some(Perm::ReadWrite),
        2 => Some(Perm::ReadExecute),
        _ => None,
    }
}

/// Builds the flags argument for a map call.
pub const fn map_flags(perm: Perm, size: PageSize, global: bool) -> u64 {
    let size_code = match size {
        PageSize::Small => 0,
        PageSize::Mega => 1,
    };
    encode_map_flags(permission_code(perm), size_code, global)
}

/// Reads back what map_flags encoded.
pub const fn map_flags_decode(flags: u64) -> Option<(Perm, PageSize, bool)> {
    let perm = match permission_from_code(flags_permission(flags)) {
        Some(perm) => perm,
        None => return None,
    };
    let size = match flags_size(flags) {
        0 => PageSize::Small,
        1 => PageSize::Mega,
        _ => return None,
    };
    Some((perm, size, flags_global(flags)))
}