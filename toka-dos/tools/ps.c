/* ps.c - PS: a friendly Toka-DOS process list.
 *
 * Toka-DOS is single-tasking, so there is only ever the shell to show. PS
 * prints a small header and one cosmetic row for ICOMMAND, then exits 0.
 *
 * Usage: PS
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("  PID  NAME");
    t_putln("    1  ICOMMAND");
    t_exit(0);
    return 0;
}
