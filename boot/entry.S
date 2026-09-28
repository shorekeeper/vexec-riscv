# Kernel entry.
#
# Reached from OpenSBI in S mode with:
#   a0 = hart id
#   a1 = physical address of the flattened device tree
#
# Interrupts are disabled and satp is zero, so all addressing here is physical.

    .section .text.entry, "ax"
    .global _entry
_entry:
    # gp must be set before any gp relative access. Relaxation is disabled for
    # these two instructions because the assembler would otherwise rewrite the
    # address load into a gp relative one, using the register being set up.
    .option push
    .option norelax
    la      gp, __global_pointer$
    .option pop

    # One hart. The executive assumes it, Sedna provides only one, and on a
    # machine with more the rest are parked rather than allowed to race through
    # a boot path that is not written for them.
    bnez    a0, .Lpark

    # Stack pointer. The region is in bss and is cleared by the loop below,
    # which is harmless: nothing has been pushed yet.
    la      sp, __boot_stack_top

    # Clear bss. Rust statics are defined to start zeroed and no earlier stage
    # has done it. The region is 8 byte aligned by the linker script.
    la      t0, __bss_start
    la      t1, __bss_end
.Lbss:
    bgeu    t0, t1, .Lbss_done
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       .Lbss
.Lbss_done:

    # a0 and a1 still hold the firmware arguments and become the arguments of
    # the Rust entry point under the C ABI.
    call    exec_start

    # exec_start is divergent. Reaching here means it returned anyway.
.Lpark:
    wfi
    j       .Lpark

    .section .bss.stack, "aw", @nobits
    .align 12
__boot_stack:
    .space 65536
    .global __boot_stack_top
__boot_stack_top: