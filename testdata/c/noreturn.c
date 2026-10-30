/*
 * Startup code laid out as Zephyr's is: Reset_Handler calls prep, which
 * copies .itcm and .data, zeroes .bss, and starts the program, without
 * returning. main runs as the entry of the program's first thread, which
 * start_threads calls through a pointer. Linked with cortex-m7.ld.
 */
#include <stdint.h>
#include <string.h>

extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

/* Code in ITCM, which prep copies there. */
__attribute__((section(".itcm_text"), noinline)) void hot(void)
{
    PERIPHERALS[0] = 2;
}

/* In .data and .bss, so that main reads what prep sets up. */
volatile uint32_t step = 1;
volatile uint32_t count;

int main(void)
{
    for (;;) {
        hot();
        count += step;
    }
}

/* The entry of the first thread, which start_threads runs. */
int (*volatile first_thread)(void) = main;

__attribute__((noreturn, noinline)) void start_threads(void)
{
    first_thread();
    for (;;) {
    }
}

__attribute__((noreturn, noinline)) void prep(void)
{
    memcpy(&_sitcm, &_siitcm, (uintptr_t)&_eitcm - (uintptr_t)&_sitcm);
    memcpy(&_sdata, &_sidata, (uintptr_t)&_edata - (uintptr_t)&_sdata);
    memset(&_sbss, 0, (uintptr_t)&_ebss - (uintptr_t)&_sbss);
    start_threads();
}

void Reset_Handler(void)
{
    prep();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
