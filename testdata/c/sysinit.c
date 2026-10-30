/*
 * Startup code with a mistake for `elfq FILE startup` to find, as vendor
 * startup code can make: Reset_Handler calls SystemInit before it copies
 * .itcm, and SystemInit calls clock_setup, which is linked into ITCM, so
 * it runs before it is there. Linked with cortex-m7.ld.
 */
#include <stdint.h>
#include <string.h>

extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

#define CLOCK_ROOT0_CONTROL (*(volatile uint32_t *)0x40CC0000u)

/* In ITCM, as code that changes the clock of the flash it would otherwise
   run from must be. */
__attribute__((section(".itcm_text"), noinline)) void clock_setup(void)
{
    CLOCK_ROOT0_CONTROL = 0x100;
}

__attribute__((noinline)) void SystemInit(void)
{
    clock_setup();
}

volatile uint32_t step = 1;
volatile uint32_t count;

int main(void)
{
    for (;;)
        count += step;
}

void Reset_Handler(void)
{
    SystemInit();
    memcpy(&_sitcm, &_siitcm, (uintptr_t)&_eitcm - (uintptr_t)&_sitcm);
    memcpy(&_sdata, &_sidata, (uintptr_t)&_edata - (uintptr_t)&_sdata);
    memset(&_sbss, 0, (uintptr_t)&_ebss - (uintptr_t)&_sbss);
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
