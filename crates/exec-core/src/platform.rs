//! Machine description, built from the device tree.
//!
//! What the kernel needs to know about the hardware it is on: where memory is,
//! how fast the timer counts, and the address of every device it has a driver
//! for. Fixed size throughout, so the description is one object in bss and the
//! parse cannot fail for want of space.
//!
//! Nodes are recognised by the compatible property where one exists and by the
//! node name otherwise. Name matching alone is not enough: a node named
//! interrupt-controller is the PLIC under soc and the per hart local
//! controller under a cpu, and only compatible tells them apart.
//!
//! State is kept per depth rather than per builder. Nodes of interest nest:
//! cpu holds an interrupt-controller, and a single slot would be overwritten
//! by the child and empty when the parent closes.

use crate::fdt::{
    list_contains, name_is, read_cells, read_u32, read_u64, Cells, Fdt, FdtError, Visitor,
    MAX_DEPTH,
};

/// Memory regions the description can hold.
pub const MAX_MEMORY_REGIONS: usize = 8;
/// Reserved regions the description can hold.
pub const MAX_RESERVATIONS: usize = 16;
/// virtio-mmio transports the description can hold.
pub const MAX_VIRTIO: usize = 16;

/// Depth at which devices sit on both target machines.
///
/// Root is one, soc is two, a device is three. Used only to decide whether an
/// unrecognised node is worth counting, so a machine that nests differently
/// reports a less useful number and nothing else.
const DEVICE_DEPTH: usize = 3;

/// A memory mapped register block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mmio {
    /// Physical address of the first register.
    pub base: u64,
    /// Length of the block in bytes.
    pub size: u64,
}

impl Mmio {
    /// A block naming nothing.
    pub const NONE: Mmio = Mmio { base: 0, size: 0 };

    /// True when the block was found.
    pub const fn is_present(&self) -> bool {
        self.size != 0
    }
}

/// A region of usable memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryRegion {
    /// First byte.
    pub base: u64,
    /// Length in bytes.
    pub size: u64,
}

impl MemoryRegion {
    /// An empty region.
    pub const NONE: MemoryRegion = MemoryRegion { base: 0, size: 0 };

    /// One past the last byte.
    pub const fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }
}

/// A device that raises interrupts through the PLIC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Device {
    /// Register block.
    pub mmio: Mmio,
    /// PLIC source number, or zero when the device raises none.
    pub irq: u32,
}

impl Device {
    /// A device that was not found.
    pub const NONE: Device = Device {
        mmio: Mmio::NONE,
        irq: 0,
    };

    /// True when the device was found.
    pub const fn is_present(&self) -> bool {
        self.mmio.is_present()
    }
}

/// A 16550 compatible serial port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Uart {
    /// Register block and interrupt.
    pub device: Device,
    /// Distance between registers, as a power of two shift.
    ///
    /// Zero means one byte apart, which is the usual layout. A nonzero value
    /// means the registers are spread out, which some implementations do.
    pub reg_shift: u32,
    /// Input clock in hertz, used to compute the divisor. Zero when the tree
    /// does not say, in which case the driver leaves the divisor alone.
    pub clock_hz: u32,
}

impl Uart {
    /// A port that was not found.
    pub const NONE: Uart = Uart {
        device: Device::NONE,
        reg_shift: 0,
        clock_hz: 0,
    };
}

/// Everything the kernel knows about the machine.
pub struct Platform {
    /// Counter ticks per second.
    pub timebase_hz: u64,
    /// Harts described by the tree. Only the boot hart is used.
    pub hart_count: u32,
    /// Hart the firmware booted on.
    pub boot_hart: u32,

    /// Usable memory.
    pub memory: [MemoryRegion; MAX_MEMORY_REGIONS],
    /// Entries filled in `memory`.
    pub memory_count: usize,

    /// Memory the firmware asks to be left alone.
    pub reserved: [MemoryRegion; MAX_RESERVATIONS],
    /// Entries filled in `reserved`.
    pub reserved_count: usize,

    /// Interrupt controller.
    pub plic: Mmio,
    /// Serial port.
    pub uart: Uart,

    /// virtio-mmio transports, ascending by base address.
    ///
    /// Sorted rather than left in tree order, because the index into this
    /// array becomes the number a device is known by, and tree order is
    /// whatever the generator felt like. QEMU emits these in descending
    /// address order.
    pub virtio: [Device; MAX_VIRTIO],
    /// Entries filled in `virtio`.
    pub virtio_count: usize,

    /// Device nodes with a register block that this kernel did not recognise.
    ///
    /// Counted so that a machine with hardware the kernel does not know about
    /// says so, instead of appearing to have none.
    pub unknown_nodes: u32,
}

impl Platform {
    /// An empty description.
    pub const fn new() -> Self {
        Platform {
            timebase_hz: 0,
            hart_count: 0,
            boot_hart: 0,
            memory: [MemoryRegion::NONE; MAX_MEMORY_REGIONS],
            memory_count: 0,
            reserved: [MemoryRegion::NONE; MAX_RESERVATIONS],
            reserved_count: 0,
            plic: Mmio::NONE,
            uart: Uart::NONE,
            virtio: [Device::NONE; MAX_VIRTIO],
            virtio_count: 0,
            unknown_nodes: 0,
        }
    }

    /// Total usable memory in bytes.
    pub fn memory_total(&self) -> u64 {
        let mut total = 0u64;
        let mut i = 0;
        while i < self.memory_count && i < MAX_MEMORY_REGIONS {
            total = total.saturating_add(self.memory[i].size);
            i += 1;
        }
        total
    }

    /// Copies the description.
    ///
    /// The map builder needs the description while it holds the allocator, and
    /// both live inside the executive. Copying is the cheapest way out: the
    /// structure is a few hundred bytes of plain data and is not modified
    /// after boot.
    pub fn clone_description(&self) -> Platform {
        Platform {
            timebase_hz: self.timebase_hz,
            hart_count: self.hart_count,
            boot_hart: self.boot_hart,
            memory: self.memory,
            memory_count: self.memory_count,
            reserved: self.reserved,
            reserved_count: self.reserved_count,
            plic: self.plic,
            uart: self.uart,
            virtio: self.virtio,
            virtio_count: self.virtio_count,
            unknown_nodes: self.unknown_nodes,
        }
    }
    
    /// Builds a description from a blob.
    pub fn from_fdt(fdt: &Fdt) -> Result<Platform, FdtError> {
        let mut builder = Builder::new();
        builder.platform.boot_hart = fdt.header().boot_cpu;

        fdt.walk(&mut builder)?;

        // Reservations come from their own block rather than from the tree.
        let mut entries = [crate::fdt::Reservation {
            address: 0,
            size: 0,
        }; MAX_RESERVATIONS];
        let count = fdt.reservations(&mut entries)?;
        let mut i = 0;
        while i < count && i < MAX_RESERVATIONS {
            builder.platform.reserved[i] = MemoryRegion {
                base: entries[i].address,
                size: entries[i].size,
            };
            i += 1;
        }
        builder.platform.reserved_count = count.min(MAX_RESERVATIONS);

        Ok(builder.platform)
    }
}

impl Default for Platform {
    fn default() -> Self {
        Self::new()
    }
}

/// What a node turned out to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Not one of the kinds below.
    Other,
    /// The cpus container. Holds the timebase on most trees.
    Cpus,
    /// A single hart.
    Cpu,
    /// Usable memory.
    Memory,
    /// Interrupt controller, subject to confirmation by compatible.
    Plic,
    /// Serial port.
    Uart,
    /// virtio-mmio transport.
    Virtio,
    /// Known hardware this kernel has no driver for and does not need one.
    Ignored,
}

/// State of one open node.
///
/// One per depth. Copy so that a completed node can be read out of the stack
/// without borrowing the builder for the length of the commit.
#[derive(Clone, Copy)]
struct Node {
    kind: Kind,
    base: u64,
    size: u64,
    has_reg: bool,
    irq: u32,
    reg_shift: u32,
    clock_hz: u32,
    /// Set when compatible names something this kernel does not drive, which
    /// overrides a guess made from the node name.
    rejected: bool,
}

impl Node {
    const EMPTY: Node = Node {
        kind: Kind::Other,
        base: 0,
        size: 0,
        has_reg: false,
        irq: 0,
        reg_shift: 0,
        clock_hz: 0,
        rejected: false,
    };
}

struct Builder {
    platform: Platform,
    /// Open nodes, indexed by depth. Index zero is unused: the root is depth
    /// one, which keeps the indexing the same as the walk reports.
    stack: [Node; MAX_DEPTH + 1],
    /// Depth of the cpus node while it is open, zero otherwise.
    cpus_depth: usize,
}

impl Builder {
    fn new() -> Self {
        Builder {
            platform: Platform::new(),
            stack: [Node::EMPTY; MAX_DEPTH + 1],
            cpus_depth: 0,
        }
    }

    /// Guesses what a node is from its name.
    ///
    /// A guess only. The compatible property confirms or rejects it, because
    /// a name is convention and compatible is specification.
    fn classify(&self, depth: usize, name: &[u8]) -> Kind {
        if name_is(name, b"cpus") {
            return Kind::Cpus;
        }
        // Only directly under cpus. A node named cpu elsewhere is something
        // else, and a hart counted twice makes the scheduler wrong.
        if name_is(name, b"cpu") && self.cpus_depth != 0 && depth == self.cpus_depth + 1 {
            return Kind::Cpu;
        }
        if name_is(name, b"memory") {
            return Kind::Memory;
        }
        if name_is(name, b"plic") || name_is(name, b"interrupt-controller") {
            return Kind::Plic;
        }
        if name_is(name, b"uart") || name_is(name, b"serial") {
            return Kind::Uart;
        }
        if name_is(name, b"virtio_mmio") {
            return Kind::Virtio;
        }
        // Recognised and deliberately undriven: counting them as unknown makes
        // the boot report say a machine has hardware nobody looked at, when in
        // fact it has hardware this kernel has decided not to use.
        if name_is(name, b"rtc") || name_is(name, b"syscon") || name_is(name, b"poweroff")
            || name_is(name, b"reboot") || name_is(name, b"clint")
            || name_is(name, b"test") || name_is(name, b"pci")
            || name_is(name, b"platform-bus") || name_is(name, b"fw-cfg")
        {
            return Kind::Ignored;
        }
        Kind::Other
    }

    /// Inserts a transport, keeping the array ascending by base address.
    ///
    /// Insertion sort over at most MAX_VIRTIO entries. The bound makes the
    /// cost irrelevant and removes the need for a sort pass afterwards.
    fn insert_virtio(&mut self, device: Device) {
        let count = self.platform.virtio_count;
        if count >= MAX_VIRTIO {
            return;
        }

        let mut at = 0;
        while at < count && self.platform.virtio[at].mmio.base < device.mmio.base {
            at += 1;
        }

        let mut i = count;
        while i > at {
            self.platform.virtio[i] = self.platform.virtio[i - 1];
            i -= 1;
        }
        self.platform.virtio[at] = device;
        self.platform.virtio_count = count + 1;
    }

    /// Records a finished node in the description.
    fn commit(&mut self, node: Node, depth: usize) {
        if node.rejected {
            return;
        }

        let device = Device {
            mmio: Mmio {
                base: node.base,
                size: node.size,
            },
            irq: node.irq,
        };

        match node.kind {
            Kind::Plic => {
                if node.has_reg && !self.platform.plic.is_present() {
                    self.platform.plic = device.mmio;
                }
            }
            Kind::Uart => {
                // First one wins. A machine with several serial ports gets the
                // one the tree lists first, which is the console on both
                // targets.
                if node.has_reg && !self.platform.uart.device.is_present() {
                    self.platform.uart = Uart {
                        device,
                        reg_shift: node.reg_shift,
                        clock_hz: node.clock_hz,
                    };
                }
            }
            Kind::Virtio => {
                // A transport with no device behind it still has a register
                // block. Filtering those is the driver's job, by reading the
                // device id, not something the tree can say.
                if node.has_reg {
                    self.insert_virtio(device);
                }
            }
            Kind::Cpu => {
                self.platform.hart_count = self.platform.hart_count.saturating_add(1);
            }
            Kind::Other => {
                // A node with registers at the depth devices live at, which
                // this kernel has no driver for.
                if node.has_reg && depth == DEVICE_DEPTH {
                    self.platform.unknown_nodes = self.platform.unknown_nodes.saturating_add(1);
                }
            }
            Kind::Cpus | Kind::Memory | Kind::Ignored => {}
        }
    }
}

impl Visitor for Builder {
    fn begin_node(&mut self, depth: usize, name: &[u8]) {
        if depth > MAX_DEPTH {
            return;
        }

        let kind = self.classify(depth, name);
        self.stack[depth] = Node {
            kind,
            ..Node::EMPTY
        };

        // Set after classify, so that a node named cpus is not treated as its
        // own child.
        if kind == Kind::Cpus {
            self.cpus_depth = depth;
        }
    }

    fn property(&mut self, depth: usize, name: &[u8], value: &[u8], parent: Cells) {
        if depth > MAX_DEPTH {
            return;
        }

        // The timebase sits on cpus on some trees and on each cpu on others,
        // so it is read wherever it appears and the first value wins.
        if name == b"timebase-frequency" && self.platform.timebase_hz == 0 {
            if let Some(hz) = read_u32(value) {
                self.platform.timebase_hz = hz as u64;
            } else if let Some(hz) = read_u64(value) {
                self.platform.timebase_hz = hz;
            }
        }

        // Memory is handled here rather than at commit, because its reg may
        // list several regions and each becomes an entry of its own.
        if self.stack[depth].kind == Kind::Memory && name == b"reg" {
            let mut offset = 0;
            while self.platform.memory_count < MAX_MEMORY_REGIONS {
                let rest = match value.get(offset..) {
                    Some(rest) if !rest.is_empty() => rest,
                    _ => break,
                };
                let (base, used_a) = match read_cells(rest, parent.address) {
                    Some(pair) => pair,
                    None => break,
                };
                let rest = match rest.get(used_a..) {
                    Some(rest) => rest,
                    None => break,
                };
                let (size, used_s) = match read_cells(rest, parent.size) {
                    Some(pair) => pair,
                    None => break,
                };
                if size == 0 {
                    break;
                }
                self.platform.memory[self.platform.memory_count] = MemoryRegion { base, size };
                self.platform.memory_count += 1;
                offset += used_a + used_s;
            }
            return;
        }

        // reg is read for every node, recognised or not: it is what makes an
        // unrecognised node countable as a device rather than as a container.
        if name == b"reg" {
            if let Some((base, used)) = read_cells(value, parent.address) {
                let size = value
                    .get(used..)
                    .and_then(|rest| read_cells(rest, parent.size))
                    .map(|(size, _)| size)
                    .unwrap_or(0);
                let node = &mut self.stack[depth];
                node.base = base;
                // A reg with no size cells still names a device. One page is
                // assumed so the block is not treated as absent.
                node.size = if size == 0 { 0x1000 } else { size };
                node.has_reg = true;
            }
            return;
        }

        let kind = self.stack[depth].kind;
        if kind == Kind::Other {
            return;
        }

        match name {
            b"interrupts" => {
                // The first cell is the source number. Trees with more cells
                // per interrupt put flags in the rest, which the PLIC does not
                // use.
                if let Some(irq) = read_u32(value) {
                    self.stack[depth].irq = irq;
                }
            }
            b"reg-shift" => {
                if let Some(shift) = read_u32(value) {
                    self.stack[depth].reg_shift = shift;
                }
            }
            b"clock-frequency" => {
                if let Some(hz) = read_u32(value) {
                    self.stack[depth].clock_hz = hz;
                }
            }
            b"compatible" => match kind {
                Kind::Plic => {
                    // An interrupt-controller that is not the PLIC is the per
                    // hart local controller inside a cpu node, and taking it
                    // for the PLIC would give the wrong base address.
                    let is_plic = list_contains(value, b"riscv,plic0")
                        || list_contains(value, b"sifive,plic-1.0.0");
                    self.stack[depth].rejected = !is_plic;
                }
                Kind::Uart => {
                    let known = list_contains(value, b"ns16550a")
                        || list_contains(value, b"ns16550")
                        || list_contains(value, b"snps,dw-apb-uart");
                    self.stack[depth].rejected = !known;
                }
                Kind::Virtio => {
                    self.stack[depth].rejected = !list_contains(value, b"virtio,mmio");
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn end_node(&mut self, depth: usize) {
        if depth <= MAX_DEPTH {
            let node = self.stack[depth];
            self.commit(node, depth);
            self.stack[depth] = Node::EMPTY;
        }
        if self.cpus_depth == depth {
            self.cpus_depth = 0;
        }
    }
}