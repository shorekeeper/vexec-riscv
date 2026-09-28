//! Interrupt source table.
//!
//! Maps a controller source number to the routine that services it and the
//! IRQL that routine runs at. Fixed size and indexed directly, so a lookup is
//! a bounds check and a load.
//!
//! Priority serves two purposes at once and they agree by construction: it is
//! what the controller compares against its threshold, and it is what the IRQL
//! of the service routine is derived from. One number, so a device cannot be
//! important to one mechanism and unimportant to the other.

use exec_abi::{Irql, Status};

/// Sources the table covers.
///
/// Must match the range the controller is driven over, which is thirty two on
/// the smaller of the two machines. A source above this is claimed and
/// completed by the dispatcher without a handler, so an unexpected one is
/// reported rather than left pending.
pub const MAX_SOURCES: usize = 32;

/// Routine that services an interrupt.
///
/// Runs at the device level of its source, with that level and everything
/// below it masked. It may not block. Its job is to quiet the device and queue
/// a deferred call; anything longer belongs in that call.
pub type Isr = fn(context: usize);

/// One registered source.
#[derive(Clone, Copy)]
pub struct Source {
    /// Routine, or None when nothing is registered.
    pub isr: Option<Isr>,
    /// Value passed to the routine.
    pub context: usize,
    /// Controller priority, one upward. Zero means unregistered.
    pub priority: u8,
}

impl Source {
    /// Unregistered source.
    pub const EMPTY: Source = Source {
        isr: None,
        context: 0,
        priority: 0,
    };

    /// Level the routine runs at.
    pub const fn irql(&self) -> Irql {
        Irql::device(self.priority)
    }
}

/// Source number to handler.
pub struct InterruptTable {
    sources: [Source; MAX_SOURCES],
    /// Interrupts claimed for a source with no handler.
    ///
    /// Nonzero means hardware is producing interrupts nobody asked for, which
    /// is a configuration fault rather than a fatal one.
    spurious: u64,
}

impl InterruptTable {
    /// Empty table.
    pub const fn new() -> Self {
        InterruptTable {
            sources: [Source::EMPTY; MAX_SOURCES],
            spurious: 0,
        }
    }

    /// Registers a handler.
    ///
    /// Source zero is rejected because the controller uses it to mean no
    /// interrupt. Priority zero is rejected because the controller uses it to
    /// mean silent, and a handler that can never run is a defect worth
    /// reporting at the point it is written.
    pub fn register(
        &mut self,
        source: u32,
        isr: Isr,
        context: usize,
        priority: u8,
    ) -> Status {
        if source == 0 || priority == 0 || priority > Irql::MAX_PRIORITY {
            return Status::InvalidArgument;
        }
        let index = source as usize;
        if index >= MAX_SOURCES {
            return Status::InvalidArgument;
        }
        if self.sources[index].isr.is_some() {
            return Status::Exists;
        }
        self.sources[index] = Source {
            isr: Some(isr),
            context,
            priority,
        };
        Status::Success
    }

    /// Removes a handler.
    pub fn unregister(&mut self, source: u32) -> Status {
        let index = source as usize;
        if index == 0 || index >= MAX_SOURCES {
            return Status::InvalidArgument;
        }
        if self.sources[index].isr.is_none() {
            return Status::NotFound;
        }
        self.sources[index] = Source::EMPTY;
        Status::Success
    }

    /// Returns the handler for a source.
    ///
    /// Counts a miss, because a claimed source with no handler is the symptom
    /// of a device left enabled by something that ran earlier.
    pub fn lookup(&mut self, source: u32) -> Option<Source> {
        let index = source as usize;
        if index == 0 || index >= MAX_SOURCES {
            self.spurious = self.spurious.saturating_add(1);
            return None;
        }
        match self.sources[index].isr {
            Some(_) => Some(self.sources[index]),
            None => {
                self.spurious = self.spurious.saturating_add(1);
                None
            }
        }
    }

    /// Interrupts claimed with no handler registered.
    pub const fn spurious(&self) -> u64 {
        self.spurious
    }
}

impl Default for InterruptTable {
    fn default() -> Self {
        Self::new()
    }
}