/*
 * Startup code that copies and zeroes the sections that tables in flash
 * list, a row for each, as CMSIS's startup code in C does: Reset_Handler
 * copies .itcm and .data, the two rows of the copy table, and zeroes .bss,
 * the one row of the zero table. Linked with cortex-m7.ld, with the tables
 * that the linker writes, as CMSIS's linker scripts do, before .text.
 */
#include <stdint.h>

extern uint32_t _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

/* Code in ITCM, which Reset_Handler copies there. */
__attribute__((section(".itcm_text"), noinline)) void hot(void)
{
    PERIPHERALS[0] = 2;
}

/* In .data and .bss, so that main reads what Reset_Handler sets up. */
volatile uint32_t step = 1;
volatile uint32_t count;

int main(void)
{
    for (;;) {
        hot();
        count += step;
    }
}

/* A row of each table: where a section's load image is, where it runs,
   and its length in words. */
struct copy {
    const uint32_t *from;
    uint32_t *to;
    uint32_t words;
};

struct zero {
    uint32_t *to;
    uint32_t words;
};

extern const struct copy __copy_table_start__[], __copy_table_end__[];
extern const struct zero __zero_table_start__[], __zero_table_end__[];

/* Without loop distribution, GCC would make the loops calls to memcpy and
   memset. */
__attribute__((optimize("no-tree-loop-distribute-patterns"))) void Reset_Handler(void)
{
    for (const struct copy *row = __copy_table_start__; row < __copy_table_end__; row++)
        for (uint32_t i = 0; i < row->words; i++)
            row->to[i] = row->from[i];
    for (const struct zero *row = __zero_table_start__; row < __zero_table_end__; row++)
        for (uint32_t i = 0; i < row->words; i++)
            row->to[i] = 0;
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
