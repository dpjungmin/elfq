/*
 * Linker script for a Cortex-M7 with tightly coupled memory (TCM). The
 * memory map follows the M7 core of an NXP i.MX RT1176, a common chip of
 * this kind: code and constants in external flash, hot code in ITCM,
 * variables in DTCM and large buffers in OCRAM. The FlexRAM banks are split
 * into 256 KiB of ITCM and 256 KiB of DTCM. A real RT1176 image also starts
 * with a boot header for the chip's boot ROM, which elfq doesn't need.
 */
ENTRY(Reset_Handler)

MEMORY
{
    ITCM  (rx)  : ORIGIN = 0x00000000, LENGTH = 256K
    DTCM  (rw)  : ORIGIN = 0x20000000, LENGTH = 256K
    OCRAM (rw)  : ORIGIN = 0x20240000, LENGTH = 512K
    FLASH (rx)  : ORIGIN = 0x30000000, LENGTH = 16M
}

_estack = ORIGIN(DTCM) + LENGTH(DTCM);

SECTIONS
{
    .isr_vector : { KEEP(*(.isr_vector)) } > FLASH

    /* Hot code, copied from flash to ITCM at startup. */
    _siitcm = LOADADDR(.itcm);
    .itcm : {
        _sitcm = .;
        *(.itcm_text*)
        *(.text.crc32 .text.apply_filter .text.filter_tap)
        . = ALIGN(4);
        _eitcm = .;
    } > ITCM AT > FLASH

    .text : { *(.text*) } > FLASH

    /* Variables, and the CRC table, which is faster to read from DTCM. It
       comes before .rodata, which would otherwise take the table. */
    _sidata = LOADADDR(.data);
    .data : {
        _sdata = .;
        *(.rodata.crc_table)
        *(.data*)
        . = ALIGN(4);
        _edata = .;
    } > DTCM AT > FLASH

    .rodata : { *(.rodata*) . = ALIGN(4); } > FLASH
    .ARM.exidx : { *(.ARM.exidx*) } > FLASH

    .bss (NOLOAD) : { _sbss = .; *(.bss*) *(COMMON) . = ALIGN(4); _ebss = .; } > DTCM

    /* Buffers too large for the TCMs. */
    .ocram_bss (NOLOAD) : { *(.ocram_bss*) } > OCRAM
}
