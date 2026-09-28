//! Trap entry and cause decoding.
//!
//! The frame layout here is the contract with boot/trap.S. Both sides are
//! fixed and the size is asserted below; a mismatch is a silently corrupted
//! register file, so it is caught at compile time rather than at run time.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Saved machine state for one trap.
///
/// Field order is the slot order in boot/trap.S. x0 is not saved because it
/// is hardwired to zero.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    /// x1, return address.
    pub ra: usize,
    /// x2, stack pointer as it was before the frame was pushed.
    pub sp: usize,
    /// x3, global pointer.
    pub gp: usize,
    /// x4, thread pointer.
    pub tp: usize,
    /// x5.
    pub t0: usize,
    /// x6.
    pub t1: usize,
    /// x7.
    pub t2: usize,
    /// x8, frame pointer.
    pub s0: usize,
    /// x9.
    pub s1: usize,
    /// x10, first argument and first return value.
    pub a0: usize,
    /// x11.
    pub a1: usize,
    /// x12.
    pub a2: usize,
    /// x13.
    pub a3: usize,
    /// x14.
    pub a4: usize,
    /// x15.
    pub a5: usize,
    /// x16.
    pub a6: usize,
    /// x17.
    pub a7: usize,
    /// x18.
    pub s2: usize,
    /// x19.
    pub s3: usize,
    /// x20.
    pub s4: usize,
    /// x21.
    pub s5: usize,
    /// x22.
    pub s6: usize,
    /// x23.
    pub s7: usize,
    /// x24.
    pub s8: usize,
    /// x25.
    pub s9: usize,
    /// x26.
    pub s10: usize,
    /// x27.
    pub s11: usize,
    /// x28.
    pub t3: usize,
    /// x29.
    pub t4: usize,
    /// x30.
    pub t5: usize,
    /// x31.
    pub t6: usize,
    /// Address the trap returns to. Writable: a handler that resumes past a
    /// faulting instruction advances this.
    pub sepc: usize,
    /// Status register at the time of the trap.
    pub sstatus: usize,
    /// Cause of the trap.
    pub scause: usize,
    /// Faulting address or instruction, depending on the cause.
    pub stval: usize,
}

// The assembly reserves 288 bytes: 35 slots of 8, rounded up to a multiple of
// 16 for stack alignment.
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 280);

/// What caused the trap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trap {
    /// Software interrupt, used for cross hart signalling and by the timer on
    /// some firmware paths.
    SoftwareInterrupt,
    /// Timer interrupt from the SBI timer or from sstc.
    TimerInterrupt,
    /// External interrupt, routed through the PLIC.
    ExternalInterrupt,
    /// Interrupt with a cause this kernel does not use.
    UnknownInterrupt(usize),
    /// Synchronous exception.
    Exception(Exception),
}

/// Synchronous exception cause.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exception {
    /// Instruction fetch was not aligned.
    InstructionMisaligned,
    /// Instruction fetch was not permitted by the PMP or the page tables.
    InstructionAccessFault,
    /// The instruction is not defined or not permitted at this level.
    IllegalInstruction,
    /// An ebreak was executed.
    Breakpoint,
    /// A load address was not aligned.
    LoadMisaligned,
    /// A load was not permitted.
    LoadAccessFault,
    /// A store or AMO address was not aligned.
    StoreMisaligned,
    /// A store or AMO was not permitted.
    StoreAccessFault,
    /// An ecall from user mode. The system call path once tasks exist.
    UserEcall,
    /// An ecall from supervisor mode. The ESL path in Isolated mode.
    SupervisorEcall,
    /// Instruction fetch found no valid mapping.
    InstructionPageFault,
    /// A load found no valid mapping.
    LoadPageFault,
    /// A store or AMO found no valid mapping.
    StorePageFault,
    /// A cause this kernel does not decode.
    Other(usize),
}

impl Trap {
    /// Decodes an scause value.
    ///
    /// The top bit distinguishes an interrupt from an exception; the rest is
    /// the cause number within that class.
    pub fn decode(scause: usize) -> Trap {
        const INTERRUPT: usize = 1 << (usize::BITS as usize - 1);
        let code = scause & !INTERRUPT;
        if scause & INTERRUPT != 0 {
            match code {
                1 => Trap::SoftwareInterrupt,
                5 => Trap::TimerInterrupt,
                9 => Trap::ExternalInterrupt,
                other => Trap::UnknownInterrupt(other),
            }
        } else {
            Trap::Exception(match code {
                0 => Exception::InstructionMisaligned,
                1 => Exception::InstructionAccessFault,
                2 => Exception::IllegalInstruction,
                3 => Exception::Breakpoint,
                4 => Exception::LoadMisaligned,
                5 => Exception::LoadAccessFault,
                6 => Exception::StoreMisaligned,
                7 => Exception::StoreAccessFault,
                8 => Exception::UserEcall,
                9 => Exception::SupervisorEcall,
                12 => Exception::InstructionPageFault,
                13 => Exception::LoadPageFault,
                15 => Exception::StorePageFault,
                other => Exception::Other(other),
            })
        }
    }
}

/// Function the dispatcher calls for every trap.
pub type Handler = fn(&mut TrapFrame);

/// Registered handler, stored as an address because a function pointer is not
/// something an atomic can hold directly.
static HANDLER: AtomicUsize = AtomicUsize::new(0);

extern "C" {
    fn _trap_entry();
}

/// Installs the trap vector and the handler.
///
/// Both in one call, so there is no window in which traps are vectored at code
/// that has nothing to call.
pub fn init(handler: Handler) {
    HANDLER.store(handler as usize, Ordering::Relaxed);
    // Through a pointer rather than straight to an integer. A function item is
    // zero sized, and casting it directly is the shape that silently produces
    // the wrong value when the item is generic.
    crate::csr::stvec_write(_trap_entry as *const () as usize);
}

/// Called from boot/trap.S with the saved frame.
///
/// Interrupts are disabled for the whole of this function: the hardware clears
/// SIE on trap entry and nothing here sets it. That is what makes the kernel
/// non reentrant, which in turn is what makes a single mutable reference to
/// executive state sound.
#[no_mangle]
extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    let address = HANDLER.load(Ordering::Relaxed);
    if address == 0 {
        // A trap before init. There is nothing to call and no way to report,
        // so stop rather than return into an unknown state.
        crate::kprintln!(
            "trap before init: scause {:#x} sepc {:#x} stval {:#x}",
            frame.scause,
            frame.sepc,
            frame.stval
        );
        crate::park();
    }
    // The address came from a Handler in init and nothing else writes it.
    let handler: Handler = unsafe { core::mem::transmute(address) };
    handler(frame);
}