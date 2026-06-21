/* move.c - MOVE: rename or move a file with t_rename.
 *
 * Usage: MOVE oldname newname
 *   Renames or relocates a single file. On success prints eight spaces
 *   followed by "1 file(s) moved". On failure prints "Cannot move " and the
 *   source name. With fewer than two names it reports a missing parameter and
 *   exits with code 1.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    static char oldname[80];
    static char newname[80];
    int have_old = 0, have_new = 0;
    int k;

    oldname[0] = 0;
    newname[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (!have_old) {
            k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                oldname[k++] = *p++;
            }
            oldname[k] = 0;
            have_old = 1;
        } else if (!have_new) {
            k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                newname[k++] = *p++;
            }
            newname[k] = 0;
            have_new = 1;
        } else {
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have_old || !have_new) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    if (t_rename(oldname, newname) == 0) {
        t_putln("        1 file(s) moved");
        t_exit(0);
    }

    t_puts("Cannot move ");
    t_putln(oldname);
    t_exit(1);
    return 0;
}
