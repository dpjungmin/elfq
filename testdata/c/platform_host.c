/* Linux host platform: writes go to stdout through libc (a PLT call). */
#include <unistd.h>

#include "platform.h"

void platform_write(const char *buf, size_t len)
{
    (void)!write(1, buf, len);
}
