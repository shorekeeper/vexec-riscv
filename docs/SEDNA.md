# Sedna, the deployment machine

What was learned by reading the OC2R jar. The mod ships Sedna as a nested jar at META-INF/jarjar/sedna.jar, 22 MB of the 25 MB total, so anything looking for classes in the outer archive finds nothing. Both archives have to be opened to see the machine.

## Boot contract

The firmware is OpenSBI fw_jump, shipped as generated/fw_jump.bin in the outer jar and starting with the instruction sequence 33 04 05 00, which is mv s0,a0. This is the same firmware QEMU virt runs, so the contract is identical on both platforms: control arrives in S mode with the hart id in a0 and the physical address of the device tree in a1.

The image the mod ships as generated/Image is 3.5 MB and starts with d1 a0 00 00, which is neither ELF nor the Linux Image header. The header magic RISCV does not appear at offset 56. The format was not identified and the class that loads it was not found: FirmwareLoader in the mod API is an interface with an empty constant pool, and the package li/cil/sedna/elf holds thirteen classes whose names were not enumerated. This is the one open question for the port and the first thing to settle.

## Memory layout

R5MemoryRangeAllocationStrategy declares PHYSICAL_MEMORY_FIRST at 0x80000000 and PHYSICAL_MEMORY_LAST at 0xffffffff, so RAM begins at the same address as QEMU virt and the linker script needs no change. Device memory occupies DEVICE_MEMORY_FIRST at 0x10000000 through DEVICE_MEMORY_LAST at 0x7fffffff.

The amount of RAM depends on the memory modules installed in the in-game computer and may be as little as eight megabytes. The page allocator bitmap is sized accordingly rather than for the larger machine.

## Fixed device addresses

R5Board declares three constants: SYSCON at 0x1000000, CLINT at 0x2000000 and PLIC at 0xc000000. The PLIC sits at the same address as on QEMU virt.

Every other device address is dynamic. R5MemoryRangeAllocationStrategy has findFreeRange and the mod API exposes MemoryRangeAllocator, so Sedna assigns ranges as devices register. This is the reason the device tree is parsed rather than hardcoded, and the existing parser handles it without change.

## Device tree

The tree is built at run time rather than shipped. DeviceTreeImpl assembles it from per device providers in li/cil/sedna/devicetree/provider, so a real tree reaches the payload in a1.

The property names present are the standard ones: compatible, reg, device_type, timebase-frequency, clock-frequency, interrupts, interrupt-parent, interrupt-controller, interrupts-extended, #address-cells, #size-cells, #interrupt-cells, bootargs, memory, mmu-type, phandle, ranges, soc, status.

The compatible values are google,goldfish-rtc for the real time clock, riscv,clint0 for the core local interrupter, riscv,plic0 for the interrupt controller, virtio,mmio for the transports, and ns16550a for the serial port. The string riscv,ndev is a PLIC property rather than a compatible value.

## Serial console

UART16550AProvider writes compatible as ns16550a and clock-frequency as 3686400, the same value QEMU uses. The existing console driver works unchanged.

## What the parser will see

The platform parser recognises ns16550a, riscv,plic0 and virtio,mmio directly. The goldfish real time clock, the syscon and the CLINT are known hardware this kernel deliberately does not drive and are classified as Ignored so they do not inflate the count of unrecognised nodes.

## Open questions

The payload format and load address are unknown. FirmwareLoader is an interface with no implementation in the API; the loader lives in the mod, and the classes named Firmware, VMDevice and VMDeviceLoadResult were not read.

The PLIC context number is assumed to be 2*hart+1 by convention. This holds on QEMU and was not confirmed for Sedna. A wrong context means external interrupts never arrive and the console receive path is silent, which is diagnosable by the spurious counter staying at zero while typing produces nothing.

Whether the debug console SBI extension is present was not checked. The console falls back to the legacy putchar call, so output works either way, but the fallback costs one ecall per byte on an interpreted machine.
