/* The kernel runs at KERNEL_BASE + physical address; it is loaded at 16 MiB. */
ENTRY(_start)
KERNEL_BASE = 0xffffffff80000000;
PHYS_LOAD = 0x1000000;

SECTIONS
{
    . = KERNEL_BASE + PHYS_LOAD;
    __kernel_start = .;
    .text : AT(ADDR(.text) - KERNEL_BASE) { *(.text.boot) *(.text .text.*) }
    . = ALIGN(4096);
    .rodata : AT(ADDR(.rodata) - KERNEL_BASE) { *(.rodata .rodata.*) }
    . = ALIGN(4096);
    .data : AT(ADDR(.data) - KERNEL_BASE) { *(.data .data.*) *(.got .got.*) }
    . = ALIGN(4096);
    .bss : AT(ADDR(.bss) - KERNEL_BASE) { __bss_start = .; *(.bss .bss.*) *(COMMON) __bss_end = .; }
    . = ALIGN(4096);
    __kernel_end = .;
    /DISCARD/ : { *(.eh_frame*) *(.note*) *(.comment) }
}
