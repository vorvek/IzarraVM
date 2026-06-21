/* fastopen.c - FASTOPEN: cosmetic file-open cache for Toka-DOS.
 *
 * Usage: FASTOPEN [drive]:
 *
 * Real DOS FASTOPEN keeps a small in-memory cache of recently opened file
 * locations so repeat opens skip the directory walk. Toka-DOS has no such
 * cache, so this tool is cosmetic: it accepts the optional drive letter,
 * reports that it installed, and exits 0.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    char drive = 0;

    while (*p == ' ' || *p == '\t') {
        p++;
    }

    if (((*p >= 'A' && *p <= 'Z') || (*p >= 'a' && *p <= 'z')) && p[1] == ':') {
        drive = *p;
        if (drive >= 'a' && drive <= 'z') {
            drive = (char)(drive - 'a' + 'A');
        }
    }

    if (drive) {
        t_puts("FASTOPEN installed for drive ");
        t_putc(drive);
        t_putln(":");
    } else {
        t_putln("FASTOPEN installed.");
    }

    t_exit(0);
    return 0;
}
