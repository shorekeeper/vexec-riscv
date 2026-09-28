OUTPUT_ARCH(riscv)
ENTRY(_entry)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    __image_start = .;

    .text : ALIGN(4096)
    {
        __text_start = .;
        /* KEEP because --gc-sections has no reference to reach the entry
           point from: nothing calls it, the firmware jumps to it. */
        KEEP(*(.text.entry))
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : ALIGN(4096)
    {
        __rodata_start = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    .data : ALIGN(4096)
    {
        __data_start = .;
        /* gp points into the middle of the small data area so that the 12 bit
           signed offset of a gp relative access reaches both ends of it. */
        PROVIDE(__global_pointer$ = . + 0x800);
        *(.sdata .sdata.*)
        *(.data .data.*)
        . = ALIGN(4096);
        __data_end = .;
    }

    .bss : ALIGN(4096)
    {
        __bss_start = .;
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        . = ALIGN(8);
        __bss_end = .;
    }

    . = ALIGN(4096);
    __image_end = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}