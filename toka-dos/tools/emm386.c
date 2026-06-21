/* emm386.c - EMM386: cosmetic expanded-memory (EMS) manager.
 *
 * Toka-DOS has no expanded memory, so this is a stub that prints a banner and
 * reports that no EMS is installed. It accepts and ignores any arguments.
 *
 * Usage: EMM386 [args]
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("EMM386 - Toka-DOS");
    t_putln("EMS not installed (no expanded memory).");
    t_exit(0);
    return 0;
}
