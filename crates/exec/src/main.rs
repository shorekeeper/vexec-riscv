//! Kernel image.
//!
//! Holds the boot assembly, the entry point the firmware calls, and the panic
//! path. Everything else lives in exec-core and exec-hal.

#![no_std]
#![no_main]
#![deny(missing_docs)]

mod kernel;
mod memory;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use exec_abi::EslMode;
use exec_hal::{console, kprint, kprintln, park};

// Boot code is included rather than assembled separately: global_asm takes the
// source directly, so there is no build script and no external assembler.
core::arch::global_asm!(include_str!("../../../boot/entry.S"));
core::arch::global_asm!(include_str!("../../../boot/trap.S"));

/// ESL configuration this image was built for.
///
/// A constant for now. It becomes a build feature once ESL exists, and the
/// value is recorded in the image header so a loader can refuse a mismatch.
pub const ESL_MODE: EslMode = EslMode::Integrity;

/// Scheduler tick, in milliseconds.
const TICK_MS: u64 = 100;

/// Ticks before the exercise gives up waiting for input.
const MAX_TICKS: u64 = 300;

/// Set by the console sink when the operator asks to stop.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" {
    static __image_start: u8;
    static __image_end: u8;
    static __bss_start: u8;
    static __bss_end: u8;
}

/// Entry point called from boot assembly.
///
/// `hart` is the hart id, guaranteed zero here because the entry code parks
/// every other hart. `dtb` is the physical address of the flattened device
/// tree, which the firmware places somewhere in RAM and does not free.
#[no_mangle]
pub extern "C" fn exec_start(hart: usize, dtb: usize) -> ! {
    // Addresses come from linker symbols, so they are read as addresses and
    // never dereferenced.
    let image_start = unsafe { &__image_start as *const u8 as usize };
    let image_end = unsafe { &__image_end as *const u8 as usize };
    let bss_start = unsafe { &__bss_start as *const u8 as usize };
    let bss_end = unsafe { &__bss_end as *const u8 as usize };

    kprintln!();
    kprintln!("vExec 0.0.0");
    kprintln!("  hart      {}", hart);
    kprintln!("  dtb       {:#x}", dtb);
    kprintln!(
        "  image     {:#x}..{:#x}  ({} KiB)",
        image_start,
        image_end,
        (image_end - image_start) / 1024
    );
    kprintln!(
        "  bss       {:#x}..{:#x}  ({} KiB)",
        bss_start,
        bss_end,
        (bss_end - bss_start) / 1024
    );
    kprintln!("  esl       {:?}", ESL_MODE);
    kprintln!(
        "  sbi       console {}",
        if exec_hal::sbi::has_dbcn() {
            "dbcn"
        } else {
            "legacy putchar"
        }
    );
    kprintln!("  core      {}", exec_core::banner());
    kprintln!();

    // Nothing that depends on the machine may run before this. A kernel that
    // cannot describe the machine has no timer frequency and no memory map,
    // and guessing either produces a system that appears to work.
    if let Err(error) = kernel::probe(dtb) {
        kprintln!("cannot describe this machine: {:?}", error);
        exec_hal::sbi::shutdown();
        park();
    }

    kernel::report();

    // Translation before devices. The map covers the device registers, so a
    // driver initialised first would be touching addresses that are about to
    // change meaning.
    match kernel::init_paging() {
        Ok(summary) => {
            memory::report(&summary);
            kprintln!();
        }
        Err(error) => {
            kprintln!("paging not enabled: {:?}", error);
            kprintln!("continuing with translation off");
            kprintln!();
        }
    }
    
    let on_uart = kernel::init_devices();
    kprintln!(
        "  console   {}",
        if on_uart {
            "serial port, interrupt driven receive"
        } else {
            "firmware, no receive"
        }
    );
    kprintln!("  tick      {} ms", TICK_MS);
    kprintln!();

    kernel::report_esl();
    kprintln!();
    kernel::demonstrate_windows();
    kprintln!();

    kernel::init_objects();
    kernel::report_objects();
    kprintln!();
    kernel::demonstrate_handles();
    kprintln!();

    kernel::set_console_sink(on_console_byte);
    kernel::init(TICK_MS);

    if on_uart {
        kprintln!("type to echo, q to stop");
    } else {
        kprintln!("no console input on this machine, running {} ticks", MAX_TICKS);
    }
    kprintln!();

    // The condition is checked outside the wait because wfi may return
    // spuriously, and both parts of it are written by an interrupt.
    while !QUIT.load(Ordering::Relaxed) && kernel::ticks() < MAX_TICKS {
        kernel::idle();
    }

    kprintln!();
    kprintln!("stopping");
    kprintln!("  ticks     {}", kernel::ticks());
    kprintln!("  spurious  {}", kernel::spurious_interrupts());
    if kernel::dpc_overflows() != 0 {
        kprintln!("  dpc lost  {}", kernel::dpc_overflows());
    }
    if kernel::console_dropped() != 0 {
        kprintln!("  rx lost   {}", kernel::console_dropped());
    }

    exec_hal::sbi::shutdown();

    // Reached when the firmware has no reset extension. Direct port writes
    // still work, so say so before stopping.
    if console::is_attached() {
        kprintln!("firmware did not reset the machine");
    }
    park()
}

/// What a received byte does.
///
/// Runs at DISPATCH inside a deferred call. Echoing from here rather than from
/// the interrupt is the point of the exercise: the interrupt empties the port
/// and this decides what the bytes mean.
fn on_console_byte(byte: u8) {
    match byte {
        b'q' | b'Q' => {
            kprintln!();
            kprintln!("quit requested");
            QUIT.store(true, Ordering::Relaxed);
        }
        b'\r' | b'\n' => kprintln!(),
        // Printable ASCII.
        0x20..=0x7e => kprint!("{}", byte as char),
        other => kprint!("<{:02x}>", other),
    }
}

/// Panic path.
///
/// Prints and stops. There is nothing to unwind to: panic is abort in every
/// profile, and a kernel that continues past a broken invariant is worse than
/// one that halts.
#[panic_handler]
fn on_panic(info: &PanicInfo) -> ! {
    kprintln!();
    kprintln!("panic: {}", info);
    park()
}