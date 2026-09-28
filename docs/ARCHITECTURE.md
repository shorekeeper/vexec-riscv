# vExec architecture

A statically configured executive for RV64, targeting QEMU virt as the reference platform and Sedna, the emulator inside OpenComputers II: Reimagined, as the deployment platform. Single hart, run to completion, no allocation in the kernel.

The design is NT: an object manager, interrupt request levels, deferred procedure calls, and a layered I/O model with driver stacks. The isolation boundary is modelled on PEF and VBS but implemented in software, because the hardware offers nothing to build it from.

## 1. Privilege layout

RV64 without the H extension has three modes and OpenSBI owns M, which leaves S and U for everything vExec does. In the Off and Integrity modes the executive runs in S and tasks run in U; in the Isolated mode the executive itself is demoted to U and the security layer takes S. There is no fourth level to hide a monitor in, so isolation is bought by demoting the kernel rather than by adding a layer beneath it.

## 2. ESL modes

ESL is the Envisor Security Layer. It has three configurations, selected at image build time and recorded in the boot header.

**Off.** No layer. The executive runs in S and owns every CSR directly. Fastest, and the mode used while bringing up support for new hardware.

**Integrity.** The executive runs in S. ESL is a module in the same privilege level that owns the page tables and the code integrity policy by convention rather than by hardware. It defends against defects in the executive, not against an executive that has been subverted: any code in S can write satp. The value is real and the bound is stated so nobody mistakes it for isolation.

**Isolated.** The executive and all drivers run in U with their own address spaces. ESL runs in S and owns satp, stvec, sie, every page table, and the SBI path. The consequences are load bearing and all of them follow from the same fact. The executive cannot execute a privileged instruction, so every such operation becomes a call into ESL, which is why the HAL is the only crate permitted to contain privileged code: in Off it emits the instruction, in Isolated it emits an ecall. Traps arrive in S, where ESL decodes them and upcalls the executive. The executive and user tasks are both in U and are told apart by which address space ESL has installed, not by the mode bits. The executive cannot read user memory directly, so transfers cross worlds through an ESL primitive that checks the mapping and the direction. SBI is not reachable from U, so ESL proxies and filters it.

The cost on Sedna is that a system call becomes four mode changes rather than two. Acceptable for a kernel whose calls are infrequent and coarse, and not acceptable for one that proxies bytes, so the interfaces are sized accordingly.

## 3. ESL invariants

These are the statements the verification effort targets. They are properties of the page tables and the world table, both of which are fixed size arrays.

I1. No page table entry reachable from any world grants write and execute together.

I2. A page gains execute only after its contents are measured and the measurement appears in the policy; from that point it is not writable by anyone.

I3. No entry outside the ESL address space maps ESL memory.

I4. Two worlds share a page only through an explicitly created window with a recorded direction and extent.

I5. A world cannot alter its own mappings; only ESL writes page tables.

I1 and I3 are the HVCI equivalent. I5 is what makes the rest mean anything. I1, I2 and I3 are enforced in the Integrity mode; I4 and I5 require Isolated.

## 4. Crates

`exec-abi` holds the types crossing every boundary: status codes, handles, the ABI version, the ESL service numbers. No code, no unsafe, no privilege.

`exec-hal` is the only place privileged operations and memory mapped register access exist. CSR access, trap entry, MMIO, SBI. Unsafe, trusted, not verified.

`exec-core` is the executive: object manager, scheduler, IRQL, I/O manager, the security layer policy. Declared no_std and forbid(unsafe_code), and partly verified.

`exec` is the kernel image, linking the boot code, the HAL and the core.

`esl` will be the security layer as a separate image in the Isolated mode.

The split exists so that the trusted computing base is a set of files rather than a claim, and exec-hal is measured by how long it takes to read.

## 5. Verification

Four modules are proved with Verus: the bounded queue, the physical page allocator, the object table, and the physical window check. They live under `spec/` as Verus source, outside the cargo workspace, and are verified standalone. The plain Rust that the kernel compiles is produced from them by `scripts/spec-strip.ps1`, which only deletes ghost items, specification clauses, proof blocks and asserts, so every executable statement in the generated file is the one that was verified, character for character.

The build runs the proofs first and refuses to compile if any fails, then compares each generated file against its source and refuses if they have drifted. The two files therefore carry the same guarantee as one would, because the equality is checked by machine.

This arrangement exists because adopting vstd as a cargo dependency puts fifteen crates into the lock file and compiles vstd for the target, which conflicts with the zero dependency rule. Nothing from the verifier reaches the artefact either way.

## 6. Object manager

Every kernel resource is an object with a common header and a type. Objects live in per type static arrays sized at build time; there is no allocator. A handle is 64 bits: the low sixteen are the slot index, the next sixteen are the generation, the next eight are the type tag, and the rest are reserved and zero.

The generation is what makes a stale handle detectable rather than dangerous. A lookup compares it against the value held by the slot and fails on mismatch, and the generation never decreases under any operation, so a handle that has stopped resolving can never resolve again: not for the next caller, not after the slot is reused, not ever. That is the property which replaces the guarantees a garbage collector would otherwise provide, and it is proved rather than asserted.

The cost is that sixteen bits give a slot 65535 uses, after which it is retired and never allocated again. Wrapping instead would make a sufficiently old handle valid once more, which is a fault that appears under load and nowhere else; a leaked slot is arithmetic, a resurrected handle is not.

The types are Task, Thread, Port, Section, Device, Driver, Timer and Event. The namespace is a flat table of interned names with a parent index, which gives NT style paths such as \Device\Serial0 without a tree structure to prove things about.

## 7. IRQL and deferred work

Four levels. PASSIVE at zero is normal execution and may block. DISPATCH at two runs the scheduler and deferred calls and may not block. DEVICE from three to fourteen is interrupt service, where the number is the PLIC priority. HIGH at fifteen masks everything. Level one is reserved and unused: it is where NT puts APC_LEVEL, which exists to run a callback in the context of a specific thread, and a run to completion kernel has no such context.

IRQL is not an abstraction over a variable. It maps onto the sie bits and the PLIC threshold register, so raising it does mask interrupts and lowering it does unmask them, and lowering below DISPATCH drains the deferred call queue before returning. The invariant carried through the executive is that code running at DISPATCH or above never blocks and never takes a fault.

Interrupt handling splits as in NT. The service routine runs at DEVICE, quiets the device, and queues a deferred call; the call runs at DISPATCH and does the work. In the Isolated mode the service routine is an ESL upcall into the driver world.

One known limitation, stated rather than hidden: deferred calls currently run with interrupts globally disabled, because the drain happens inside a trap handler and the hardware clears the enable on entry. Device interrupts are delayed by the length of the drain. NT enables them at that point, and doing so here means permitting the trap handler to reenter itself, which needs a bound on nesting depth and a stack sized for it.

## 8. I/O model

NT layering with fixed size packets. A request is a packet with a header and a static array of stack locations, one per layer in the device stack, bounded at build time. A driver examines its own location and either completes the packet or passes it down.

Drivers are separate images. In the Isolated mode each is a world of its own; in Off they are modules in the kernel address space. The driver interface is the same in both, which is what makes the mode a build option rather than a fork.

Loading a driver works like this: the image carries an ABI version and a measurement, the executive refuses a version mismatch, and ESL refuses an unmeasured image. This is what remains of the DKMS idea once the target has no compiler. Modules are built against a versioned interface on the host and are checked, not rebuilt, on load.

## 9. Memory

No demand paging, no swap, no copy on write. Physical memory is partitioned at boot from the device tree into fixed regions, and a region is either kernel, a world, or a shared window. Kernel memory is identity mapped, which removes an entire class of obligation from the proof because a pointer means the same thing on both sides of a table walk. Worlds use 4 KiB pages under Sv39; the kernel uses 2 MiB pages where alignment permits.

The page allocator is a bitmap rather than a free list. A free list stores its links inside the free pages, which means the allocator writes to memory it does not own yet and a corrupted page corrupts the allocator; a bitmap keeps its state in one place and a page that is never touched while free stays untouched.

## 10. Platform differences

QEMU virt is the reference and correctness is defined against it. Sedna is the deployment target.

Sedna makes FENCE and FENCE.I no-ops, does not order atomics against memory, and is incorrect on multiple harts. A single hart executive that never relies on a barrier cannot observe any of this, which is why the single hart decision is architectural rather than a simplification.

Floating point is software emulated there and therefore expensive. The kernel is built for riscv64imac with the lp64 ABI and contains no floating point at all, which also removes IEEE semantics from the proof obligations.

Sedna serialises and restores machine state when a chunk unloads, so the delta across a restore is arbitrary and possibly enormous. Only the monotonic counter is treated as time, and code that consumes it tolerates a large jump.

## 11. DMA, and what isolation can mean without an IOMMU

Neither target machine has an IOMMU. QEMU virt does not present one and Sedna does not implement one. This is not a detail; it decides what the Isolated mode is able to promise.

A virtio driver builds a descriptor holding a physical address and tells the device to proceed. The device reads the descriptor and writes to that address. If the driver is a world confined to its own address space but is free to write its own descriptors, it names any physical address it likes and the device writes there on its behalf. Page tables are not consulted, because the device is not a hart, so isolation is bypassed through hardware without a single page table entry being touched.

There are three ways out. Queues could live in ESL, so the driver sends a request and ESL builds the descriptor: isolation holds and the virtio transport moves into the trusted computing base, roughly doubling it. Drivers that touch devices could simply not be isolated: cheap, honest, and it leaves the disk and the network outside the boundary, which is to say outside anything interesting. Or each world owns a fixed physical region and ESL validates every address a queue names against that region before the device is allowed to proceed, with data crossing by copy.

The third is what this system does, for two reasons. The validation is arithmetic over a fixed size array, which is the kind of thing that can be proved rather than reviewed, and it is proved. And the mechanism costs nothing in the Off mode, where the check compares against a window covering all of memory.

The consequence for drivers is that a buffer must live in the world's window rather than wherever the allocator put it. Stated here, in one place, rather than discovered later in every driver at once.

## 12. The boundary call

ESL is reached by one call shape in every mode, and only the transport changes: a direct call in Off and Integrity, where the layer is code in the same privilege level and address space, and an ecall in Isolated, where the executive is in U and the layer is in S.

The shape is fixed by the strictest of the three, which is the ecall. Arguments are values in registers, six at most, sixty four bits each. There are no pointer arguments, ever: a structure the caller can reach is a structure the caller can change between the validation and the use, and that is the ordinary way a checked system call is defeated. The return is one status and one value.

Writing the interface this way in the Off mode costs nothing and is what makes the Isolated mode a configuration rather than a rewrite.

## 13. Boot

OpenSBI hands control in S at 0x80200000 with the hart id in a0 and the device tree address in a1. The entry code sets gp, parks every hart but the first, clears bss, sets the stack, and calls into Rust.

Nothing that depends on the machine runs before the device tree is read. A kernel that cannot describe the machine has no timer frequency and no memory map, and guessing either produces a system that appears to work.

Translation is enabled before any device driver initialises, because the map covers the device registers and a driver brought up first would be touching addresses that are about to change meaning. The map is checked before satp is written: every address that has to work across that instruction is resolved through the new tables while the old translation is still in effect, because a failure at that point is recoverable and the same failure a moment later is not.

In the Isolated mode the image entered is ESL, which then starts the executive in U. In Off the image is the executive itself. The entry code is identical; what differs is which crate provides exec_start.