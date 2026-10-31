/*
 * Startup code that sets up memory with functions from the chip's boot ROM,
 * as code for chips whose ROM offers a table of functions can: one that
 * takes a list of regions to copy, which copies .data, and one that fills
 * memory as memset does, which zeroes .bss. The ROM isn't in the file, so
 * elfq can't tell what the first does, and says so, but the second takes
 * what memset would to zero .bss. Reset_Handler copies .itcm with a loop
 * of its own. Linked with cortex-m7.ld.
 */
#include <stdint.h>

extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

/* A region for the ROM to copy: where its load image is, where it runs,
   and its length in bytes. */
struct region {
    const void *from;
    void *to;
    uint32_t bytes;
};

/* The boot ROM's table of functions, at a fixed address in the ROM. */
struct rom_functions {
    uint32_t version;
    void (*copy_regions)(const struct region *regions, uint32_t count);
    void *(*fill)(void *to, int value, uint32_t bytes);
};
#define ROM_FUNCTIONS (*(const struct rom_functions *const *)0x0021001cu)

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

/* Without loop distribution, GCC would make the loops calls to memcpy and
   memset. */
__attribute__((optimize("no-tree-loop-distribute-patterns"))) void Reset_Handler(void)
{
    for (uint32_t *from = &_siitcm, *to = &_sitcm; to < &_eitcm;)
        *to++ = *from++;
    const struct region data = {&_sidata, &_sdata, (uintptr_t)&_edata - (uintptr_t)&_sdata};
    ROM_FUNCTIONS->copy_regions(&data, 1);
    ROM_FUNCTIONS->fill(&_sbss, 0, (uintptr_t)&_ebss - (uintptr_t)&_sbss);
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
