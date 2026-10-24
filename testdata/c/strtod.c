/*
 * A call to newlib's strtod, on a Cortex-M4 with its FPU (see
 * testdata/README.md). newlib's _strtod_l starts with a switch on the
 * first character that GCC built as a table of addresses that
 * `ldr.w pc, [r0, r2, lsl #2]` jumps through, not as a tbb or tbh table.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

extern uint32_t _estack;

volatile const char *text = "-12.5e3";
volatile double value;

/* The heap that strtod's big numbers come from, as malloc asks for it. */
void *_sbrk(ptrdiff_t increment)
{
    static uint8_t heap[4096];
    static size_t used;

    if (increment < 0 || (size_t)increment > sizeof heap - used)
        return (void *)-1;
    used += (size_t)increment;
    return heap + used - increment;
}

void Reset_Handler(void)
{
    for (;;)
        value = strtod((const char *)text, 0);
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
