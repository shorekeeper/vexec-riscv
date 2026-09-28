//! Timer.
//!
//! One shot, rearmed by the handler. There is no periodic mode in the SBI
//! timer: a deadline is programmed and the interrupt fires once.
//!
//! The tick frequency comes from the device tree and is not knowable here, so
//! it is supplied at init. Nothing in this module assumes a value for it.

use core::sync::atomic::{AtomicU64, Ordering};

/// Timer extension, TIME.
const EXT_TIME: usize = 0x5449_4D45;
/// Legacy set_timer. Present on firmware predating the TIME extension.
const EXT_LEGACY_SET_TIMER: usize = 0x00;

/// Ticks per second, as reported by the device tree.
static FREQUENCY: AtomicU64 = AtomicU64::new(0);

/// Records the timer frequency.
///
/// Called once during boot with the timebase-frequency property of the cpus
/// node. Until then every interval conversion returns zero, which is visible
/// rather than wrong by a factor nobody notices.
pub fn init(frequency_hz: u64) {
    FREQUENCY.store(frequency_hz, Ordering::Relaxed);
}

/// Ticks per second.
pub fn frequency() -> u64 {
    FREQUENCY.load(Ordering::Relaxed)
}

/// Current value of the monotonic counter.
///
/// Sedna serialises and restores machine state, so the delta across a restore
/// is arbitrary and possibly enormous. Consumers must tolerate that: a
/// deadline computed before a restore has long expired after it.
pub fn now() -> u64 {
    crate::csr::time_read()
}

/// Converts milliseconds to ticks.
///
/// Saturating, because the product of a large interval and a high frequency
/// overflows and a wrapped deadline is one that fires immediately and forever.
pub fn ms_to_ticks(ms: u64) -> u64 {
    frequency().saturating_mul(ms) / 1000
}

/// Programs the next timer interrupt at an absolute counter value.
///
/// A deadline in the past fires immediately, which is the defined behaviour
/// and is relied on by the tick path after a long pause.
pub fn set_deadline(when: u64) {
    // TIME function 0: set_timer. On rv64 the value fits one register.
    let ret = crate::sbi::raw_call(EXT_TIME, 0, when as usize, 0, 0);
    if ret.error != 0 {
        crate::sbi::raw_call(EXT_LEGACY_SET_TIMER, 0, when as usize, 0, 0);
    }
}

/// Programs the next timer interrupt the given number of ticks from now.
pub fn set_interval(ticks: u64) {
    set_deadline(now().saturating_add(ticks));
}

/// Stops the timer.
///
/// There is no disable in the SBI interface, so the deadline is pushed beyond
/// any reachable counter value.
pub fn clear() {
    set_deadline(u64::MAX);
}