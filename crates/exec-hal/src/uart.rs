//! 16550 compatible serial port.
//!
//! Register numbers, each at `base + (n << shift)`:
//!
//! ```text
//!   0  RBR read, THR write, DLL when DLAB is set
//!   1  IER, DLM when DLAB is set
//!   2  IIR read, FCR write
//!   3  LCR
//!   4  MCR
//!   5  LSR
//!   6  MSR
//!   7  SCR
//! ```
//!
//! Transmit is polled and receive is interrupt driven. Polling a transmit is
//! bounded by the line rate and both target machines complete it immediately,
//! so the cost of an interrupt path for it is not worth the state it needs.

use crate::mmio;

const REG_DATA: usize = 0;
const REG_IER: usize = 1;
const REG_FCR: usize = 2;
const REG_LCR: usize = 3;
const REG_MCR: usize = 4;
const REG_LSR: usize = 5;

/// Received data available.
const IER_RX: u8 = 1 << 0;

/// Enable the FIFOs.
const FCR_ENABLE: u8 = 1 << 0;
/// Clear the receive FIFO.
const FCR_CLEAR_RX: u8 = 1 << 1;
/// Clear the transmit FIFO.
const FCR_CLEAR_TX: u8 = 1 << 2;

/// Eight data bits, no parity, one stop bit.
const LCR_8N1: u8 = 0b11;

/// Data terminal ready.
const MCR_DTR: u8 = 1 << 0;
/// Request to send.
const MCR_RTS: u8 = 1 << 1;
/// Auxiliary output two. On parts where it gates the interrupt line, an
/// interrupt never reaches the controller without it.
const MCR_OUT2: u8 = 1 << 3;

/// Receive holding register has a byte.
const LSR_RX_READY: u8 = 1 << 0;
/// Transmit holding register is empty.
const LSR_TX_EMPTY: u8 = 1 << 5;

/// Address of a register.
const fn reg(base: usize, shift: u32, index: usize) -> usize {
    base + (index << shift)
}

/// Prepares the port for interrupt driven receive.
///
/// The divisor is left as the firmware set it. Changing the line rate would
/// require a correct input clock, and getting it wrong makes every subsequent
/// diagnostic unreadable, including the one saying what went wrong.
///
/// # Safety
///
/// `base` must be the register block of a 16550 compatible port.
pub unsafe fn init(base: usize, shift: u32) {
    // Interrupts off while the FIFOs are reconfigured.
    mmio::write8(reg(base, shift, REG_IER), 0);
    // Clear DLAB so the low registers mean what the names say.
    mmio::write8(reg(base, shift, REG_LCR), LCR_8N1);
    mmio::write8(
        reg(base, shift, REG_FCR),
        FCR_ENABLE | FCR_CLEAR_RX | FCR_CLEAR_TX,
    );
    mmio::write8(reg(base, shift, REG_MCR), MCR_DTR | MCR_RTS | MCR_OUT2);

    // Drain anything that arrived before the kernel was listening, so the
    // first interrupt corresponds to a byte typed after it started.
    while mmio::read8(reg(base, shift, REG_LSR)) & LSR_RX_READY != 0 {
        let _ = mmio::read8(reg(base, shift, REG_DATA));
    }

    mmio::write8(reg(base, shift, REG_IER), IER_RX);
}

/// Takes one received byte, or None when there is none.
///
/// # Safety
///
/// `base` must be an initialised port.
pub unsafe fn read_byte(base: usize, shift: u32) -> Option<u8> {
    if mmio::read8(reg(base, shift, REG_LSR)) & LSR_RX_READY == 0 {
        return None;
    }
    Some(mmio::read8(reg(base, shift, REG_DATA)))
}

/// Sends one byte, waiting for room.
///
/// # Safety
///
/// `base` must be an initialised port.
pub unsafe fn write_byte(base: usize, shift: u32, byte: u8) {
    // Bounded by the line rate. The count is a backstop for a port that never
    // reports itself empty, which would otherwise hang the console and with it
    // any chance of finding out why.
    let mut spins = 0u32;
    while mmio::read8(reg(base, shift, REG_LSR)) & LSR_TX_EMPTY == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return;
        }
    }
    mmio::write8(reg(base, shift, REG_DATA), byte);
}