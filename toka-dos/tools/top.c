/* top.c - TOP: a process and resource view, in the spirit of PS.
 *
 * Toka-DOS is a single-tasking real-mode system, so there is exactly one
 * running program: the ICOMMAND shell. This view is cosmetic. It prints a
 * header and one row for ICOMMAND, then exits.
 *
 * Usage: TOP
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("Toka-DOS TOP");
    t_putln("  PID  NAME      CPU");
    t_putln("    1  ICOMMAND   0%");
    t_exit(0);
    return 0;
}
