/* Calls too far for one branch instruction, for range checks.
 *
 * Code runs from RAM at 0x0, as from a Cortex-M7's ITCM, and from flash at
 * 0x30000000, and calls a routine in the chip's ROM at 0x1fff0000 that only
 * the linker script names (see far.ld). They are further apart than the
 * 16 MiB that `bl` reaches, so each call between them goes one of two ways:
 *
 * - The linker puts a veneer next to the call, which jumps on to the
 *   target. GNU ld and LLD each have a few kinds; build.sh makes a sample
 *   with each.
 * - The compiler puts the target's address in a register and branches
 *   through it, for a function declared long_call.
 */

#define RAM __attribute__((section(".ramfunc"), noinline))
#define FLASH __attribute__((noinline))

volatile unsigned counter;

/* At 0x1fff0000, from far.ld. */
void rom_routine(void);

/* Called through a register from RAM. */
__attribute__((long_call)) void flash_far(void);

FLASH void flash_work(void) { counter += 3; }
FLASH void flash_tail(void) { counter += 5; }
RAM void ram_leaf(void) { counter += 7; }

/* A call, through a veneer. */
RAM void ram_calls_flash(void) { counter++; flash_work(); counter++; }
/* A tail call, through a veneer. Cortex-M0 has no tail calls this far. */
RAM void ram_tails_flash(void) { counter++; flash_tail(); }
/* A call to the ROM, through a veneer. */
RAM void ram_calls_rom(void) { counter++; rom_routine(); counter++; }
/* A tail call to the ROM, through a veneer. */
RAM void ram_tails_rom(void) { counter++; rom_routine(); }
/* A long call, through a register. */
RAM void ram_long_calls_flash(void) { counter++; flash_far(); counter++; }

FLASH void flash_calls_ram(void) { counter++; ram_leaf(); counter++; }
FLASH void flash_calls_rom(void) { counter++; rom_routine(); counter++; }
FLASH void flash_far(void) { counter += 9; }

void Reset_Handler(void)
{
    ram_calls_flash();
    ram_tails_flash();
    ram_calls_rom();
    ram_tails_rom();
    ram_long_calls_flash();
    flash_calls_ram();
    flash_calls_rom();
    for (;;) {
    }
}

__attribute__((section(".isr_vector"), used))
static void (*const vector_table[])(void) = {
    (void (*)(void))0x20010000,
    Reset_Handler,
};
