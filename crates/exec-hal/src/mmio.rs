//! Memory mapped register access.
//!
//! Volatile throughout. A device register is not memory: reading it can have
//! an effect, two reads can give different values with nothing written in
//! between, and a write that looks dead is not. Ordinary accesses would let
//! the compiler merge, reorder or delete any of that.
//!
//! Every function is unsafe because nothing here can establish that the
//! address names a device. The caller has that from the device tree.

/// Reads a byte register.
///
/// # Safety
///
/// The address must be a mapped device register.
pub unsafe fn read8(address: usize) -> u8 {
    (address as *const u8).read_volatile()
}

/// Writes a byte register.
///
/// # Safety
///
/// The address must be a mapped device register.
pub unsafe fn write8(address: usize, value: u8) {
    (address as *mut u8).write_volatile(value)
}

/// Reads a word register.
///
/// # Safety
///
/// The address must be a mapped device register, four byte aligned.
pub unsafe fn read32(address: usize) -> u32 {
    (address as *const u32).read_volatile()
}

/// Writes a word register.
///
/// # Safety
///
/// The address must be a mapped device register, four byte aligned.
pub unsafe fn write32(address: usize, value: u32) {
    (address as *mut u32).write_volatile(value)
}