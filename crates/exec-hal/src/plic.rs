//! Platform level interrupt controller.
//!
//! Routes device interrupts to a hart. Register layout, all offsets from the
//! base address in the device tree:
//!
//! ```text
//!   0x000000 + 4*source            priority of a source, 0 disables it
//!   0x001000 + 4*(source/32)       pending bits, read only
//!   0x002000 + 0x80*ctx + 4*(s/32) enable bits for one context
//!   0x200000 + 0x1000*ctx          priority threshold for one context
//!   0x200000 + 0x1000*ctx + 4      claim on read, complete on write
//! ```
//!
//! A context is a hart and privilege level pair. The threshold masks every
//! source whose priority is at or below it, which is what makes the device
//! range of IRQL real rather than notional.
//!
//! The base and context are held in statics because the IRQL path writes the
//! threshold and has nowhere to carry them from: raising IRQL happens in code
//! that knows nothing about the machine.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use exec_abi::Irql;

use crate::mmio;

const PRIORITY_BASE: usize = 0x0000;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_STRIDE: usize = 0x80;
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const THRESHOLD_OFFSET: usize = 0x0;
const CLAIM_OFFSET: usize = 0x4;

/// Sources the controller is driven with.
///
/// Thirty two is what the smaller of the two controllers implements. Its
/// decoder masks a source number down into that range rather than ignoring it,
/// so a write past the end lands on a source that exists and silently changes
/// it. The enable bitmap is one word wide at this size, which is the part that
/// matters: a second word is past the end of the array the controller keeps.
///
/// A source above this is claimed, reported and completed without a handler,
/// so an unexpected one cannot wedge the machine.
pub const MAX_SOURCES: u32 = 32;

/// Base address, zero until init.
static BASE: AtomicUsize = AtomicUsize::new(0);
/// Context this hart uses.
static CONTEXT: AtomicU32 = AtomicU32::new(0);

/// Supervisor context for a hart.
///
/// By convention the contexts of hart h are 2h for machine mode and 2h+1 for
/// supervisor mode. The tree states this properly in interrupts-extended on
/// the controller node, as a list of phandle and cause pairs in context order.
/// Reading that needs phandle resolution, which this parser does not do; the
/// convention holds on both target machines.
pub const fn supervisor_context(hart: u32) -> u32 {
    2 * hart + 1
}

/// Prepares the controller.
///
/// Every source is silenced and every enable bit cleared before anything is
/// registered: the firmware ran first and may have left sources on, and one
/// that fires with no handler is a diagnostic at best.
///
/// # Safety
///
/// `base` must be the controller register block from the device tree.
pub unsafe fn init(base: usize, context: u32) {
    BASE.store(base, Ordering::Relaxed);
    CONTEXT.store(context, Ordering::Relaxed);

    let mut source = 1;
    while source < MAX_SOURCES {
        mmio::write32(base + PRIORITY_BASE + 4 * source as usize, 0);
        source += 1;
    }

    // Enable bits are one per source, so MAX_SOURCES bits is MAX_SOURCES/32
    // words.
    let enable = base + ENABLE_BASE + ENABLE_STRIDE * context as usize;
    let mut word = 0;
    while word < (MAX_SOURCES as usize) / 32 {
        mmio::write32(enable + 4 * word, 0);
        word += 1;
    }

    // Threshold zero admits everything that is enabled. IRQL raises it.
    mmio::write32(
        base + CONTEXT_BASE + CONTEXT_STRIDE * context as usize + THRESHOLD_OFFSET,
        0,
    );
}

/// True when a controller has been prepared.
pub fn is_present() -> bool {
    BASE.load(Ordering::Relaxed) != 0
}

/// Admits a source at a priority.
///
/// Priority zero would leave the source silent, so it is rejected: a caller
/// asking for it means a mapping went wrong somewhere earlier.
///
/// # Safety
///
/// Requires init to have run.
pub unsafe fn enable_source(source: u32, priority: u8) -> bool {
    let base = BASE.load(Ordering::Relaxed);
    // A priority above what the controller implements is refused rather than
    // clamped. Clamping would make two levels indistinguishable at the
    // hardware while the executive still treats them as ordered.
    if base == 0
        || source == 0
        || source >= MAX_SOURCES
        || priority == 0
        || priority > Irql::MAX_PRIORITY
    {
        return false;
    }
    let context = CONTEXT.load(Ordering::Relaxed) as usize;

    mmio::write32(base + PRIORITY_BASE + 4 * source as usize, priority as u32);

    let word = base + ENABLE_BASE + ENABLE_STRIDE * context + 4 * (source as usize / 32);
    let bit = 1u32 << (source % 32);
    mmio::write32(word, mmio::read32(word) | bit);
    true
}

/// Silences a source.
///
/// # Safety
///
/// Requires init to have run.
pub unsafe fn disable_source(source: u32) {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 || source == 0 || source >= MAX_SOURCES {
        return;
    }
    let context = CONTEXT.load(Ordering::Relaxed) as usize;

    let word = base + ENABLE_BASE + ENABLE_STRIDE * context + 4 * (source as usize / 32);
    let bit = 1u32 << (source % 32);
    mmio::write32(word, mmio::read32(word) & !bit);
    mmio::write32(base + PRIORITY_BASE + 4 * source as usize, 0);
}

/// Masks every source at or below a priority.
///
/// Called from the IRQL path. A value above the highest priority the hardware
/// implements masks everything, which is what the top of the range means.
pub fn set_threshold(priority: u8) {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }
    let context = CONTEXT.load(Ordering::Relaxed) as usize;
    unsafe {
        mmio::write32(
            base + CONTEXT_BASE + CONTEXT_STRIDE * context + THRESHOLD_OFFSET,
            priority as u32,
        );
    }
}

/// Takes the highest priority pending source, or zero when there is none.
///
/// The read is the acknowledgement: the source stops being pending and does
/// not become so again until completed. A claim of zero means the interrupt
/// went away before it was claimed, which happens and is not an error.
pub fn claim() -> u32 {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    let context = CONTEXT.load(Ordering::Relaxed) as usize;
    unsafe { mmio::read32(base + CONTEXT_BASE + CONTEXT_STRIDE * context + CLAIM_OFFSET) }
}

/// Reports a source as handled.
///
/// Until this is written the source cannot fire again. A handler that returns
/// without completing silences its own device permanently, which is why the
/// dispatcher completes even for a source it found no handler for.
pub fn complete(source: u32) {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }
    let context = CONTEXT.load(Ordering::Relaxed) as usize;
    unsafe {
        mmio::write32(
            base + CONTEXT_BASE + CONTEXT_STRIDE * context + CLAIM_OFFSET,
            source,
        );
    }
}