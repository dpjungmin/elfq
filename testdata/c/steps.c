/*
 * Startup code with more steps than `elfq FILE startup` shows without
 * --all: after Reset_Handler copies .itcm and .data and zeroes .bss, it
 * calls 32 functions that each set up a peripheral, then a hook through a
 * pointer, which elfq can't follow, and which may zero .ocram_bss, before
 * main. Linked with cortex-m7.ld.
 */
#include <stdint.h>
#include <string.h>

extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

#define PERIPHERALS ((volatile uint32_t *)0x40000000u)

#define INIT(n)                                                                \
    __attribute__((noinline)) void init_##n(void)                              \
    {                                                                          \
        PERIPHERALS[n * 0x400] = 1;                                            \
    }

INIT(0) INIT(1) INIT(2) INIT(3) INIT(4) INIT(5) INIT(6) INIT(7)
INIT(8) INIT(9) INIT(10) INIT(11) INIT(12) INIT(13) INIT(14) INIT(15)
INIT(16) INIT(17) INIT(18) INIT(19) INIT(20) INIT(21) INIT(22) INIT(23)
INIT(24) INIT(25) INIT(26) INIT(27) INIT(28) INIT(29) INIT(30) INIT(31)

/* Code in ITCM, which Reset_Handler copies there. */
__attribute__((section(".itcm_text"), noinline)) void hot(void)
{
    PERIPHERALS[0] = 2;
}

/* What the hook may zero: elfq doesn't know where it points. */
__attribute__((section(".ocram_bss"))) uint8_t frame[256];

/* In .data, so its value is set only once Reset_Handler copies .data. */
void (*volatile hook)(void) = init_0;

volatile uint32_t ticks;

int main(void)
{
    for (;;) {
        hot();
        ticks += frame[ticks % sizeof frame];
    }
}

void Reset_Handler(void)
{
    memcpy(&_sitcm, &_siitcm, (uintptr_t)&_eitcm - (uintptr_t)&_sitcm);
    memcpy(&_sdata, &_sidata, (uintptr_t)&_edata - (uintptr_t)&_sdata);
    memset(&_sbss, 0, (uintptr_t)&_ebss - (uintptr_t)&_sbss);
    init_0(); init_1(); init_2(); init_3(); init_4(); init_5(); init_6(); init_7();
    init_8(); init_9(); init_10(); init_11(); init_12(); init_13(); init_14(); init_15();
    init_16(); init_17(); init_18(); init_19(); init_20(); init_21(); init_22(); init_23();
    init_24(); init_25(); init_26(); init_27(); init_28(); init_29(); init_30(); init_31();
    hook();
    main();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
