/*
 * Startup code that copies and zeroes the sections that tables in flash
 * list, with a function for each kind of row, as the startup code that
 * MCUXpresso IDE writes for NXP's chips does: ResetISR goes through the
 * data table, whose rows give where a section's load image is, where it
 * runs and its length in bytes, and calls data_init for each, then
 * through the bss table, which comes next, and calls bss_init for each.
 * The tables hold .itcm and .data, and .bss and .ocram_bss. Linked with
 * cortex-m7.ld, with the tables that the linker writes before .text.
 */
#include <stdint.h>

extern uint32_t _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

/* Code in ITCM, which ResetISR copies there. */
__attribute__((section(".itcm_text"), noinline)) void hot(void)
{
    PERIPHERALS[0] = 2;
}

/* In .data, .bss and .ocram_bss, so that main reads what ResetISR sets
   up. */
volatile uint32_t step = 1;
volatile uint32_t count;
__attribute__((section(".ocram_bss"))) volatile uint32_t buffer[16];

int main(void)
{
    for (;;) {
        hot();
        count += step;
        buffer[count % 16] = count;
    }
}

extern uint32_t __data_section_table[], __data_section_table_end[];
extern uint32_t __bss_section_table_end[];

/* Without loop distribution, GCC would make the loops calls to memcpy and
   memset. */
__attribute__((noinline, optimize("no-tree-loop-distribute-patterns"))) void
data_init(uint32_t load, uint32_t start, uint32_t length)
{
    uint32_t *to = (uint32_t *)start;
    const uint32_t *from = (const uint32_t *)load;
    for (uint32_t done = 0; done < length; done += 4)
        *to++ = *from++;
}

__attribute__((noinline, optimize("no-tree-loop-distribute-patterns"))) void
bss_init(uint32_t start, uint32_t length)
{
    uint32_t *to = (uint32_t *)start;
    for (uint32_t done = 0; done < length; done += 4)
        *to++ = 0;
}

void ResetISR(void)
{
    uint32_t *row = __data_section_table;
    while (row < __data_section_table_end) {
        uint32_t load = *row++;
        uint32_t start = *row++;
        uint32_t length = *row++;
        data_init(load, start, length);
    }
    /* The bss table starts where the data table ends. */
    while (row < __bss_section_table_end) {
        uint32_t start = *row++;
        uint32_t length = *row++;
        bss_init(start, length);
    }
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    ResetISR,
};
