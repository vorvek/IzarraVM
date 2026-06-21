/* mirror.c - MIRROR: cosmetic deletion-tracking save.
 *
 * Usage: MIRROR [drive:]
 *   Writes a deletion-tracking "mirror image" of the drive's file table so a
 *   later UNDELETE can recover names. This build is cosmetic: it prints the
 *   familiar progress lines and exits success.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_puts("Creating MIRROR image of drive C:.");
    t_putln("");
    t_putln("The MIRROR process was successful.");
    t_exit(0);
    return 0;
}
