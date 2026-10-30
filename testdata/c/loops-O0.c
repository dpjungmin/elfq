/*
 * Startup code in C that copies and zeroes memory with loops of its own,
 * built without optimization, as debug builds of startup code are: each
 * pointer lives on the stack, and the loops for .itcm, .data and .bss share
 * the same two, from and to, where the zeroing of .bss goes on from where
 * the copy of .data left `to`. Linked with cortex-m7.ld.
 */
#include <stdint.h>

extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

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

void Reset_Handler(void)
{
    uint32_t *from = &_siitcm;
    uint32_t *to = &_sitcm;
    while (to < &_eitcm)
        *to++ = *from++;
    from = &_sidata;
    to = &_sdata;
    while (to < &_edata)
        *to++ = *from++;
    /* .bss follows .data, where the copy left `to`. */
    while (to < &_ebss)
        *to++ = 0;
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
