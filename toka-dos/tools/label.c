/* label.c - LABEL: show or set the volume label.
 *
 * Usage: LABEL [newlabel]
 *
 * Toka-DOS has no real volume-label storage, so this is cosmetic: it always
 * reports the fixed label for drive C. When a non-empty new label is supplied
 * on the command line it also confirms that the label was set. Exit code 0.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    int have_label = 0;

    while (*p == ' ' || *p == '\t') {
        p++;
    }
    if (*p) {
        have_label = 1;
    }

    t_putln(" Volume in drive C is TOKA-DOS");
    if (have_label) {
        t_putln(" Volume label set");
    }

    t_exit(0);
    return 0;
}
