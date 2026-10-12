/* Minimal linker script for a Cortex-M4 with 256 KiB flash and 64 KiB RAM. */
ENTRY(Reset);

MEMORY
{
    FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 256K
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 64K
}

_stack_start = ORIGIN(RAM) + LENGTH(RAM);

SECTIONS
{
    .vector_table ORIGIN(FLASH) :
    {
        LONG(_stack_start);
        KEEP(*(.vector_table.reset_vector));
        KEEP(*(.vector_table.exceptions));
    } > FLASH

    .text : { *(.text .text.*) } > FLASH
    .rodata : { *(.rodata .rodata.*) . = ALIGN(4); } > FLASH

    .data : { _sdata = .; *(.data .data.*) . = ALIGN(4); _edata = .; } > RAM AT > FLASH
    _sidata = LOADADDR(.data);
    .bss (NOLOAD) : { _sbss = .; *(.bss .bss.*) . = ALIGN(4); _ebss = .; } > RAM

    /DISCARD/ : { *(.ARM.exidx .ARM.exidx.*) }
}
