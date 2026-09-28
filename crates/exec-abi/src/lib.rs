//! Types that cross every boundary in the system.
//!
//! Shared by the executive, the HAL, ESL and loadable drivers. Contains no
//! code that touches hardware and no unsafe, so that agreeing on an interface
//! does not mean depending on an implementation.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod esl;

/// Version of the interface between the executive and a loadable module.
///
/// A module records the value it was built against. The loader refuses a
/// mismatch rather than attempting compatibility: the target has no compiler,
/// so a module is checked on load, never rebuilt.
pub const ABI_VERSION: u32 = 1;

/// Result of an operation that can fail.
///
/// Values are stable and negative when they denote failure, so that the same
/// encoding survives a register return across an ecall boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Status {
    /// Operation completed.
    Success = 0,
    /// A handle did not name a live object of the expected type.
    InvalidHandle = -1,
    /// An argument was outside its permitted range.
    InvalidArgument = -2,
    /// A fixed size table has no free slot.
    NoResources = -3,
    /// The caller lacks the right required for this operation.
    AccessDenied = -4,
    /// The named object does not exist.
    NotFound = -5,
    /// The object already exists under that name.
    Exists = -6,
    /// The operation is not implemented for this object type.
    NotSupported = -7,
    /// The request was aborted before completion.
    Cancelled = -8,
    /// The device or software layer reported a failure.
    DeviceError = -9,
    /// The operation would block at an IRQL where blocking is forbidden.
    WouldBlock = -10,
    /// The requested IRQL transition is not permitted from the current level.
    IrqlViolation = -11,
}

impl Status {
    /// True when the status denotes success.
    pub const fn is_ok(self) -> bool {
        matches!(self, Status::Success)
    }
}

/// Kind of object a handle refers to.
///
/// The tag is carried in the handle so that a lookup can reject a handle of
/// the wrong type before it indexes anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ObjectType {
    /// Address space and resource container.
    Task = 1,
    /// Schedulable execution context.
    Thread = 2,
    /// Endpoint for message passing.
    Port = 3,
    /// Region of physical memory that can be mapped.
    Section = 4,
    /// Target of I/O requests.
    Device = 5,
    /// Loaded module providing device behaviour.
    Driver = 6,
    /// Expiry notification.
    Timer = 7,
    /// Binary signalled state.
    Event = 8,
}

/// Reference to a kernel object.
///
/// Layout:
///
/// ```text
///  bits  0..15   index into the array for the type
///  bits 16..31   generation, incremented when the slot is released
///  bits 32..39   type tag
///  bits 40..63   reserved, zero
/// ```
///
/// The generation is what makes a stale handle detectable instead of
/// dangerous. A lookup compares it against the one held by the slot, so a
/// handle whose object has been closed names nothing, even after the slot has
/// been reused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Handle(pub u64);

/// Handle value that never names an object.
pub const HANDLE_NULL: Handle = Handle(0);

impl Handle {
    /// Builds a handle from its parts.
    pub const fn new(index: u16, generation: u16, ty: ObjectType) -> Self {
        Handle((index as u64) | ((generation as u64) << 16) | ((ty as u64) << 32))
    }

    /// Index into the array for the type.
    pub const fn index(self) -> u16 {
        self.0 as u16
    }

    /// Generation recorded when the handle was issued.
    pub const fn generation(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// Raw type tag. Not decoded here: an out of range tag is a failed lookup,
    /// not a panic, and the decision belongs to the object manager.
    pub const fn type_tag(self) -> u8 {
        (self.0 >> 32) as u8
    }

    /// True when the reserved bits are clear.
    pub const fn is_well_formed(self) -> bool {
        (self.0 >> 40) == 0
    }

    /// True when this is the null handle.
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// Interrupt request level.
///
/// A number rather than an enumeration, because the device range is a range:
/// there are seven device levels and code compares against them arithmetically.
/// The values are the ones the HAL maps onto the sie mask and the PLIC
/// threshold, so they are part of the interface and not an internal encoding.
///
/// ```text
///   0        PASSIVE     normal execution, may block
///   1                    reserved, see below
///   2        DISPATCH    scheduler and DPCs, may not block
///   3..9     DEVICE(n)   interrupt service, n is the PLIC priority
///   15       HIGH        everything masked
/// ```
///
/// Level 1 is reserved and never used. It is where NT puts APC_LEVEL, which
/// exists to run a callback in the context of a specific thread. A run to
/// completion kernel has no such context, so the level is left empty rather
/// than renumbered, to keep the correspondence with NT readable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct Irql(pub u8);

impl Irql {
    /// Normal execution. May block, may take a fault.
    pub const PASSIVE: Irql = Irql(0);
    /// Scheduler and deferred procedure calls. May not block.
    pub const DISPATCH: Irql = Irql(2);
    /// Lowest device level.
    pub const DEVICE_MIN: Irql = Irql(3);
    /// Highest priority the interrupt controller implements.
    ///
    /// Seven on both target machines. A source registered above this cannot be
    /// told apart from one at seven, and a threshold written above it masks
    /// every source there is, so the value is a bound and not a suggestion.
    pub const MAX_PRIORITY: u8 = 7;
    /// Highest device level.
    ///
    /// Derived, so the range and the controller cannot disagree.
    pub const DEVICE_MAX: Irql = Irql(Irql::DEVICE_MIN.0 + Irql::MAX_PRIORITY - 1);
    /// Everything masked.
    pub const HIGH: Irql = Irql(15);

    /// Level for a device of the given PLIC priority.
    ///
    /// Priority 0 means the source is disabled in the PLIC and has no level,
    /// so it is folded onto the lowest device level rather than rejected: this
    /// is called on a path that has no way to report failure.
    pub const fn device(priority: u8) -> Irql {
        if priority == 0 {
            return Irql::DEVICE_MIN;
        }
        let level = Irql::DEVICE_MIN.0 + priority - 1;
        if level > Irql::DEVICE_MAX.0 {
            Irql::DEVICE_MAX
        } else {
            Irql(level)
        }
    }

    /// PLIC priority this level corresponds to, or zero below the device range.
    pub const fn plic_priority(self) -> u8 {
        if self.0 < Irql::DEVICE_MIN.0 {
            0
        } else {
            self.0 - Irql::DEVICE_MIN.0 + 1
        }
    }

    /// True when the level is one the system defines.
    ///
    /// The gap between the top of the device range and HIGH is not a level. No
    /// controller priority maps into it, so a raise there would mask nothing it
    /// had not already masked, and the matching lower would restore a state
    /// that was never entered.
    pub const fn is_valid(self) -> bool {
        self.0 != 1 && (self.0 <= Irql::DEVICE_MAX.0 || self.0 == Irql::HIGH.0)
    }

    /// True when code at this level is allowed to block or fault.
    pub const fn may_block(self) -> bool {
        self.0 < Irql::DISPATCH.0
    }

    /// True when this level is in the device range.
    pub const fn is_device(self) -> bool {
        self.0 >= Irql::DEVICE_MIN.0 && self.0 <= Irql::DEVICE_MAX.0
    }
}

/// Isolation configuration the image was built for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EslMode {
    /// No security layer. The executive runs in S and owns the machine.
    Off = 0,
    /// The executive runs in S; ESL owns page tables and code integrity by
    /// convention. Defends against defects, not against subversion.
    Integrity = 1,
    /// The executive runs in U; ESL runs in S and owns all privilege.
    Isolated = 2,
}