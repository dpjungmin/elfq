/*
 * Float arithmetic on a Cortex-M4 with its FPU (see testdata/README.md):
 * GCC picks between float values with conditional VFP instructions inside
 * IT blocks, such as `ite gt; vmovgt.f32 s0, s14; vmovle.f32 s0, s15`.
 */
#include <stdint.h>

extern uint32_t _estack;

volatile float x, y, lo = -1.0f, hi = 1.0f, result;
volatile int sign;

/* value held between low and high. */
__attribute__((noinline)) float clampf(float value, float low, float high)
{
    if (value < low)
        return low;
    if (value > high)
        return high;
    return value;
}

/* The larger of a and b. */
__attribute__((noinline)) float maxf(float a, float b)
{
    return a > b ? a : b;
}

/* -1, 0 or 1. */
__attribute__((noinline)) int signf(float value)
{
    return (value > 0.0f) - (value < 0.0f);
}

/* a + b, or 0 when both are negative. */
__attribute__((noinline)) float positive_sum(float a, float b)
{
    float sum = a + b;
    if (a < 0.0f && b < 0.0f)
        sum = 0.0f;
    return sum;
}

/* c when a > b, else d. */
__attribute__((noinline)) float pick(float a, float b, float c, float d)
{
    return a > b ? c : d;
}

/* total + step, or total - step when total is past limit. */
__attribute__((noinline)) float step_toward(float total, float step, float limit)
{
    if (total > limit)
        total -= step;
    else
        total += step;
    return total;
}

/* Calls each function, forever. This isn't in Reset_Handler itself: elfq
 * takes an entry point that loads where a section starts and ends for
 * startup code that copies or clears it, and wouldn't count those loads as
 * uses of hi and sign. */
__attribute__((noinline, noreturn)) static void run(void)
{
    for (;;) {
        result = clampf(x, lo, hi);
        result = maxf(x, y);
        result = positive_sum(x, y);
        sign = signf(x);
        result = pick(x, y, lo, hi);
        result = step_toward(x, y, hi);
    }
}

void Reset_Handler(void)
{
    run();
}

__attribute__((section(".isr_vector"), used)) static const void *const vector_table[] = {
    &_estack,
    Reset_Handler,
};
