/* attrib.c - ATTRIB: list file attributes.
 *
 * Usage: ATTRIB [+R|-R|+A|-A|+S|-S|+H|-H] [filespec]
 *
 * Parses optional +/- attribute switches and an optional filespec. The
 * attribute switches are accepted but treated as no-ops, since the Toka-DOS
 * host filesystem does not model the DOS read-only, archive, system, and
 * hidden bits. When no filespec is given it defaults to *.*. Each non-directory
 * match is listed with a leading "A    " and its 8.3 name.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    static char spec[80];
    static unsigned char dta[64];
    int have_spec = 0;
    int more;

    spec[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (*p == '+' || *p == '-') {
            /* An attribute switch like +R or -H. Accept and ignore it. */
            p++;
            if (*p) {
                p++;
            }
        } else {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                spec[k++] = *p++;
            }
            spec[k] = 0;
            have_spec = 1;
        }
    }

    if (!have_spec) {
        strcpy(spec, "*.*");
    }

    more = (t_findfirst(spec, 0x10, dta) == 0);
    while (more) {
        if ((dta[0x15] & 0x10) == 0) {
            t_puts("A    ");
            t_putln((char *)&dta[0x1E]);
        }
        more = (t_findnext(dta) == 0);
    }

    t_exit(0);
    return 0;
}