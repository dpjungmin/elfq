/* The only things app.c needs from the platform it runs on. */
#ifndef PLATFORM_H
#define PLATFORM_H

#include <stddef.h>
#include <stdint.h>

void platform_write(const char *buf, size_t len);

/* Defined in app.c; the Cortex-M SysTick interrupt calls it too. */
void app_tick(uint32_t now);

/* For the builds that diff compares with this sample's (see build.sh): with
   -DDIFF_MOVED, crc32 logs an empty frame. It is here, at the end of this
   file, and expands to nothing otherwise, so that app.c's lines, and the
   other builds' bytes, stay as they are. */
#ifdef DIFF_MOVED
#define LOG_EMPTY_FRAME(len) if ((len) == 0) log_msg("empty frame\n");
#else
#define LOG_EMPTY_FRAME(len)
#endif

#endif
