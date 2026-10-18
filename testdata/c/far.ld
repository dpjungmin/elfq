/* For far.c: code in RAM at 0x0, as in a Cortex-M7's ITCM, data in RAM at
   0x20000000, code in flash at 0x30000000, and a routine in the chip's ROM
   at 0x1fff0000. Works with GNU ld and LLD. */
ENTRY(Reset_Handler)
MEMORY {
  RAM   (rwx) : ORIGIN = 0x00000000, LENGTH = 64K
  DRAM  (rw)  : ORIGIN = 0x20000000, LENGTH = 64K
  FLASH (rx)  : ORIGIN = 0x30000000, LENGTH = 1M
}
/* The ROM routine is Thumb code, so its address has bit 0 set. */
rom_routine = 0x1fff0001;
SECTIONS {
  .isr_vector : { KEEP(*(.isr_vector)) } > FLASH
  .ram : { *(.ramfunc*) } > RAM AT > FLASH
  .text : { *(.text*) *(.rodata*) } > FLASH
  .bss (NOLOAD) : { *(.bss*) *(COMMON) } > DRAM
}
