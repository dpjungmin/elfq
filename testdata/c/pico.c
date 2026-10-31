/*
 * Startup code laid out as the Pico SDK's crt0.S for the Raspberry Pi
 * RP2040 lays it out: on core 0, _reset_handler copies the sections that a
 * table of (load image, start, end) rows lists, up to a row of zeros, with
 * a small loop that isn't a function of its own, then zeroes .bss and
 * calls runtime_init and main. Core 1 goes back to the boot ROM, as the
 * entry point does, which is how a debugger starts the program. Linked
 * with pico.ld.
 */
#include <stdint.h>

extern uint32_t __stack_top;

#define GPIO_OUT (*(volatile uint32_t *)0xd0000010u)

/* In .data, .bss and .scratch_y, so that main reads what _reset_handler
   sets up. */
volatile uint32_t scale = 3;
volatile uint32_t counts[8];
__attribute__((section(".scratch_y.steps"))) volatile uint32_t steps[4] = {1, 2, 3, 4};

/* Code in scratch X, which _reset_handler copies there, as the SDK's
   __scratch_x functions are. */
__attribute__((section(".scratch_x.hot"), noinline)) uint32_t hot(uint32_t n)
{
    return n * scale + steps[n % 4];
}

void runtime_init(void)
{
    GPIO_OUT = 0;
}

int main(void)
{
    for (uint32_t n = 0;; n++)
        counts[n % 8] = hot(n);
}

__attribute__((noreturn)) void exit(int status)
{
    (void)status;
    for (;;)
        ;
}

void _reset_handler(void), isr_hang(void);

/* The vector table, after the boot loader's 256 bytes: the stack, then
   the reset, NMI and HardFault handlers. */
__attribute__((section(".vectors"), used)) static const void *const vector_table[] = {
    &__stack_top,
    _reset_handler,
    isr_hang,
    isr_hang,
};

__asm__(
    "    .syntax unified\n"
    "    .section .text._entry_point, \"ax\", %progbits\n"
    /* Starts the program through the boot ROM's vector table, at 0, which
       boots the chip as at reset. */
    "    .global _entry_point\n"
    "    .type _entry_point, %function\n"
    "    .thumb_func\n"
    "_entry_point:\n"
    "    movs  r0, #0\n"
    "enter_vector_table:\n"
    "    ldr   r1, =0xe000ed08\n" /* VTOR */
    "    str   r0, [r1]\n"
    "    ldmia r0!, {r1, r2}\n"
    "    msr   msp, r1\n"
    "    bx    r2\n"
    "\n"
    "    .global _reset_handler\n"
    "    .type _reset_handler, %function\n"
    "    .thumb_func\n"
    "_reset_handler:\n"
    /* SIO's CPUID, 0 on core 0: core 1 goes back to the boot ROM, which
       keeps it waiting until core 0 starts it. */
    "    ldr   r0, =0xd0000000\n"
    "    ldr   r0, [r0]\n"
    "    cmp   r0, #0\n"
    "    beq   1f\n"
    "    movs  r0, #0\n"
    "    b     enter_vector_table\n"
    /* Each row of the table, up to the row of zeros. */
    "1:  adr   r4, data_cpy_table\n"
    "2:  ldmia r4!, {r1, r2, r3}\n"
    "    cmp   r1, #0\n"
    "    beq   3f\n"
    "    bl    data_cpy\n"
    "    b     2b\n"
    /* .bss, a word at a time. */
    "3:  ldr   r1, =__bss_start__\n"
    "    ldr   r2, =__bss_end__\n"
    "    movs  r0, #0\n"
    "    b     5f\n"
    "4:  stmia r1!, {r0}\n"
    "5:  cmp   r1, r2\n"
    "    bne   4b\n"
    "    ldr   r1, =runtime_init\n"
    "    blx   r1\n"
    "    ldr   r1, =main\n"
    "    blx   r1\n"
    "    ldr   r1, =exit\n"
    "    blx   r1\n"
    /* Copies the words from r1 to r2, up to r3. */
    "data_cpy_loop:\n"
    "    ldmia r1!, {r0}\n"
    "    stmia r2!, {r0}\n"
    "data_cpy:\n"
    "    cmp   r2, r3\n"
    "    blo   data_cpy_loop\n"
    "    bx    lr\n"
    "\n"
    "    .global isr_hang\n"
    "    .type isr_hang, %function\n"
    "    .thumb_func\n"
    "isr_hang:\n"
    "    b     isr_hang\n"
    "\n"
    "    .align 2\n"
    "data_cpy_table:\n"
    "    .word __data_source__, __data_start__, __data_end__\n"
    "    .word __scratch_x_source__, __scratch_x_start__, __scratch_x_end__\n"
    "    .word __scratch_y_source__, __scratch_y_start__, __scratch_y_end__\n"
    "    .word 0\n"
    "    .ltorg\n");
