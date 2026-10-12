/*
 * Bare-metal platform in the style of an NXP i.MX RT1176's Cortex-M7 core,
 * whose memory comes in several kinds:
 *
 *   ITCM   0x00000000  fast memory for code, next to the core
 *   DTCM   0x20000000  fast memory for data, next to the core
 *   OCRAM  0x20240000  larger on-chip RAM
 *   flash  0x30000000  external flash, read through FlexSPI
 *
 * rt1176.ld puts the hot signal-processing functions and the SysTick handler
 * in ITCM, variables and the CRC table in DTCM, the UART's buffer in OCRAM,
 * and everything else in flash. Calls between flash and ITCM are too far for
 * one branch instruction, so the linker adds veneers: small stubs that load
 * the target's address and jump to it.
 */
#include "platform.h"

#define LPUART1_STAT (*(volatile uint32_t *)0x4007C014u)
#define LPUART1_DATA (*(volatile uint32_t *)0x4007C01Cu)
#define LPUART_STAT_TDRE (1u << 23)

#define ITCM_CODE __attribute__((section(".itcm_text"), noinline))
#define OCRAM_DATA __attribute__((section(".ocram_bss")))

/* Too large for the TCMs, so it goes to OCRAM. */
static OCRAM_DATA char tx_buffer[4096];
static size_t tx_head, tx_tail;

void platform_write(const char *buf, size_t len)
{
    /* Queue the bytes, then send everything queued. */
    for (size_t i = 0; i < len; i++) {
        tx_buffer[tx_head] = buf[i];
        tx_head = (tx_head + 1) % sizeof tx_buffer;
    }
    while (tx_tail != tx_head) {
        while ((LPUART1_STAT & LPUART_STAT_TDRE) == 0) {
        }
        LPUART1_DATA = (uint8_t)tx_buffer[tx_tail];
        tx_tail = (tx_tail + 1) % sizeof tx_buffer;
    }
}

/* Symbols provided by rt1176.ld. */
extern uint32_t _siitcm, _sitcm, _eitcm;
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

int main(void);

static void copy(uint32_t *dst, const uint32_t *end, const uint32_t *src)
{
    while (dst < end)
        *dst++ = *src++;
}

void Reset_Handler(void)
{
    copy(&_sitcm, &_eitcm, &_siitcm);
    copy(&_sdata, &_edata, &_sidata);
    for (uint32_t *dst = &_sbss; dst < &_ebss;)
        *dst++ = 0;
    main();
    for (;;) {
    }
}

void Default_Handler(void)
{
    for (;;) {
    }
}

static uint32_t systick_count;

/* Runs every millisecond, so it is kept in ITCM. */
ITCM_CODE void SysTick_Handler(void)
{
    app_tick(++systick_count);
}

void NMI_Handler(void) __attribute__((weak, alias("Default_Handler")));
void HardFault_Handler(void) __attribute__((weak, alias("Default_Handler")));
void MemManage_Handler(void) __attribute__((weak, alias("Default_Handler")));
void BusFault_Handler(void) __attribute__((weak, alias("Default_Handler")));
void UsageFault_Handler(void) __attribute__((weak, alias("Default_Handler")));
void SVC_Handler(void) __attribute__((weak, alias("Default_Handler")));
void DebugMon_Handler(void) __attribute__((weak, alias("Default_Handler")));
void PendSV_Handler(void) __attribute__((weak, alias("Default_Handler")));

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
    NMI_Handler,
    HardFault_Handler,
    MemManage_Handler,
    BusFault_Handler,
    UsageFault_Handler,
    0,
    0,
    0,
    0,
    SVC_Handler,
    DebugMon_Handler,
    0,
    PendSV_Handler,
    SysTick_Handler,
};
