//! Console output.
//!
//! Two paths. Before a port is attached, output goes through the firmware,
//! which works on both target machines and needs no device tree. Afterwards it
//! goes to the port directly.
//!
//! The firmware path stays compiled in rather than being replaced: it is what
//! prints the diagnostic when attaching a port is the thing that went wrong,
//! and on a machine whose tree describes no port it is the only path there is.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Port base address, zero when none is attached.
static UART_BASE: AtomicUsize = AtomicUsize::new(0);
/// Register spacing of the attached port.
static UART_SHIFT: AtomicU32 = AtomicU32::new(0);

/// Directs output to a serial port.
///
/// The port must already be initialised. Called once, after the device tree
/// has been read.
pub fn attach(base: usize, shift: u32) {
    UART_SHIFT.store(shift, Ordering::Relaxed);
    // Base last: it is what the write path tests, so it must not become
    // nonzero before the shift beside it is correct.
    UART_BASE.store(base, Ordering::Relaxed);
}

/// True when output is going to a port rather than to the firmware.
pub fn is_attached() -> bool {
    UART_BASE.load(Ordering::Relaxed) != 0
}

/// Writes bytes to whichever path is current.
pub fn write_bytes(bytes: &[u8]) {
    let base = UART_BASE.load(Ordering::Relaxed);
    if base == 0 {
        crate::sbi::console_write(bytes);
        return;
    }
    let shift = UART_SHIFT.load(Ordering::Relaxed);
    for &byte in bytes {
        // A bare newline leaves the cursor in the same column on a terminal in
        // raw mode, which is how a serial console is normally run.
        if byte == b'\n' {
            unsafe { crate::uart::write_byte(base, shift, b'\r') };
        }
        unsafe { crate::uart::write_byte(base, shift, byte) };
    }
}

/// Sink for formatted output.
pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

/// Writes formatted output to the console.
///
/// Not synchronised. Correct while there is one hart and the kernel is not
/// reentrant; a console object takes over once there is something to serialise
/// against.
pub fn print(args: fmt::Arguments) {
    let _ = Console.write_fmt(args);
}

/// Writes formatted output to the console.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::console::print(format_args!($($arg)*))
    };
}

/// Writes formatted output followed by a newline.
#[macro_export]
macro_rules! kprintln {
    () => {
        $crate::console::print(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::console::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}