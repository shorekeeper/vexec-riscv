//! Interface to the security layer.
//!
//! Shared by the executive, the layer itself, and eventually by drivers. The
//! shape is fixed by the strictest transport, which is the ecall used in the
//! Isolated mode: arguments are register width values, there are at most six,
//! and none of them is a pointer.
//!
//! The absence of pointer arguments is the load bearing rule. A structure the
//! caller can reach is a structure the caller can change between the moment it
//! is checked and the moment it is used, and that is the ordinary way a
//! validated system call is defeated. Everything crosses by value.

/// Which service a call asks for.
///
/// Numbered rather than derived from an enum position, because the value
/// crosses a privilege boundary and has to survive both sides being built
/// separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
pub enum EslService {
    /// Interface version of the layer. Takes nothing.
    Version = 0,
    /// Isolation mode in effect. Takes nothing.
    Mode = 1,
    /// Map one page into a world.
    ///
    /// Arguments: world, virtual address, physical address, encoded page size
    /// and permission.
    Map = 2,
    /// Remove a mapping. Arguments: world, virtual address.
    Unmap = 3,
    /// Check a physical range against a world's windows.
    ///
    /// Arguments: world, address, length, nonzero for a write. This is what a
    /// driver calls before handing an address to a device, and what ESL calls
    /// on its behalf when the driver is not trusted to.
    WindowCheck = 4,
    /// Record a measurement of a region and mark it unwritable.
    ///
    /// Arguments: world, address, length. Executable permission is only
    /// granted to a region that has been through this.
    Seal = 5,
    /// Counters, for diagnosis. Argument: which counter.
    Stats = 6,
}

impl EslService {
    /// Service for a raw number, or None when unknown.
    ///
    /// An unknown service is refused rather than ignored: a caller asking for
    /// something that does not exist has a different idea of the interface
    /// than the layer does, and continuing hides that.
    pub const fn from_raw(value: u64) -> Option<EslService> {
        match value {
            0 => Some(EslService::Version),
            1 => Some(EslService::Mode),
            2 => Some(EslService::Map),
            3 => Some(EslService::Unmap),
            4 => Some(EslService::WindowCheck),
            5 => Some(EslService::Seal),
            6 => Some(EslService::Stats),
            _ => None,
        }
    }
}

/// Result of a call into the layer.
///
/// Two registers. Negative status is failure, and the value is meaningful only
/// on success.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EslResult {
    /// Zero on success.
    pub status: i64,
    /// Meaning depends on the service.
    pub value: u64,
}

impl EslResult {
    /// Success carrying a value.
    pub const fn ok(value: u64) -> Self {
        EslResult { status: 0, value }
    }

    /// Failure.
    pub const fn err(status: EslError) -> Self {
        EslResult {
            status: status as i64,
            value: 0,
        }
    }

    /// True when the call succeeded.
    pub const fn is_ok(self) -> bool {
        self.status == 0
    }
}

/// Why a call was refused.
///
/// Separate from the executive's Status because these cross a privilege
/// boundary and their numbering is part of the interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i64)]
pub enum EslError {
    /// The service number is not one the layer implements.
    UnknownService = -1,
    /// The world does not exist.
    NoSuchWorld = -2,
    /// An argument is outside its permitted range.
    BadArgument = -3,
    /// The policy forbids this. The usual case is write and execute together.
    Refused = -4,
    /// A physical range lies outside every window of the world.
    OutsideWindow = -5,
    /// No memory for an intermediate page table.
    NoMemory = -6,
    /// Something is already mapped there.
    AlreadyMapped = -7,
    /// The region has been sealed and cannot be changed.
    Sealed = -8,
    /// A table is full.
    NoResources = -9,
}

/// Which world a call refers to.
///
/// The executive is world zero in every mode. In Off and Integrity it is the
/// only one; in Isolated each driver is a world of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct WorldId(pub u16);

impl WorldId {
    /// The executive.
    pub const KERNEL: WorldId = WorldId(0);
    /// Worlds a build supports.
    ///
    /// Small on purpose: each one costs a page table root and a window set,
    /// and a machine of this class runs a handful of drivers.
    pub const MAX: usize = 8;

    /// True when this names a world a table could hold.
    pub const fn is_valid(self) -> bool {
        (self.0 as usize) < WorldId::MAX
    }
}

/// Counters the layer keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
pub enum EslCounter {
    /// Mappings made.
    Mapped = 0,
    /// Mappings refused by policy.
    Refused = 1,
    /// Window checks that passed.
    WindowPass = 2,
    /// Window checks that failed.
    WindowFail = 3,
    /// Regions sealed.
    Sealed = 4,
}

/// Encodes a page size and permission into one argument.
///
/// The Map service has five arguments and six registers, so this could be two
/// fields. It is one because the pair is validated together and a caller that
/// can name a size without a permission has a state the layer would have to
/// reject anyway.
///
/// ```text
///   bits 0..3   permission: 0 read, 1 read write, 2 read execute
///   bits 4..7   size: 0 four kilobytes, 1 two megabytes
///   bit  8      global
/// ```
pub const fn encode_map_flags(permission: u8, size: u8, global: bool) -> u64 {
    ((permission as u64) & 0xf) | (((size as u64) & 0xf) << 4) | if global { 1 << 8 } else { 0 }
}

/// Permission from encoded flags.
pub const fn flags_permission(flags: u64) -> u8 {
    (flags & 0xf) as u8
}

/// Page size from encoded flags.
pub const fn flags_size(flags: u64) -> u8 {
    ((flags >> 4) & 0xf) as u8
}

/// Global bit from encoded flags.
pub const fn flags_global(flags: u64) -> bool {
    (flags & (1 << 8)) != 0
}

/// Version of this interface.
///
/// Distinct from the module ABI version: a driver built against one may run
/// against a layer built against another, and the two move independently.
pub const ESL_INTERFACE_VERSION: u64 = 1;