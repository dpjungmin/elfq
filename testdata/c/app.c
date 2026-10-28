/*
 * Sample program for exercising elfq.
 *
 * It decodes a few hard-coded "sensor frames", filters the samples and prints
 * a report. What it computes does not matter: each function exists to create
 * a specific shape in the call graph or a specific DWARF pattern. The table in
 * testdata/README.md lists which function demonstrates what.
 *
 * The same file is built for x86-64 Linux (platform_host.c) and for a
 * Cortex-M4 microcontroller (platform_cortexm.c).
 */
#include <stddef.h>
#include <stdint.h>

#include "platform.h"

#define NOINLINE __attribute__((noinline))
#define ALWAYS_INLINE static inline __attribute__((always_inline))

/* ------------------------------------------------------------------------- */
/* Shared helper: called from many places, so it is never exclusive to one.  */
/* ------------------------------------------------------------------------- */

NOINLINE void log_msg(const char *msg)
{
    size_t len = 0;

    while (msg[len] != '\0')
        len++;
    platform_write(msg, len);
}

/* ------------------------------------------------------------------------- */
/* Event handlers: only reachable through a function pointer table.          */
/* ------------------------------------------------------------------------- */

typedef void (*handler_fn)(uint32_t arg);

enum { EVT_TIMER, EVT_BUTTON, EVT_DATA, EVT_COUNT };

static handler_fn handlers[EVT_COUNT];
static uint32_t timer_ticks;
static uint32_t button_presses;
static uint32_t data_events;

NOINLINE static void on_timer(uint32_t arg)
{
    timer_ticks += arg;
}

NOINLINE static void on_button(uint32_t arg)
{
    button_presses += arg;
    log_msg("button\n");
}

NOINLINE static void on_data(uint32_t arg)
{
    data_events += arg;
}

NOINLINE void register_handler(unsigned id, handler_fn fn)
{
    if (id < EVT_COUNT)
        handlers[id] = fn;
}

/* The call through handlers[] is indirect: its target is unknown statically. */
NOINLINE void dispatch_event(unsigned id, uint32_t arg)
{
    if (id < EVT_COUNT && handlers[id] != NULL)
        handlers[id](arg);
}

/* Ends in a direct tail call, which the compiler emits as a jump, not a call. */
NOINLINE void app_tick(uint32_t now)
{
    if (now % 100u == 0)
        log_msg("tick\n");
    dispatch_event(EVT_TIMER, now);
}

/* ------------------------------------------------------------------------- */
/* process_frame: the "expensive function" a user would investigate.        */
/* crc32, decode_header, apply_filter and filter_tap are reachable only      */
/* through it; log_msg and dispatch_event are shared with other callers.     */
/* ------------------------------------------------------------------------- */

/* 1 KiB of .rodata that only crc32() reads. */
static const uint32_t crc_table[256] = {
#include "crc32_table.inc"
};

NOINLINE static uint32_t crc32(const uint8_t *data, size_t len)
{
    uint32_t crc = 0xffffffffu;
    LOG_EMPTY_FRAME(len)
    for (size_t i = 0; i < len; i++)
        crc = crc_table[(crc ^ data[i]) & 0xffu] ^ (crc >> 8);
    return ~crc;
}

struct header {
    uint16_t magic;
    uint16_t length;
    uint8_t kind;
};

#define FRAME_MAGIC EDITED(0xE1F0u, 0xE1F2u)
#define HEADER_SIZE 5u
#define CRC_SIZE 4u
#define MAX_SAMPLES 32u

DIFF_INLINED_FN int decode_header(const uint8_t *buf, size_t len, struct header *out)
{
    if (len < HEADER_SIZE)
        return -1;
    out->magic = (uint16_t)(buf[0] | buf[1] << 8);
    out->length = (uint16_t)(buf[2] | buf[3] << 8);
    out->kind = buf[4];
    return out->magic == FRAME_MAGIC ? 0 : -1;
}

ALWAYS_INLINE uint32_t load_le32(const uint8_t *p)
{
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

ALWAYS_INLINE int16_t saturate(int32_t v)
{
    if (v > INT16_MAX)
        return INT16_MAX;
    if (v < INT16_MIN)
        return INT16_MIN;
    return (int16_t)v;
}

static const int16_t fir_coeffs[16] = {
    -120, -340, 210, 1830, 4100, 6620, 8300, 8900,
    8300, 6620, 4100, 1830, 210, -340, -120, EDITED(40, 42),
};

/* Only called by apply_filter, so it is exclusive to process_frame too. */
NOINLINE static int32_t filter_tap(const int16_t *x, size_t i)
{
    int32_t acc = 0;

    for (size_t k = 0; k < 16 && k <= i; k++)
        acc += (int32_t)x[i - k] * fir_coeffs[k];
    return acc;
}

NOINLINE static void apply_filter(int16_t *samples, size_t n)
{
    int16_t out[MAX_SAMPLES];

    for (size_t i = 0; i < n; i++)
        out[i] = saturate(filter_tap(samples, i) >> 15);
    for (size_t i = 0; i < n; i++)
        samples[i] = out[i];
}

/* Inlined into process_frame: its code counts toward process_frame's size. */
ALWAYS_INLINE size_t unpack_samples(const uint8_t *payload, size_t len, int16_t *out)
{
    size_t n = len / 2;

    if (n > MAX_SAMPLES)
        n = MAX_SAMPLES;
    for (size_t i = 0; i < n; i++)
        out[i] = (int16_t)(payload[2 * i] | payload[2 * i + 1] << 8);
    return n;
}

struct stats {
    int32_t min;
    int32_t max;
    int64_t sum;
    uint64_t energy;
    uint32_t count;
    uint32_t clipped;
};

static struct stats stats = { INT32_MAX, INT32_MIN, 0, 0, 0, 0 };

/* Inlined into process_frame as well. */
ALWAYS_INLINE void update_stats(const int16_t *samples, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        int32_t v = samples[i];

        if (v < stats.min)
            stats.min = v;
        if (v > stats.max)
            stats.max = v;
        if (v == INT16_MAX || v == INT16_MIN)
            stats.clipped++;
        stats.sum += v;
        stats.energy += (uint64_t)((int64_t)v * v);
        stats.count++;
    }
}

static uint32_t frames_ok;
static uint32_t frames_bad;

NOINLINE int process_frame(const uint8_t *buf, size_t len)
{
    struct header h;
    int16_t samples[MAX_SAMPLES];
    size_t n;

    if (decode_header(buf, len, &h) != 0 || HEADER_SIZE + h.length + CRC_SIZE > len) {
        frames_bad++;
        log_msg("bad header\n");
        return -1;
    }
    if (crc32(buf + HEADER_SIZE, h.length) != load_le32(buf + HEADER_SIZE + h.length)) {
        frames_bad++;
        log_msg("bad crc\n");
        return -1;
    }

    n = unpack_samples(buf + HEADER_SIZE, h.length, samples);
    apply_filter(samples, n);
    update_stats(samples, n);
    dispatch_event(h.kind % EVT_COUNT, (uint32_t)n);
    frames_ok++;
    return 0;
}

/* ------------------------------------------------------------------------- */
/* Configuration parser: parse_value and parse_list are mutually recursive,  */
/* which forms a cycle in the call graph.                                    */
/* ------------------------------------------------------------------------- */

static uint32_t config_sum;

NOINLINE static const char *parse_value(const char *p, unsigned depth);

NOINLINE static const char *parse_list(const char *p, unsigned depth)
{
    p++; /* '[' */
    while (*p != '\0' && *p != ']') {
        p = parse_value(p, depth + 1);
        if (*p == ',')
            p++;
    }
    return *p == ']' ? p + 1 : p;
}

NOINLINE static const char *parse_value(const char *p, unsigned depth)
{
    uint32_t v = 0;

    if (*p == '[')
        return parse_list(p, depth);
    while (*p >= '0' && *p <= '9')
        v = v * 10 + (uint32_t)(*p++ - '0');
    config_sum += v * depth;
    return p;
}

/* ------------------------------------------------------------------------- */
/* Reporting.                                                                */
/* ------------------------------------------------------------------------- */

struct node {
    const struct node *left;
    const struct node *right;
};

static const struct node leaf_a = { NULL, NULL };
static const struct node leaf_b = { NULL, NULL };
static const struct node leaf_c = { NULL, NULL };
static const struct node inner = { &leaf_b, &leaf_c };
static const struct node root = { &leaf_a, &inner };

/* Directly recursive: a self-loop in the call graph. */
NOINLINE static unsigned count_nodes(const struct node *n)
{
    if (n == NULL)
        return 0;
    return 1 + count_nodes(n->left) + count_nodes(n->right);
}

/* Only called by report. */
NOINLINE static char *format_u32(char *p, uint32_t v)
{
    char digits[10];
    int i = 0;

    do {
        digits[i++] = (char)('0' + v % 10);
        v /= 10;
    } while (v != 0);
    while (i > 0)
        *p++ = digits[--i];
    return p;
}

ALWAYS_INLINE char *append(char *p, const char *s)
{
    while (*s != '\0')
        *p++ = *s++;
    return p;
}

NOINLINE void report(void)
{
    char line[96];
    char *p = line;

    p = append(p, "ok=");
    p = format_u32(p, frames_ok);
    p = append(p, " bad=");
    p = format_u32(p, frames_bad);
    p = append(p, " samples=");
    p = format_u32(p, stats.count);
    p = append(p, " clipped=");
    p = format_u32(p, stats.clipped);
    p = append(p, " config=");
    p = format_u32(p, config_sum);
    p = append(p, " nodes=");
    p = format_u32(p, count_nodes(&root));
    *p++ = '\n';
    *p = '\0';
    log_msg(line);
}

/* Never called. Kept on the host build, removed by --gc-sections on Cortex-M. */
NOINLINE void debug_dump(void)
{
    log_msg("debug: ");
    report();
}

/* ------------------------------------------------------------------------- */
/* Entry point.                                                              */
/* ------------------------------------------------------------------------- */

static const uint8_t frame_a[] = {
    0xf0, 0xe1, 0x10, 0x00, 0x01, 0x64, 0x00, 0x38, 0xff, 0x2c, 0x01, 0x70, 0xfe,
    0xf4, 0x01, 0xa8, 0xfd, 0xbc, 0x02, 0xe0, 0xfc, 0xa4, 0xf6, 0x8a, 0xf6,
};

static const uint8_t frame_b[] = {
    0xf0, 0xe1, 0x08, 0x00, 0x02, 0xe8, 0x03, 0xd0, 0x07, 0x48, 0xf4, 0xa0, 0x0f,
    0xb9, 0x31, 0x34, 0x76,
};

/* Bad magic: exercises the error path. */
static const uint8_t frame_c[] = { 0x00, 0x00, 0x00, 0x00, 0x00 };

int main(void)
{
    register_handler(EVT_TIMER, on_timer);
    register_handler(EVT_BUTTON, on_button);
    register_handler(EVT_DATA, on_data);
    parse_value("[1,[2,3],[4,[5]]]", 0);

    process_frame(frame_a, sizeof(frame_a));
    process_frame(frame_b, sizeof(frame_b));
    process_frame(frame_c, sizeof(frame_c));
    for (uint32_t t = 0; t <= 200; t += 50)
        app_tick(t);
    dispatch_event(EVT_BUTTON, 1);
    report();
    return 0;
}
