/* debug.c - DEBUG: the Toka-DOS debugger.
 *
 * This is a placeholder. The real DEBUG (assemble, disassemble, dump, trace,
 * and patch memory) is not part of this release yet. For now it just announces
 * itself and exits cleanly so scripts that call DEBUG do not fail.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("Toka-DOS DEBUG");
    t_putln("Not yet implemented in this release.");
    t_exit(0);
    return 0;
}
