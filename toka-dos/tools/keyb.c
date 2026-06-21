/* keyb.c - KEYB: cosmetic keyboard layout selector for Toka-DOS.
 *
 * Usage: KEYB [layout]
 *   With no argument, reports the current layout (US).
 *   With an argument such as UK or ES, echoes it back uppercased.
 * This is cosmetic only; no keyboard remapping happens.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    static char layout[16];
    int k = 0;
    char c;

    /* Skip the leading blanks DOS leaves before the first argument. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    /* Copy the first whitespace-delimited token, uppercasing as we go. */
    while (*p && *p != ' ' && *p != '\t' && k < 15) {
        c = *p;
        if (c >= 'a' && c <= 'z') {
            c = (char)(c - 'a' + 'A');
        }
        layout[k++] = c;
        p++;
    }
    layout[k] = 0;

    if (k == 0) {
        t_putln("Current keyboard layout: US");
    } else {
        t_puts("Keyboard layout set: ");
        t_putln(layout);
    }

    t_exit(0);
    return 0;
}
