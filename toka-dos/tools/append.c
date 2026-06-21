/* append.c - APPEND: cosmetic data-file search path.
 *
 * Usage: APPEND [path]
 *   With no argument, report that no directories are appended.
 *   With a path argument, echo "APPEND=" followed by the path.
 *
 * This is a stub. Toka-DOS does not implement a real append search list, so
 * nothing is actually appended; the command only reports the given path back.
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
        t_putln("No appended directories.");
    } else {
        t_puts("APPEND=");
        t_putln(p);
    }

    t_exit(0);
    return 0;
}
