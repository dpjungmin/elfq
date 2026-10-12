/*
 * Bare-metal Cortex-M4 platform (STM32F4-style memory map).
 *
 * Interrupt handlers are reached only through the vector table, which is
 * data, not code: no instruction calls them. Tools that build the call graph
 * from "call" instructions alone miss them.
 */
#include "platform.h"

#define USART2_SR (*(volatile uint32_t *)0x40004400u)
#define USART2_DR (*(volatile uint32_t *)0x40004404u)
#define USART_SR_TXE (1u << 7)

void platform_write(const char *buf, size_t len)
{
    for (size_t i = 0; i < len; i++) {
        while ((USART2_SR & USART_SR_TXE) == 0) {
        }
        USART2_DR = (uint8_t)buf[i];
    }
}

/* Symbols provided by cortex-m4.ld. */
extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;

int main(void);

void Reset_Handler(void)
{
    uint32_t *src = &_sidata;

    for (uint32_t *dst = &_sdata; dst < &_edata;)
        *dst++ = *src++;
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

void SysTick_Handler(void)
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
