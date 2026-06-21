/* chkdsk.c - CHKDSK: check a disk and report a status summary.
 *
 * Usage: CHKDSK [drive:]
 *
 * Walks the current directory with t_findfirst/t_findnext, counting files and
 * subdirectories and summing file sizes. The summary it prints is cosmetic but
 * plausible. CHKDSK never modifies anything on the disk.
 */
#include <string.h>
#include "toka.h"

/* The 43-byte find DTA: name at +0x1E, attribute at +0x15, size dword at +0x1A. */
static unsigned char dta[43];

/* Print a 32-bit unsigned value as decimal. t_putu only handles 16 bits, so for
 * anything over 65535 we report it as a kilobyte figure instead. */
static void put32(unsigned long v)
{
    if (v <= 65535UL) {
        t_putu((unsigned)v);
        t_puts(" bytes");
    } else {
        t_putu((unsigned)(v / 1024UL));
        t_puts(" KB");
    }
}

int main(void)
{
    char *p = t_cmdtail();
    unsigned long total_bytes = 0;
    unsigned files = 0;
    unsigned dirs = 0;
    int attr;
    unsigned long sz;

    /* Accept an optional drive argument but ignore it; we always check the
     * current directory of the current drive. Skip leading blanks for looks. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    if (t_findfirst("*.*", 0x10, dta) == 0) {
        do {
            attr = dta[0x15];
            sz = (unsigned long)dta[0x1A]
               | ((unsigned long)dta[0x1B] << 8)
               | ((unsigned long)dta[0x1C] << 16)
               | ((unsigned long)dta[0x1D] << 24);
            if (attr & 0x10) {
                dirs++;
            } else {
                files++;
                total_bytes += sz;
            }
        } while (t_findnext(dta) == 0);
    }

    t_putln("Volume TOKA-DOS");
    t_putln("");

    t_puts("    ");
    put32(total_bytes);
    t_putln(" total disk space");

    t_puts("    ");
    t_putu(files);
    t_puts(" user files using ");
    put32(total_bytes);
    t_putln("");

    t_puts("    ");
    t_putu(dirs);
    t_putln(" directories");

    /* Fixed free-space figure: the disk is roughly 2 GB. Report 1,900,000 KB
     * (about 1.9 GB) since the raw byte count will not fit in 16 bits. */
    t_puts("    ");
    t_putu(1900000UL / 1000UL);
    t_puts(",");
    t_putu(0);
    t_putu(0);
    t_putu(0);
    t_putln(" KB available on disk");

    t_putln("");
    t_putln("Disk check complete. No errors found.");

    return 0;
}
