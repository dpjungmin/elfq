/*
 * Startup code that copies and zeroes memory with loops of its own rather
 * than memcpy and memset, written in assembly as vendor startup files are:
 * Reset_Handler copies .itcm and .data a word at a time in loops of
 * conditional instructions, and zeroes .bss counting down from its end.
 * Linked with cortex-m7.ld, with .text moved before .itcm, so that .data's
 * load image follows .itcm's in flash and the copy of .data goes on from
 * where the copy of .itcm ended.
 */
#include <stdint.h>

extern uint32_t _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

/* Code in ITCM, which Reset_Handler copies there. */
__attribute__((section(".itcm_text"), noinline)) void hot(void)
{
    PERIPHERALS[0] = 2;
}

/* In .data and .bss, three words each, so that the loops run more than
   once, and main reads what Reset_Handler sets up. */
volatile uint32_t steps[3] = {1, 2, 3};
volatile uint32_t counts[3];

int main(void)
{
    for (;;) {
        hot();
        for (int i = 0; i < 3; i++)
            counts[i] += steps[i];
    }
}

__attribute__((naked)) void Reset_Handler(void)
{
    __asm__ volatile(
        /* .itcm, from its load image to ITCM. */
        "    ldr   r1, =_siitcm\n"
        "    ldr   r2, =_sitcm\n"
        "    ldr   r3, =_eitcm\n"
        "1:  cmp   r2, r3\n"
        "    ittt  lt\n"
        "    ldrlt r0, [r1], #4\n"
        "    strlt r0, [r2], #4\n"
        "    blt   1b\n"
        /* .data, whose load image comes next, so r1 already points at
           it. */
        "    ldr   r2, =_sdata\n"
        "    ldr   r3, =_edata\n"
        "2:  cmp   r2, r3\n"
        "    ittt  lt\n"
        "    ldrlt r0, [r1], #4\n"
        "    strlt r0, [r2], #4\n"
        "    blt   2b\n"
        /* .bss, from its last word down to its first. */
        "    ldr   r1, =_sbss\n"
        "    ldr   r2, =_ebss\n"
        "    movs  r0, #0\n"
        "    subs  r2, r2, r1\n"
        "    ble   4f\n"
        "3:  subs  r2, #4\n"
        "    str   r0, [r1, r2]\n"
        "    bgt   3b\n"
        "4:  bl    main\n"
        "    b     .\n"
        "    .ltorg\n");
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
