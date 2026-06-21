/* mem.c - MEM: report system memory usage.
 *
 * Cosmetic only. Toka-DOS has no live memory manager wired up yet, so this
 * prints a fixed, plausible layout for an Izarra 3000: 640K conventional and
 * a block of contiguous extended memory. Values that fit in 16 bits go out
 * through t_putu; the extended-memory byte count overflows an unsigned 16-bit
 * decimal, so that one is reported in kilobytes instead.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    /* Conventional memory: 640 KB = 655360 bytes, fits in 16 bits when the
     * value is treated as unsigned (655360 > 65535, so it does NOT fit). The
     * spec text shows the byte figure, but 655360 overflows t_putu's 16-bit
     * range too, so the byte lines are printed from fixed strings and only the
     * kilobyte figures use t_putu. */
    static const char *col = "    ";

    /* Conventional memory total and free, in bytes (as text) and KB. */
    t_puts(col);
    t_puts("655360 bytes total conventional memory");
    t_putln("");

    t_puts(col);
    t_puts("655360 bytes available to Toka-DOS");
    t_putln("");

    t_putln("");

    /* Extended memory: 24 MB contiguous. 24 * 1024 * 1024 = 25165824 bytes,
     * far past 16 bits, so report the kilobyte figure with t_putu. */
    t_puts(col);
    t_putu((unsigned)(24U * 1024U));   /* 24576 K, fits in 16 bits */
    t_puts("K total contiguous extended memory");
    t_putln("");

    t_puts(col);
    t_putu((unsigned)(23U * 1024U));   /* 23552 K available */
    t_puts("K extended memory free");
    t_putln("");

    t_putln("");

    /* A short kilobyte summary line for the conventional region. */
    t_puts(col);
    t_putu((unsigned)640);
    t_puts("K total conventional memory");
    t_putln("");

    t_exit(0);
    return 0;
}
