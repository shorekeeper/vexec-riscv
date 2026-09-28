//! Interrupt request level.
//!
//! The level is a property of the executing code, not of the hardware, and it
//! is enforced rather than described: raising it masks interrupts, lowering it
//! unmasks them, and the transition rules below are what the rest of the
//! kernel relies on to know what it is allowed to do.
//!
//! Rules, all checked here:
//!
//!   R1  A raise moves to a level at or above the current one.
//!   R2  A lower moves to a level at or below the current one.
//!   R3  The level after a raise and its matching lower is the level before.
//!   R4  Code at DISPATCH or above never blocks and never faults.
//!
//! R1 to R3 are enforced by this module. R4 is a property of the code that
//! runs at those levels and is checked at the points where blocking happens.

use exec_abi::{Irql, Status};

/// Applies a level to the hardware interrupt mask.
///
/// Implemented by the HAL. The executive owns the level and the rules; the
/// implementation owns the register writes. In the Isolated ESL mode the
/// implementation is an ESL call rather than a CSR write, and nothing above
/// this trait changes.
pub trait InterruptController {
    /// Masks every interrupt at or below the given level.
    fn set_mask(&mut self, irql: Irql);
}

/// Current interrupt request level.
///
/// One per hart. There is one hart, so there is one of these, held by the
/// executive.
#[derive(Clone, Copy, Debug)]
pub struct IrqlState {
    current: Irql,
}

impl IrqlState {
    /// New state at PASSIVE.
    ///
    /// The hardware mask is not written here: construction happens before the
    /// controller exists. The first raise or lower brings the two into
    /// agreement, and `sync` does it explicitly during boot.
    pub const fn new() -> Self {
        IrqlState {
            current: Irql::PASSIVE,
        }
    }

    /// Current level.
    pub const fn current(&self) -> Irql {
        self.current
    }

    /// Writes the current level to the hardware mask.
    ///
    /// Called once during boot to make the mask match the state. Used nowhere
    /// else: every other path changes both together.
    pub fn sync(&self, controller: &mut impl InterruptController) {
        controller.set_mask(self.current);
    }

    /// Raises to a level and returns the previous one.
    ///
    /// Fails rather than silently doing nothing when the target is below the
    /// current level. A raise that does not raise is a bug in the caller, and
    /// the matching lower would then drop the level further than intended.
    pub fn raise(
        &mut self,
        controller: &mut impl InterruptController,
        to: Irql,
    ) -> Result<Irql, Status> {
        if !to.is_valid() {
            return Err(Status::InvalidArgument);
        }
        if to < self.current {
            return Err(Status::IrqlViolation);
        }
        let previous = self.current;
        self.current = to;
        // Mask before returning: the caller is entitled to assume the level is
        // in effect the moment the call returns.
        controller.set_mask(to);
        Ok(previous)
    }

    /// Lowers to a level.
    ///
    /// The argument is the value a matching raise returned. Fails when the
    /// target is above the current level, which is a raise wearing the wrong
    /// name.
    ///
    /// Does not drain the DPC queue. That is done by the caller, because the
    /// queue lives in the executive and this module has no access to it; the
    /// executive wrapper does both in the right order.
    pub fn lower(&mut self, controller: &mut impl InterruptController, to: Irql) -> Status {
        if !to.is_valid() {
            return Status::InvalidArgument;
        }
        if to > self.current {
            return Status::IrqlViolation;
        }
        self.current = to;
        controller.set_mask(to);
        Status::Success
    }

    /// True when code at the current level may block.
    pub const fn may_block(&self) -> bool {
        self.current.may_block()
    }
}

impl Default for IrqlState {
    fn default() -> Self {
        Self::new()
    }
}