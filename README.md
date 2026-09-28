# vExec

A statically configured executive for RV64, verified with Verus.

Amiga Exec and the NT Executive rather than a UNIX kernel: objects behind handles, message ports, a hardware abstraction layer under a portable core. One hart, run to completion, no allocation in the kernel.

## Dependencies

The only rustup components used are the target's prebuilt `core`, and `llvm-tools` if a flat image is wanted, both of which are official binaries rather than third party source.

## Layout

    crates/exec-core   portable core. forbid(unsafe_code). verified.
    crates/exec-hal    csr, trap entry, mmio. unsafe, trusted, not verified.
    boot/              entry assembly and the linker script.
    spec/              abstract machine the core is proved to refine.
    scripts/           activate, verify, build, audit.
    tools/             verus. not in version control.