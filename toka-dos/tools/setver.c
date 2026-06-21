/* setver.c - SETVER: a cosmetic DOS version table.
 *
 * Usage: SETVER [name n.nn]
 *   With no arguments, print the (faked) version table.
 *   With arguments, acknowledge an update without storing anything.
 *
 * Toka-DOS keeps no real per-program version map, so this tool is purely
 * for show: it never reads or writes any table. Exit code is always 0.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();

    /* Skip the leading blank DOS leaves before the first argument. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    if (*p == 0) {
        t_putln("Toka-DOS version table");
        t_putln("WIN100.BIN    3.40");
        t_putln("WIN200.BIN    3.40");
        t_putln("REDIR.EXE     4.00");
    } else {
        t_putln("Version table updated");
    }

    t_exit(0);
    return 0;
}
