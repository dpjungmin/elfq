/*
 * Linker script for a Raspberry Pi RP2040, laid out as the Pico SDK lays
 * out a program that runs from flash: the second-stage boot loader's 256
 * bytes, then the vector table and code in flash, variables in the main
 * RAM, and code and variables for each core in the two 4 KiB scratch banks,
 * all three copied from flash at startup by the table that pico.c ends
 * with. The boot loader here is only zeros, which elfq doesn't need.
 */
ENTRY(_entry_point)

MEMORY
{
    FLASH     (rx)  : ORIGIN = 0x10000000, LENGTH = 2048K
    RAM       (rwx) : ORIGIN = 0x20000000, LENGTH = 256K
    SCRATCH_X (rwx) : ORIGIN = 0x20040000, LENGTH = 4K
    SCRATCH_Y (rwx) : ORIGIN = 0x20041000, LENGTH = 4K
}

__stack_top = ORIGIN(SCRATCH_Y) + LENGTH(SCRATCH_Y);

SECTIONS
{
    .boot2 : { BYTE(0) . = 256; } > FLASH

    .text : {
        KEEP(*(.vectors))
        *(.text*)
        *(.rodata*)
        . = ALIGN(4);
    } > FLASH
    .ARM.exidx : { *(.ARM.exidx*) } > FLASH

    .data : {
        __data_start__ = .;
        *(.data*)
        . = ALIGN(4);
        __data_end__ = .;
    } > RAM AT > FLASH
    __data_source__ = LOADADDR(.data);

    .bss (NOLOAD) : {
        __bss_start__ = .;
        *(.bss*) *(COMMON)
        . = ALIGN(4);
        __bss_end__ = .;
    } > RAM

    .scratch_x : {
        __scratch_x_start__ = .;
        *(.scratch_x*)
        . = ALIGN(4);
        __scratch_x_end__ = .;
    } > SCRATCH_X AT > FLASH
    __scratch_x_source__ = LOADADDR(.scratch_x);

    .scratch_y : {
        __scratch_y_start__ = .;
        *(.scratch_y*)
        . = ALIGN(4);
        __scratch_y_end__ = .;
    } > SCRATCH_Y AT > FLASH
    __scratch_y_source__ = LOADADDR(.scratch_y);
}
