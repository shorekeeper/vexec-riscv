# Trap entry.
#
# Installed in stvec with mode zero, so every trap and every interrupt enters
# here. Runs with interrupts disabled: the hardware clears SIE on entry and
# nothing here sets it, which is what makes the kernel non reentrant and lets
# the dispatcher hold a single mutable reference to executive state.
#
# The frame layout must match TrapFrame in exec-hal. The size is asserted
# against the Rust structure at compile time.

    .equ  FRAME_SIZE, 288

    .section .text.trap, "ax"
    .align 4
    .global _trap_entry
_trap_entry:
    addi    sp, sp, -FRAME_SIZE

    # Every register is saved before any of them is used as a scratch. t0 is
    # written to its slot first for that reason, and only then borrowed to
    # compute the caller's stack pointer.
    sd      ra,   0*8(sp)
    sd      gp,   2*8(sp)
    sd      tp,   3*8(sp)
    sd      t0,   4*8(sp)
    sd      t1,   5*8(sp)
    sd      t2,   6*8(sp)
    sd      s0,   7*8(sp)
    sd      s1,   8*8(sp)
    sd      a0,   9*8(sp)
    sd      a1,  10*8(sp)
    sd      a2,  11*8(sp)
    sd      a3,  12*8(sp)
    sd      a4,  13*8(sp)
    sd      a5,  14*8(sp)
    sd      a6,  15*8(sp)
    sd      a7,  16*8(sp)
    sd      s2,  17*8(sp)
    sd      s3,  18*8(sp)
    sd      s4,  19*8(sp)
    sd      s5,  20*8(sp)
    sd      s6,  21*8(sp)
    sd      s7,  22*8(sp)
    sd      s8,  23*8(sp)
    sd      s9,  24*8(sp)
    sd      s10, 25*8(sp)
    sd      s11, 26*8(sp)
    sd      t3,  27*8(sp)
    sd      t4,  28*8(sp)
    sd      t5,  29*8(sp)
    sd      t6,  30*8(sp)

    # Slot 1 is the stack pointer as it was before the frame was pushed. It is
    # informational: the return path restores sp by undoing the adjustment.
    addi    t0, sp, FRAME_SIZE
    sd      t0,   1*8(sp)

    csrr    t0, sepc
    sd      t0,  31*8(sp)
    csrr    t0, sstatus
    sd      t0,  32*8(sp)
    csrr    t0, scause
    sd      t0,  33*8(sp)
    csrr    t0, stval
    sd      t0,  34*8(sp)

    # The frame address is the only argument. The dispatcher may modify sepc
    # in the frame, which is how a handler resumes past a faulting instruction.
    mv      a0, sp
    call    trap_dispatch

    # sepc and sstatus come back from the frame rather than being left in the
    # CSRs, because the dispatcher is allowed to change them.
    ld      t0,  31*8(sp)
    csrw    sepc, t0
    ld      t0,  32*8(sp)
    csrw    sstatus, t0

    ld      ra,   0*8(sp)
    ld      gp,   2*8(sp)
    ld      tp,   3*8(sp)
    ld      t1,   5*8(sp)
    ld      t2,   6*8(sp)
    ld      s0,   7*8(sp)
    ld      s1,   8*8(sp)
    ld      a0,   9*8(sp)
    ld      a1,  10*8(sp)
    ld      a2,  11*8(sp)
    ld      a3,  12*8(sp)
    ld      a4,  13*8(sp)
    ld      a5,  14*8(sp)
    ld      a6,  15*8(sp)
    ld      a7,  16*8(sp)
    ld      s2,  17*8(sp)
    ld      s3,  18*8(sp)
    ld      s4,  19*8(sp)
    ld      s5,  20*8(sp)
    ld      s6,  21*8(sp)
    ld      s7,  22*8(sp)
    ld      s8,  23*8(sp)
    ld      s9,  24*8(sp)
    ld      s10, 25*8(sp)
    ld      s11, 26*8(sp)
    ld      t3,  27*8(sp)
    ld      t4,  28*8(sp)
    ld      t5,  29*8(sp)
    ld      t6,  30*8(sp)
    # t0 last, since it was the scratch for the CSR restores above.
    ld      t0,   4*8(sp)

    addi    sp, sp, FRAME_SIZE
    sret