//! The executive instance, and the trap path that reaches it.
//!
//! Composition, not policy. The logic is in exec-core, which is free of
//! unsafe; the single instance and the pointer from the trap vector to it are
//! here, because a statically allocated singleton reached from an interrupt
//! cannot be expressed without unsafe.
//!
//! Access goes through `with_executive`, which disables interrupts for the
//! duration of the borrow. That is what makes the reference sound rather than
//! merely convenient: without it, a trap taken while the borrow is live would
//! produce a second live mutable reference to the same object, which is
//! undefined behaviour and is also the licence an optimiser uses to cache a
//! value the handler has since written.
//!
//! The rule that follows and is worth stating on its own: no interrupt service
//! routine and no deferred call is ever invoked while a borrow is held. Both
//! are copied out of the executive first, the borrow is dropped, and only then
//! is the routine called. That is what lets a routine use `with_executive`
//! itself.
//!
//! One hart is assumed throughout. Sedna provides one and the entry code parks
//! any others, so disabling interrupts is sufficient exclusion.
//!
//! Known limitation, stated rather than hidden: deferred calls run with
//! interrupts globally disabled, because the drain happens inside a trap
//! handler and the hardware clears the enable on entry. Device interrupts are
//! delayed by the length of the drain. NT enables them at that point; doing so
//! here means permitting the trap handler to reenter itself, which needs a
//! bound on nesting depth and a stack sized for it.

use core::cell::UnsafeCell;

use exec_abi::{Irql, Status};
use exec_core::dpc::{DpcRoutine, MAX_DRAIN};
use exec_core::queue::Dpc;
use exec_core::fdt::{Fdt, FdtError};
use exec_core::interrupt::Isr;
use exec_core::irql::InterruptController;
use exec_core::platform::Platform;
use exec_core::Executive;
use exec_hal::trap::{Exception, Trap, TrapFrame};
use exec_hal::{console, csr, dtb, interrupt, kprintln, plic, timer, uart};

use exec_abi::esl::WorldId;
use exec_core::object::DeviceObject;

use crate::memory::{self, MapSummary, MemoryError};

/// Wrapper that makes a static of a non Sync type declarable.
///
/// Sound here for the reasons in the module comment, and nowhere else by
/// default.
struct Singleton<T>(UnsafeCell<T>);

unsafe impl<T> Sync for Singleton<T> {}

static EXECUTIVE: Singleton<Executive> = Singleton(UnsafeCell::new(Executive::new()));

/// Runs a closure with exclusive access to the executive.
///
/// Interrupts are disabled on entry and restored on exit, so the borrow cannot
/// overlap with one taken by a trap handler. In a handler they are already
/// disabled and the call is a no-op, which is why one function serves both.
fn with_executive<R>(f: impl FnOnce(&mut Executive) -> R) -> R {
    let were_enabled = csr::interrupts_disable();
    // Exclusive for the length of the call: interrupts are off, there is one
    // hart, and no other path constructs a reference to this object.
    let result = f(unsafe { &mut *EXECUTIVE.0.get() });
    if were_enabled {
        csr::interrupts_enable();
    }
    result
}

/// Applies IRQL to the hardware masks.
///
/// Zero sized: the state is in the registers, not in this object.
pub struct Hal;

impl InterruptController for Hal {
    fn set_mask(&mut self, irql: Irql) {
        interrupt::apply(irql);
    }
}

// ---------------------------------------------------------------- irql

/// Raises the level and returns the previous one.
pub fn raise_irql(to: Irql) -> Result<Irql, Status> {
    with_executive(|exec| exec.irql.raise(&mut Hal, to))
}

/// Lowers the level, running deferred calls on the way down.
///
/// The order matters. The queue is drained while still at DISPATCH, so a
/// routine runs with the guarantees it was written for, and only then does the
/// level fall to the target. Draining after the drop would run DISPATCH code
/// at PASSIVE.
pub fn lower_irql(to: Irql) -> Status {
    let crossing = with_executive(|exec| {
        to < Irql::DISPATCH && exec.irql.current() >= Irql::DISPATCH
    });

    if crossing {
        let status = with_executive(|exec| exec.irql.lower(&mut Hal, Irql::DISPATCH));
        if !status.is_ok() {
            return status;
        }
        drain_dpcs();
    }

    with_executive(|exec| exec.irql.lower(&mut Hal, to))
}

/// Runs every pending deferred call.
///
/// Each routine is called with no borrow held, so it may reach the executive
/// itself. A routine may queue further calls and those run in the same drain,
/// which terminates only because a routine that queues unconditionally is a
/// defect; the bound turns that defect into a diagnosable stall rather than a
/// hang.
fn drain_dpcs() {
    let mut ran = 0;
    while ran < MAX_DRAIN {
        let next = with_executive(|exec| exec.dpcs.pop());
        match next {
            Some(dpc) => {
                // Safety: the address was produced by casting a DpcRoutine in
                // queue_dpc, which is the only way an entry reaches the queue.
                // The signature is fixed by the type, so the cast back is the
                // exact inverse of the cast out.
                let routine: DpcRoutine = unsafe { core::mem::transmute(dpc.routine) };
                routine(dpc.context);
                ran += 1;
            }
            None => return,
        }
    }
    kprintln!("dpc drain hit its bound, {} calls", ran);
}

/// Queues a deferred call.
///
/// The routine is stored as an address. Reversed by drain_dpcs, which is the
/// only reader.
pub fn queue_dpc(routine: DpcRoutine, context: usize) -> Status {
    let entry = Dpc {
        routine: routine as *const () as usize,
        context,
    };
    with_executive(|exec| {
        if exec.dpcs.push(entry) {
            Status::Success
        } else {
            Status::NoResources
        }
    })
}

// ---------------------------------------------------------------- probe

/// Why the machine could not be described.
#[derive(Clone, Copy, Debug)]
pub enum ProbeError {
    /// No blob at the address the firmware gave.
    NotFound,
    /// The blob is not a device tree this parser accepts.
    Parse(FdtError),
    /// The tree does not state the timer frequency, so no interval can be
    /// computed and the scheduler has no clock.
    NoTimebase,
    /// The tree describes no usable memory.
    NoMemory,
}

/// Reads the device tree and records what it describes.
///
/// Called before anything that depends on the machine and before interrupts
/// are enabled. Failure is returned rather than fatal: the caller decides
/// whether a machine it cannot describe is one it can run on.
pub fn probe(dtb_address: usize) -> Result<(), ProbeError> {
    let blob = dtb::locate(dtb_address).ok_or(ProbeError::NotFound)?;
    let fdt = Fdt::new(blob).map_err(ProbeError::Parse)?;
    let platform = Platform::from_fdt(&fdt).map_err(ProbeError::Parse)?;

    if platform.timebase_hz == 0 {
        return Err(ProbeError::NoTimebase);
    }
    if platform.memory_count == 0 {
        return Err(ProbeError::NoMemory);
    }

    timer::init(platform.timebase_hz);
    with_executive(|exec| exec.platform = platform);

    let base = blob.as_ptr() as u64;
    let size = blob.len() as u64;
    with_executive(|exec| {
        let platform = exec.platform.clone_description();
        memory::init_allocator(&mut exec.pmm, &platform, base, size);
    });

    Ok(())
}

/// Builds the kernel page tables and installs them.
///
/// Called after probe and before interrupts are enabled. A failure leaves
/// translation off, which is a slower and less protected machine rather than
/// a stopped one.
pub fn init_paging() -> Result<MapSummary, MemoryError> {
    let platform = with_executive(|exec| exec.platform.clone_description());

    // The layer comes up before anything is mapped, because the map is the
    // first thing it has an opinion about.
    with_executive(|exec| {
        let ram = if exec.platform.memory_count > 0 {
            exec.platform.memory[0]
        } else {
            exec_core::platform::MemoryRegion::NONE
        };
        exec.esl.init(crate::ESL_MODE, ram.base, ram.size);

        let layout = memory::image_layout();
        exec.esl.set_own_region(layout.start, layout.end - layout.start);
    });

    let result =
        with_executive(|exec| memory::build_and_activate(&mut exec.esl, &mut exec.pmm, &platform));
    if let Ok(summary) = result {
        with_executive(|exec| exec.page_root = summary.root);
    }
    result
}

/// Free physical pages.
///
/// Unused until something allocates after boot. Kept because the number is
/// what a diagnostic asks for first when a machine runs out of memory.
#[allow(dead_code)]
pub fn free_pages() -> usize {
    with_executive(|exec| exec.pmm.free_pages())
}

/// Prints what the machine turned out to be.
pub fn report() {
    with_executive(|exec| {
        let p = &exec.platform;

        kprintln!("platform");
        kprintln!("  timebase  {} Hz", p.timebase_hz);
        kprintln!("  harts     {}, booted on {}", p.hart_count, p.boot_hart);
        kprintln!(
            "  memory    {} region(s), {} MiB total",
            p.memory_count,
            p.memory_total() / (1024 * 1024)
        );
        let mut i = 0;
        while i < p.memory_count {
            let r = p.memory[i];
            kprintln!("            {:#012x}..{:#012x}", r.base, r.end());
            i += 1;
        }

        if p.reserved_count != 0 {
            kprintln!("  reserved  {} region(s)", p.reserved_count);
            let mut i = 0;
            while i < p.reserved_count {
                let r = p.reserved[i];
                kprintln!("            {:#012x}..{:#012x}", r.base, r.end());
                i += 1;
            }
        }

        if p.plic.is_present() {
            kprintln!("  plic      {:#012x} size {:#x}", p.plic.base, p.plic.size);
        } else {
            kprintln!("  plic      absent");
        }

        if p.uart.device.is_present() {
            kprintln!(
                "  uart      {:#012x} irq {} shift {} clock {} Hz",
                p.uart.device.mmio.base,
                p.uart.device.irq,
                p.uart.reg_shift,
                p.uart.clock_hz
            );
        } else {
            kprintln!("  uart      absent");
        }

        if p.virtio_count != 0 {
            let first = p.virtio[0];
            let last = p.virtio[p.virtio_count - 1];
            kprintln!(
                "  virtio    {} transports, {:#x} irq {} .. {:#x} irq {}",
                p.virtio_count,
                first.mmio.base,
                first.irq,
                last.mmio.base,
                last.irq
            );
        } else {
            kprintln!("  virtio    none");
        }

        if p.unknown_nodes != 0 {
            kprintln!("  unknown   {} device node(s) not recognised", p.unknown_nodes);
        }
    });
}

// ---------------------------------------------------------------- init

/// Controller priority the console runs at.
///
/// Low. A serial port is the least urgent thing on the machine, and a higher
/// value would let typing preempt a device that matters.
const CONSOLE_PRIORITY: u8 = 1;

/// Brings up the interrupt controller and the console port.
///
/// Returns whether the console is now on its own port. A machine whose tree
/// describes neither keeps the firmware path, which is correct and slower.
pub fn init_devices() -> bool {
    let (plic_base, boot_hart, uart) =
        with_executive(|exec| (exec.platform.plic, exec.platform.boot_hart, exec.platform.uart));

    if plic_base.is_present() {
        // Safety: the address came from the device tree and names the
        // controller block.
        // The tree gives addresses as u64 regardless of the width of the
        // machine. Lossless on rv64, where usize is also 64 bits.
        unsafe {
            plic::init(
                plic_base.base as usize,
                plic::supervisor_context(boot_hart),
            )
        };
    }

    if !uart.device.is_present() {
        return false;
    }

    let base = uart.device.mmio.base as usize;
    // Safety: the address came from the device tree and the node declared
    // itself compatible with a 16550.
    unsafe { uart::init(base, uart.reg_shift) };

    if plic_base.is_present() && uart.device.irq != 0 {
        let status = with_executive(|exec| {
            exec.interrupts
                .register(uart.device.irq, console_isr, base, CONSOLE_PRIORITY)
        });
        if status.is_ok() {
            unsafe { plic::enable_source(uart.device.irq, CONSOLE_PRIORITY) };
        } else {
            kprintln!("console interrupt not registered: {:?}", status);
        }
    }

    // Output moves to the port only after it is configured, so a failure above
    // leaves the firmware path in place and the diagnostic readable.
    console::attach(base, uart.reg_shift);
    true
}

/// Registers an interrupt handler and admits its source.
///
/// Unused while the console is the only device with a driver, which registers
/// through init_devices. This is the entry point a loaded driver uses.
#[allow(dead_code)]
pub fn register_interrupt(source: u32, isr: Isr, context: usize, priority: u8) -> Status {
    let status = with_executive(|exec| exec.interrupts.register(source, isr, context, priority));
    if !status.is_ok() {
        return status;
    }
    // Safety: init has run if a controller was described; the call is a no-op
    // otherwise.
    if unsafe { !plic::enable_source(source, priority) } {
        with_executive(|exec| exec.interrupts.unregister(source));
        return Status::DeviceError;
    }
    Status::Success
}

/// Installs the trap vector and starts the timer.
///
/// `interval_ms` is the scheduler tick. Called once, after the devices are up.
pub fn init(interval_ms: u64) {
    exec_hal::trap::init(on_trap);

    let ticks = timer::ms_to_ticks(interval_ms);
    with_executive(|exec| {
        exec.tick_interval = ticks;
        exec.irql.sync(&mut Hal);
    });

    timer::set_interval(ticks);
    interrupt::enable();
}

// ---------------------------------------------------------------- state

/// Timer interrupts taken.
pub fn ticks() -> u64 {
    with_executive(|exec| exec.ticks)
}

/// Deferred calls dropped for want of queue space.
pub fn dpc_overflows() -> u64 {
    with_executive(|exec| exec.dpcs.dropped())
}

/// Interrupts claimed with no handler registered.
pub fn spurious_interrupts() -> u64 {
    with_executive(|exec| exec.interrupts.spurious())
}

/// Console bytes dropped because the ring was full.
pub fn console_dropped() -> u64 {
    with_executive(|exec| exec.console_rx.dropped())
}

/// Takes one received console byte.
pub fn console_read() -> Option<u8> {
    with_executive(|exec| exec.console_rx.pop())
}

/// Waits for the next interrupt.
///
/// Holds no borrow across the wait, which is what makes the wait a point at
/// which a handler may run.
pub fn idle() {
    exec_hal::wait_for_interrupt();
}

// ---------------------------------------------------------------- traps

/// Trap handler.
///
/// Runs with interrupts disabled at the level the trap was taken from.
fn on_trap(frame: &mut TrapFrame) {
    match Trap::decode(frame.scause) {
        Trap::TimerInterrupt => on_timer(),
        Trap::ExternalInterrupt => on_external(),
        Trap::SoftwareInterrupt => {
            // Nothing sends these yet. Acknowledging one means clearing the
            // pending bit in sip, which belongs in the HAL; until there is a
            // sender, arriving here is a defect worth reporting.
            kprintln!("unexpected software interrupt");
        }
        Trap::UnknownInterrupt(code) => {
            kprintln!("unknown interrupt, cause {}", code);
        }
        Trap::Exception(exception) => on_exception(exception, frame),
    }
}

/// Timer interrupt.
///
/// Rearms the timer, counts the tick, and lowers back through DISPATCH, which
/// is where any deferred calls queued since the last one get to run.
fn on_timer() {
    let interval = with_executive(|exec| {
        exec.ticks += 1;
        exec.tick_interval
    });
    timer::set_interval(interval);

    // Raising fails only on an invalid level or a current level above the
    // target, neither of which can hold here.
    if let Ok(previous) = raise_irql(Irql::DISPATCH) {
        lower_irql(previous);
    }
}

/// External interrupt.
///
/// Claims sources until the controller has none left. One interrupt can stand
/// for several pending sources, and stopping after the first would leave the
/// rest to fire again immediately.
fn on_external() {
    if !plic::is_present() {
        kprintln!("external interrupt with no controller");
        return;
    }

    // Cannot exceed the number of sources: each claim removes one from the
    // pending set and nothing here can put one back.
    let mut claims = 0;
    while claims <= plic::MAX_SOURCES {
        let source = plic::claim();
        if source == 0 {
            return;
        }
        dispatch_source(source);
        // Completed even when no handler ran. Without it the source never
        // fires again, so a single unexpected interrupt would silence a device
        // permanently.
        plic::complete(source);
        claims += 1;
    }
    kprintln!("external interrupt claim loop hit its bound");
}

/// Runs the handler for one source at its device level.
fn dispatch_source(source: u32) {
    let entry = with_executive(|exec| exec.interrupts.lookup(source));

    let entry = match entry {
        Some(entry) => entry,
        None => {
            kprintln!("interrupt {} has no handler", source);
            return;
        }
    };

    let isr = match entry.isr {
        Some(isr) => isr,
        None => return,
    };

    // The borrow is dropped before the routine is called, so the routine can
    // reach the executive itself.
    match raise_irql(entry.irql()) {
        Ok(previous) => {
            isr(entry.context);
            lower_irql(previous);
        }
        Err(_) => {
            // Already at or above this device's level, which means the
            // controller admitted a source it should have masked.
            kprintln!("interrupt {} arrived above its own level", source);
        }
    }
}

/// Synchronous exception.
///
/// Every case is fatal for now. A page fault becomes recoverable once there
/// are address spaces; an ecall becomes the system call path once there are
/// tasks. Until then, continuing past one would be guessing.
fn on_exception(exception: Exception, frame: &mut TrapFrame) -> ! {
    kprintln!();
    kprintln!("exception: {:?}", exception);
    kprintln!("  sepc    {:#018x}", frame.sepc);
    kprintln!("  stval   {:#018x}", frame.stval);
    kprintln!("  sstatus {:#018x}", frame.sstatus);
    kprintln!("  ra      {:#018x}", frame.ra);
    kprintln!("  sp      {:#018x}", frame.sp);
    exec_hal::park()
}

// ---------------------------------------------------------------- console

/// Console interrupt service routine.
///
/// Runs at the console device level. Drains the port into the ring and queues
/// the work, which is the whole of what a service routine should do: the port
/// has a short FIFO and stops accepting input while it is full, so emptying it
/// is urgent and looking at the contents is not.
///
/// `context` is the port base address.
fn console_isr(context: usize) {
    let shift = with_executive(|exec| exec.platform.uart.reg_shift);
    let mut received = 0;

    // Bounded by more than the deepest FIFO a 16550 has, so a port that
    // reports data forever cannot hold the level.
    while received < 64 {
        // Safety: the base came from the table entry, which was registered
        // with the address the tree gave for an initialised port.
        let byte = match unsafe { uart::read_byte(context, shift) } {
            Some(byte) => byte,
            None => break,
        };
        with_executive(|exec| exec.console_rx.push(byte));
        received += 1;
    }

    if received != 0 {
        queue_dpc(console_dpc, received);
    }
}

/// Handler for received console bytes, set by the image.
///
/// A function pointer rather than a call, so the policy of what typing does
/// lives with the code that has an opinion about it.
static mut CONSOLE_SINK: Option<fn(u8)> = None;

/// Sets what happens to a received byte.
///
/// Called once during boot, before interrupts are enabled.
pub fn set_console_sink(sink: fn(u8)) {
    // Written once before any interrupt can read it.
    unsafe { CONSOLE_SINK = Some(sink) };
}

/// Deferred work for received console bytes.
///
/// Runs at DISPATCH. Takes bytes out of the ring one at a time rather than
/// holding the executive for the length of the loop, so an interrupt service
/// routine can add to it while it drains.
fn console_dpc(_context: usize) {
    let sink = unsafe { CONSOLE_SINK };
    let sink = match sink {
        Some(sink) => sink,
        None => return,
    };
    while let Some(byte) = console_read() {
        sink(byte);
    }
}

// ---------------------------------------------------------------- objects

/// Registers the devices the tree described.
///
/// One object per device the kernel has a driver for, named the way NT names
/// them. The names are what a later loader resolves a driver against, and what
/// a diagnostic prints instead of an address.
pub fn init_objects() {
    with_executive(|exec| {
        let uart = exec.platform.uart;
        if uart.device.is_present() {
            let device = DeviceObject {
                driver: exec_abi::HANDLE_NULL,
                mmio_base: uart.device.mmio.base,
                irq: uart.device.irq,
                context: uart.reg_shift as usize,
            };
            let _ = exec.objects.create_device(b"\\Device\\Serial0", device);
        }

        let mut i = 0;
        while i < exec.platform.virtio_count && i < 10 {
            let transport = exec.platform.virtio[i];
            let device = DeviceObject {
                driver: exec_abi::HANDLE_NULL,
                mmio_base: transport.mmio.base,
                irq: transport.irq,
                context: i,
            };
            // Names are ascii, and the index is one digit by the bound above.
            let name = [
                b'\\', b'D', b'e', b'v', b'i', b'c', b'e', b'\\',
                b'V', b'i', b'r', b't', b'i', b'o', b'0' + i as u8,
            ];
            let _ = exec.objects.create_device(&name, device);
            i += 1;
        }
    });
}

/// Prints the namespace.
pub fn report_objects() {
    with_executive(|exec| {
        let (devices, drivers) = exec.objects.counts();
        kprintln!("objects");
        kprintln!("  live      {} device(s), {} driver(s)", devices, drivers);

        exec.objects.names.for_each(&mut |path, handle| {
            // A directory refers to nothing and exists to hold other names.
            if handle.is_null() {
                return;
            }
            let mut text = [0u8; 48];
            let mut n = 0;
            while n < path.len() && n < text.len() {
                text[n] = path[n];
                n += 1;
            }
            let name = core::str::from_utf8(&text[..n]).unwrap_or("<bad>");
            kprintln!(
                "  {:<24} handle {:#x}  slot {} gen {}",
                name,
                handle.0,
                handle.index(),
                handle.generation()
            );
        });
    });
}

/// Demonstrates that a released handle does not come back.
///
/// The property the object table is built around, shown rather than asserted:
/// a handle is opened, resolved, released, and resolved again. The second
/// resolution fails, and it fails because the generation moved, not because the
/// slot is empty. The slot is then reused, and the old handle still fails.
pub fn demonstrate_handles() {
    with_executive(|exec| {
        let probe = DeviceObject {
            driver: exec_abi::HANDLE_NULL,
            mmio_base: 0xdead_0000,
            irq: 0,
            context: 0,
        };

        let handle = match exec.objects.create_device(b"\\Device\\Probe", probe) {
            Ok(handle) => handle,
            Err(status) => {
                kprintln!("  could not create the probe object: {:?}", status);
                return;
            }
        };

        kprintln!("handles");
        kprintln!(
            "  opened    {:#x}  slot {} gen {}",
            handle.0,
            handle.index(),
            handle.generation()
        );
        kprintln!(
            "  resolves  {}",
            exec.objects.device(handle).is_some()
        );

        let _ = exec.objects.names.remove(b"\\Device\\Probe");
        exec.objects.release(handle);
        kprintln!(
            "  released, resolves now {}",
            exec.objects.device(handle).is_some()
        );

        // The slot is free, so the next object takes it. The generation has
        // moved on, so the handle above names nothing even though its index is
        // once again in use.
        let second = DeviceObject {
            driver: exec_abi::HANDLE_NULL,
            mmio_base: 0xbeef_0000,
            irq: 0,
            context: 0,
        };
        if let Ok(reused) = exec.objects.create_device(b"\\Device\\Probe", second) {
            kprintln!(
                "  reused    {:#x}  slot {} gen {}",
                reused.0,
                reused.index(),
                reused.generation()
            );
            kprintln!(
                "  old handle resolves {}, same slot",
                exec.objects.device(handle).is_some()
            );
            let _ = exec.objects.names.remove(b"\\Device\\Probe");
            exec.objects.release(reused);
        }
    });
}

// ---------------------------------------------------------------- esl

/// Prints what the layer is doing.
pub fn report_esl() {
    with_executive(|exec| {
        let stats = exec.esl.stats();
        kprintln!("esl");
        kprintln!("  mode      {:?}", exec.esl.mode());
        kprintln!(
            "  mappings  {} made, {} refused",
            stats.mapped,
            stats.refused
        );
        kprintln!("  sealed    {} region(s)", exec.esl.sealed_count());

        let mut i = 0;
        while i < exec.esl.sealed_count() {
            if let Some(region) = exec.esl.sealed_at(i) {
                kprintln!(
                    "            {:#012x}..{:#012x}  measure {:#018x}",
                    region.base,
                    region.base + region.size,
                    region.measurement
                );
            }
            i += 1;
        }
    });
}

/// Exercises the window check.
///
/// The property that isolation rests on where there is no IOMMU, shown against
/// the addresses this machine actually has. Three cases: a buffer inside the
/// window, one that starts inside and runs past the end, and one whose length
/// would wrap the address space.
pub fn demonstrate_windows() {
    with_executive(|exec| {
        let ram = if exec.platform.memory_count > 0 {
            exec.platform.memory[0]
        } else {
            return;
        };

        kprintln!("windows");

        let inside = ram.base + 0x10_0000;
        kprintln!(
            "  inside    {:#x} + 4096      {}",
            inside,
            exec.esl.window_check(WorldId::KERNEL, inside, 4096, true)
        );

        // Starts in the window and ends past it. The check that a naive
        // implementation gets right.
        let straddle = ram.end() - 2048;
        kprintln!(
            "  straddle  {:#x} + 4096      {}",
            straddle,
            exec.esl.window_check(WorldId::KERNEL, straddle, 4096, true)
        );

        // A length that wraps. The check a naive implementation gets wrong,
        // because address plus length lands back below the base and compares
        // as if it fit.
        kprintln!(
            "  wrapping  {:#x} + {:#x}  {}",
            inside,
            u64::MAX - 1024,
            exec.esl.window_check(WorldId::KERNEL, inside, u64::MAX - 1024, true)
        );

        // Below the window entirely.
        kprintln!(
            "  below     {:#x} + 4096       {}",
            0x1000u64,
            exec.esl.window_check(WorldId::KERNEL, 0x1000, 4096, true)
        );
    });
}