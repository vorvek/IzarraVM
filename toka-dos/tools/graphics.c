/* graphics.c - GRAPHICS: cosmetic graphics screen-print loader.
 *
 * Usage: GRAPHICS [type]
 *
 * Prints "GRAPHICS loaded." and exits 0. The optional `type` argument is
 * accepted for compatibility and echoed back, but no graphics mode is changed.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();

    while (*p == ' ' || *p == '\t') {
        p++;
    }

    t_putln("GRAPHICS loaded.");

    if (*p) {
        t_puts("Type: ");
        t_putln(p);
    }

    t_exit(0);
    return 0;
}
