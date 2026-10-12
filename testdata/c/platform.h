/* The only things app.c needs from the platform it runs on. */
#ifndef PLATFORM_H
#define PLATFORM_H

#include <stddef.h>
#include <stdint.h>

void platform_write(const char *buf, size_t len);

/* Defined in app.c; the Cortex-M SysTick interrupt calls it too. */
void app_tick(uint32_t now);

#endif
