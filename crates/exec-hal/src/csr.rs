//! Supervisor control and status registers.
//!
//! Direct access. Valid only when the executive runs in S, that is in the Off
//! and Integrity ESL modes. In Isolated these are reached through ESL instead;
//! the callers go through the wrappers at the bottom of this file rather than
//! through the macros.

/// Reads a CSR by name.
#[macro_export]
macro_rules! csr_read {
    ($name:literal) => {{
        let value: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrr {0}, ", $name),
                out(reg) value,
                options(nomem, nostack)
            );
        }
        value
    }};
}

/// Writes a CSR by name.
#[macro_export]
macro_rules! csr_write {
    ($name:literal, $value:expr) => {{
        let value: usize = $value;
        unsafe {
            core::arch::asm!(
                concat!("csrw ", $name, ", {0}"),
                in(reg) value,
                options(nomem, nostack)
            );
        }
    }};
}

/// Sets the bits of a CSR and returns its previous value.
#[macro_export]
macro_rules! csr_set {
    ($name:literal, $value:expr) => {{
        let value: usize = $value;
        let old: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrrs {0}, ", $name, ", {1}"),
                out(reg) old,
                in(reg) value,
                options(nomem, nostack)
            );
        }
        old
    }};
}

/// Clears the bits of a CSR and returns its previous value.
#[macro_export]
macro_rules! csr_clear {
    ($name:literal, $value:expr) => {{
        let value: usize = $value;
        let old: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrrc {0}, ", $name, ", {1}"),
                out(reg) old,
                in(reg) value,
                options(nomem, nostack)
            );
        }
        old
    }};
}

/// Bit positions in sstatus.
pub mod sstatus {
    /// Supervisor interrupt enable.
    pub const SIE: usize = 1 << 1;
    /// Interrupt enable in effect before the trap.
    pub const SPIE: usize = 1 << 5;
    /// Privilege level in effect before the trap. Set means S.
    pub const SPP: usize = 1 << 8;
    /// Permit S mode access to pages marked user.
    pub const SUM: usize = 1 << 18;
    /// Make executable pages readable.
    pub const MXR: usize = 1 << 19;
}

/// Bit positions in sie and sip.
pub mod sint {
    /// Supervisor software interrupt.
    pub const SSI: usize = 1 << 1;
    /// Supervisor timer interrupt.
    pub const STI: usize = 1 << 5;
    /// Supervisor external interrupt.
    pub const SEI: usize = 1 << 9;
}

/// Reads sstatus.
pub fn sstatus_read() -> usize {
    crate::csr_read!("sstatus")
}

/// Reads the interrupt enable mask.
pub fn sie_read() -> usize {
    crate::csr_read!("sie")
}

/// Writes the interrupt enable mask.
pub fn sie_write(value: usize) {
    crate::csr_write!("sie", value);
}

/// Reads the pending interrupt mask.
pub fn sip_read() -> usize {
    crate::csr_read!("sip")
}

/// Enables interrupts globally and returns whether they were already enabled.
pub fn interrupts_enable() -> bool {
    (crate::csr_set!("sstatus", sstatus::SIE) & sstatus::SIE) != 0
}

/// Disables interrupts globally and returns whether they were enabled.
pub fn interrupts_disable() -> bool {
    (crate::csr_clear!("sstatus", sstatus::SIE) & sstatus::SIE) != 0
}

/// Installs the trap vector. The address must be four byte aligned; the low
/// two bits select the mode and zero means every trap enters at one address.
pub fn stvec_write(address: usize) {
    crate::csr_write!("stvec", address & !0b11);
}

/// Reads the trap cause.
pub fn scause_read() -> usize {
    crate::csr_read!("scause")
}

/// Reads the faulting address or instruction associated with a trap.
pub fn stval_read() -> usize {
    crate::csr_read!("stval")
}

/// Reads the address a trap returns to.
pub fn sepc_read() -> usize {
    crate::csr_read!("sepc")
}

/// Sets the address a trap returns to.
pub fn sepc_write(address: usize) {
    crate::csr_write!("sepc", address);
}

/// Reads the address translation register.
pub fn satp_read() -> usize {
    crate::csr_read!("satp")
}

/// Reads the monotonic cycle counter.
///
/// Sedna serialises and restores machine state, so wall clock deltas across a
/// restore are arbitrary. Only this counter is treated as time, and code that
/// consumes it must tolerate a large jump.
pub fn time_read() -> u64 {
    crate::csr_read!("time") as u64
}