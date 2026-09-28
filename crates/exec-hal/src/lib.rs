//! Hardware abstraction layer.
//!
//! The only crate in the system that contains privileged instructions and
//! memory mapped register access. This is not a style rule: in the Isolated
//! ESL mode the executive runs in U and can do neither, so every such
//! operation has to be a function here that switches between an instruction
//! and an ecall. Code that reads a CSR directly elsewhere would compile in one
//! mode and fault in another.
//!
//! Trusted and not verified. Verus has no model of a privileged register or of
//! a device that changes under a read, so a proof here would be a proof about
//! something else. Kept small enough to read in one sitting, because its size
//! is the size of what the proofs above it rest on.

#![no_std]
#![allow(unsafe_code)]
#![deny(missing_docs)]

pub mod console;
pub mod csr;
pub mod dtb;
pub mod interrupt;
pub mod mmio;
pub mod paging;
pub mod plic;
pub mod sbi;
pub mod timer;
pub mod trap;
pub mod uart;

/// Halts the hart forever.
///
/// Used by the panic path and by parked harts. wfi is a hint and may return at
/// any time, so it is in a loop.
pub fn park() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nostack));
        }
    }
}

/// Waits for the next interrupt.
///
/// The idle path. Returns when any interrupt is taken, and may return
/// spuriously, so every caller is a loop that rechecks its condition.
///
/// Not marked nomem. An interrupt taken during the wait runs a handler that
/// writes executive state, so from the point of view of the code around this
/// call, memory does change here. Claiming otherwise lets the optimiser treat
/// a loop whose body is only this call as touching nothing, hoist the loop
/// condition out, and never observe the value the handler wrote.
pub fn wait_for_interrupt() {
    unsafe {
        core::arch::asm!("wfi", options(nostack));
    }
}