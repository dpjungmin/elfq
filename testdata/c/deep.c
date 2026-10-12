/*
 * A program whose trees are deeper than elfq draws in one piece, for the
 * tests of trees that continue below (see testdata/README.md):
 *
 * - chain1 calls chain2, which calls chain3, and so on to chain20, which
 *   uses chain_table: a chain of calls 20 deep.
 * - stage1 to stage20 call each other the same way, but stage1 to stage19
 *   also call a check that only they use, so their tree branches at every
 *   level.
 * - step1 to step20 are inlined into each other, and all of them into
 *   run_steps, so its inlined code is 20 deep too. step20 reads
 *   chain_table, so chain1 shares the table with run_steps.
 *
 * The constants differ from function to function so that the compiler
 * can't merge any of them.
 */
#include <stdio.h>

unsigned char chain_table[1024] = {1};
unsigned char stage_table[1024] = {2};

/* A chain of calls. */

__attribute__((noinline)) unsigned chain20(unsigned x)
{
    return chain_table[x % sizeof chain_table] + x;
}

#define CHAIN(n, next)                                    \
    __attribute__((noinline)) unsigned chain##n(unsigned x) \
    {                                                     \
        return chain##next(x * n + 1) ^ n;                \
    }

CHAIN(19, 20)
CHAIN(18, 19)
CHAIN(17, 18)
CHAIN(16, 17)
CHAIN(15, 16)
CHAIN(14, 15)
CHAIN(13, 14)
CHAIN(12, 13)
CHAIN(11, 12)
CHAIN(10, 11)
CHAIN(9, 10)
CHAIN(8, 9)
CHAIN(7, 8)
CHAIN(6, 7)
CHAIN(5, 6)
CHAIN(4, 5)
CHAIN(3, 4)
CHAIN(2, 3)
CHAIN(1, 2)

/* A chain of calls with a branch at every level. */

__attribute__((noinline)) unsigned stage20(unsigned x)
{
    return stage_table[x % sizeof stage_table] + x;
}

#define STAGE(n, next)                                          \
    __attribute__((noinline)) unsigned check##n(unsigned x)       \
    {                                                           \
        for (unsigned i = 0; i < n % 4 + 2; i++)                \
            x = x * 31 + n;                                     \
        return x;                                               \
    }                                                           \
    __attribute__((noinline)) unsigned stage##n(unsigned x)       \
    {                                                           \
        return stage##next(check##n(x) * n + 1) ^ n;            \
    }

STAGE(19, 20)
STAGE(18, 19)
STAGE(17, 18)
STAGE(16, 17)
STAGE(15, 16)
STAGE(14, 15)
STAGE(13, 14)
STAGE(12, 13)
STAGE(11, 12)
STAGE(10, 11)
STAGE(9, 10)
STAGE(8, 9)
STAGE(7, 8)
STAGE(6, 7)
STAGE(5, 6)
STAGE(4, 5)
STAGE(3, 4)
STAGE(2, 3)
STAGE(1, 2)

/* Inlined code nested 20 deep. */

static inline __attribute__((always_inline)) unsigned step20(unsigned x)
{
    return x * 7 + chain_table[x % 8];
}

#define STEP(n, next)                                                    \
    static inline __attribute__((always_inline)) unsigned step##n(unsigned x) \
    {                                                                    \
        return step##next(x + n) ^ (x >> (n % 7 + 1));                  \
    }

STEP(19, 20)
STEP(18, 19)
STEP(17, 18)
STEP(16, 17)
STEP(15, 16)
STEP(14, 15)
STEP(13, 14)
STEP(12, 13)
STEP(11, 12)
STEP(10, 11)
STEP(9, 10)
STEP(8, 9)
STEP(7, 8)
STEP(6, 7)
STEP(5, 6)
STEP(4, 5)
STEP(3, 4)
STEP(2, 3)
STEP(1, 2)

__attribute__((noinline)) unsigned run_steps(unsigned x)
{
    return step1(x);
}

int main(int argc, char **argv)
{
    (void)argv;
    printf("%u %u %u\n", chain1(argc), stage1(argc), run_steps(argc));
    return 0;
}
