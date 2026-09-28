//! Supervisor Binary Interface.
//!
//! Reaches the firmware in M mode. Available directly only while the executive
//! runs in S, that is in the Off and Integrity ESL modes. In the Isolated mode
//! these calls are proxied and filtered by ESL, and this module is compiled
//! into ESL rather than into the executive.

use core::arch::asm;
use core::sync::atomic::{AtomicU8, Ordering};

/// Base extension. Present in every conforming implementation.
const EXT_BASE: usize = 0x10;
/// Debug console extension, DBCN.
const EXT_DBCN: usize = 0x4442_434E;
/// System reset extension, SRST.
const EXT_SRST: usize = 0x5352_5354;
/// Legacy console putchar. Deprecated but universally implemented.
const EXT_LEGACY_PUTCHAR: usize = 0x01;

/// Return value of an SBI call.
pub struct SbiRet {
    /// Zero on success, negative otherwise.
    pub error: isize,
    /// Meaning depends on the call.
    pub value: usize,
}

/// Issues an SBI call.
///
/// Public because the timer module builds its own calls; there is no reason to
/// duplicate the inline assembly for each extension.
#[inline]
pub fn raw_call(eid: usize, fid: usize, a0: usize, a1: usize, a2: usize) -> SbiRet {
    let error: isize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => error,
            inlateout("a1") a1 => value,
            in("a2") a2,
            in("a6") fid,
            in("a7") eid,
            options(nostack)
        );
    }
    SbiRet { error, value }
}

/// Reports whether an extension is implemented.
pub fn probe_extension(eid: usize) -> bool {
    // Base extension, function 3: probe_extension. Returns nonzero when the
    // extension is available.
    let ret = raw_call(EXT_BASE, 3, eid, 0, 0);
    ret.error == 0 && ret.value != 0
}

/// Which console path this firmware supports. Resolved once, because probing
/// costs an ecall and the answer cannot change while the machine runs.
///
/// 0 unresolved, 1 DBCN, 2 legacy.
static CONSOLE_PATH: AtomicU8 = AtomicU8::new(0);

fn console_path() -> u8 {
    let cached = CONSOLE_PATH.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let path = if probe_extension(EXT_DBCN) { 1 } else { 2 };
    CONSOLE_PATH.store(path, Ordering::Relaxed);
    path
}

/// True when the firmware implements the debug console extension.
///
/// Worth reporting at boot: the fallback is one ecall per byte, and on an
/// interpreted machine that is the difference between a console and a delay.
pub fn has_dbcn() -> bool {
    console_path() == 1
}

/// Writes one byte to the firmware console.
///
/// The debug console extension is preferred and the legacy call is the
/// fallback, because Sedna and QEMU may ship different OpenSBI versions and
/// the early boot path has to produce output on both.
pub fn console_put(byte: u8) {
    match console_path() {
        // DBCN function 2: console_write_byte.
        1 => {
            raw_call(EXT_DBCN, 2, byte as usize, 0, 0);
        }
        // Legacy extension, function 0, argument in a0.
        _ => {
            raw_call(EXT_LEGACY_PUTCHAR, 0, byte as usize, 0, 0);
        }
    }
}

/// Writes a byte slice to the firmware console.
///
/// DBCN takes a physical address and a length, which is one call rather than
/// one per byte. Relevant on Sedna, where each ecall is interpreted.
pub fn console_write(bytes: &[u8]) {
    if console_path() == 1 {
        // DBCN function 0: console_write. Arguments are length, address low,
        // address high. Addressing is physical and satp is zero during boot,
        // so the pointer is usable as is.
        let addr = bytes.as_ptr() as usize;
        raw_call(EXT_DBCN, 0, bytes.len(), addr, 0);
        return;
    }
    for &b in bytes {
        console_put(b);
    }
}

/// Requests that the machine shut down.
///
/// Returns if the firmware does not implement the reset extension, which is
/// why the caller must have somewhere to go afterwards.
pub fn shutdown() {
    // SRST function 0: system_reset. Type 0 is shutdown, reason 0 is none.
    raw_call(EXT_SRST, 0, 0, 0, 0);
}