/*
 * Arithmetic on doubles, built for a Cortex-M4 without its FPU, so that
 * GCC calls libgcc's routines for it (see testdata/README.md).
 *
 * Those routines are hand-written, and some have more than one entry
 * point, each with a symbol whose size runs to the end they share:
 * __aeabi_dsub flips the sign of its second argument and runs on into
 * __adddf3, which __aeabi_dadd also names. elfq ends each entry where the
 * next starts, and has it run on into the next as a tail call.
 */
#include <stdint.h>

extern uint32_t _estack;

volatile double x, y, sum, difference, converted;
volatile uint64_t count;
volatile int less;

void Reset_Handler(void)
{
    for (;;) {
        sum = x + y;
        difference = x - y;
        converted = count;
        less = x < y;
    }
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
