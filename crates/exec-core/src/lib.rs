//! Portable core of the executive.
//!
//! Object manager, scheduler, IRQL, I/O manager. No hardware, no assembly, no
//! allocation: everything this crate owns lives in a fixed array declared at
//! compile time, so an obligation is a bound on an index rather than a
//! separation logic over a heap.
//!
//! Four of the modules here are generated. queue.rs, pmm.rs, objects.rs and
//! window.rs
//! come from the files of the same name under spec/, which are verified by
//! Verus and carry the proofs; scripts/spec-strip.ps1 produces the plain Rust
//! by deleting ghost items and nothing else, and its -Check mode fails the
//! build if the two have drifted. Do not edit them here.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod dpc;
pub mod esl;
pub mod fdt;
pub mod interrupt;
pub mod irql;
pub mod memmap;
pub mod namespace;
pub mod object;
pub mod objects;
pub mod paging;
pub mod platform;
pub mod pmm;
pub mod queue;
pub mod window;

pub use exec_abi::{Handle, Irql, ObjectType, Status, ABI_VERSION};

use esl::Esl;
use interrupt::InterruptTable;
use irql::IrqlState;
use object::ObjectManager;
use platform::Platform;
use pmm::Pmm;
use queue::{ByteRing, Dpc, DpcQueue};

/// Identifies the build to the boot diagnostic.
pub fn banner() -> &'static str {
    concat!("abi ", env!("CARGO_PKG_VERSION"))
}

/// Executive state.
///
/// One instance, owned by the kernel image. Everything reachable from here is
/// a fixed size array, so the whole of the executive is one statically sized
/// object in bss.
///
/// The fields are public and the policy that governs them is not here. IRQL
/// transitions in particular are driven from the kernel image, because
/// lowering has to run deferred calls and a call has to be able to reach this
/// object, which it cannot while the code calling it holds it.
pub struct Executive {
    /// Current interrupt request level.
    pub irql: IrqlState,
    /// Pending deferred calls.
    pub dpcs: DpcQueue,
    /// Registered interrupt handlers.
    pub interrupts: InterruptTable,
    /// Bytes received on the console and not yet consumed.
    pub console_rx: ByteRing,
    /// Timer interrupts taken since boot.
    pub ticks: u64,
    /// Counter ticks between timer interrupts.
    pub tick_interval: u64,
    /// The machine this kernel is running on.
    pub platform: Platform,
    /// Physical page allocator.
    pub pmm: Pmm,
    /// Physical address of the root page table, zero before one is built.
    pub page_root: u64,
    /// Objects and their names.
    pub objects: ObjectManager,
    /// The security layer.
    pub esl: Esl,
}

impl Executive {
    /// Empty executive at PASSIVE.
    ///
    /// const, because this is a static and there is no allocator to build one
    /// at run time instead. The fill values handed to the queues occupy slots
    /// that are outside the occupied range and are never read.
    pub const fn new() -> Self {
        Executive {
            irql: IrqlState::new(),
            dpcs: DpcQueue::new(Dpc {
                routine: 0,
                context: 0,
            }),
            interrupts: InterruptTable::new(),
            console_rx: ByteRing::new(0),
            ticks: 0,
            tick_interval: 0,
            platform: Platform::new(),
            pmm: Pmm::new(),
            page_root: 0,
            objects: ObjectManager::new(),
            esl: Esl::new(),
        }
    }
}

impl Default for Executive {
    fn default() -> Self {
        Self::new()
    }
}