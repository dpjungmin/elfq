/*
 * A shared library whose code calls its own functions through each kind of
 * PLT stub that GNU ld makes without Intel CET (built with
 * -fcf-protection=none; see testdata/README.md):
 *
 * - twice and pick, through 16-byte stubs in .plt;
 * - thrice and negate, whose addresses pick also takes, through 8-byte
 *   stubs in .plt.got, between which ld puts the import __cxa_finalize;
 * - sum_to, an ifunc: sum_to_resolver picks sum_fast or sum_plain when the
 *   library is loaded, and code calls it through a stub in .plt;
 * - square, an ifunc that code only takes the address of, which is the
 *   address of its stub in .plt.
 */

typedef int (*operation)(int);

int twice(int x) { return 2 * x; }

int thrice(int x) { return 3 * x; }

int negate(int x) { return -x; }

operation pick(int which) { return which ? thrice : negate; }

static int sum_plain(int n) {
    int sum = 0;
    for (int i = 1; i <= n; i++)
        sum += i;
    return sum;
}

static int sum_fast(int n) { return n * (n + 1) / 2; }

/* A real resolver would look at the processor; this one reads a variable
 * that the compiler can't assume the value of. */
static volatile int fast = 1;

static operation sum_to_resolver(void) { return fast ? sum_fast : sum_plain; }

static int sum_to(int n) __attribute__((ifunc("sum_to_resolver")));

static int square_any(int x) { return x * x; }

static operation square_resolver(void) { return square_any; }

static int square(int x) __attribute__((ifunc("square_resolver")));

operation pick_square(void) { return square; }

int run(int x) {
    return twice(x) + thrice(x) + negate(x) + sum_to(x) + pick(x)(x);
}
