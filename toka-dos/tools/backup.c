/* backup.c - BACKUP: cosmetic file listing for Toka-DOS.
 *
 * Usage: BACKUP source destdrive
 *   source     a filespec in the current directory (wildcards allowed)
 *   destdrive  the destination drive (named, not used; this is cosmetic)
 *
 * For each matching non-directory file it prints "Backing up NAME", then the
 * total count followed by " file(s) backed up". No real archive is written.
 */
#include <string.h>
#include "toka.h"

static char dta[43];

int main(void)
{
    char *p = t_cmdtail();
    static char spec[80];
    static char dest[80];
    char *name;
    int k;
    unsigned count = 0;

    spec[0] = 0;
    dest[0] = 0;

    /* First token: the source filespec. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    k = 0;
    while (*p && *p != ' ' && *p != '\t' && k < 79) {
        spec[k++] = *p++;
    }
    spec[k] = 0;

    /* Second token: the destination drive. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    k = 0;
    while (*p && *p != ' ' && *p != '\t' && k < 79) {
        dest[k++] = *p++;
    }
    dest[k] = 0;

    if (spec[0] == 0 || dest[0] == 0) {
        t_putln("Required parameter missing");
        t_exit(1);
        return 1;
    }

    if (t_findfirst(spec, 0x10, dta) == 0) {
        do {
            /* Skip directories: attribute byte at dta+0x15, bit 0x10. */
            if ((dta[0x15] & 0x10) == 0) {
                name = &dta[0x1E];
                t_puts("Backing up ");
                t_putln(name);
                count++;
            }
        } while (t_findnext(dta) == 0);
    }

    t_putu(count);
    t_putln(" file(s) backed up");

    t_exit(0);
    return 0;
}