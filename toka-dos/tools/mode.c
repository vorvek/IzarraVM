/* mode.c - MODE: cosmetic device status for Toka-DOS.
 *
 * Usage: MODE [args]
 *   With no arguments, report the console geometry:
 *     Status for device CON:
 *         Columns=80
 *         Lines=25
 *   With any arguments, acknowledge with "MODE set" and exit 0.
 *
 * This is a display-only stub. It does not change any real device state.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();

    while (*p == ' ' || *p == '\t') {
        p++;
    }

    if (*p == 0) {
        t_putln("Status for device CON:");
        t_putln("    Columns=80");
        t_putln("    Lines=25");
    } else {
        t_putln("MODE set");
    }

    t_exit(0);
    return 0;
}
