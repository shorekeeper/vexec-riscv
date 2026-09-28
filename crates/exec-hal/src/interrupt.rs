//! Interrupt masking for the IRQL implementation.
//!
//! Implements the controller contract exec-core defines. The executive holds
//! the level and the rules; this holds the register writes, which is the line
//! the Isolated ESL mode also runs along.
//!
//! A level is two masks, not one. The supervisor enable register decides which
//! classes of interrupt reach the hart at all, and the controller threshold
//! decides which devices are among them. Both have to move together or the
//! device range of the level ordering means nothing.

use exec_abi::Irql;

use crate::csr::{self, sint};
use crate::plic;

/// Applies an IRQL.
///
/// ```text
///   PASSIVE     software, timer and external admitted, threshold 0
///   DISPATCH    external only, threshold 0
///   DEVICE(n)   external only, threshold n, masking equal and lower devices
///   HIGH        nothing
/// ```
///
/// The timer is masked from DISPATCH upward because it drives the scheduler,
/// which runs at DISPATCH: a timer interrupt taken by code already there would
/// reenter the scheduler.
///
/// A source at the same priority as the threshold is masked, not admitted, so
/// a device does not preempt itself.
pub fn apply(irql: Irql) {
    let mask = if irql >= Irql::HIGH {
        0
    } else if irql >= Irql::DISPATCH {
        sint::SEI
    } else {
        sint::SSI | sint::STI | sint::SEI
    };
    csr::sie_write(mask);

    // Zero below the device range, which admits every enabled source.
    plic::set_threshold(irql.plic_priority());
}

/// Enables interrupts globally. Returns the previous state.
pub fn enable() -> bool {
    csr::interrupts_enable()
}

/// Disables interrupts globally. Returns the previous state.
pub fn disable() -> bool {
    csr::interrupts_disable()
}