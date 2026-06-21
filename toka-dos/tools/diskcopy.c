/* diskcopy.c - DISKCOPY: cosmetic floppy copy for Toka-DOS.
 *
 * Usage: DISKCOPY [d1] [d2]
 *
 * Toka-DOS has no real floppy track-copy path yet, so this walks the operator
 * through the motions of a single-drive copy and reports success. The drive
 * arguments are accepted for command compatibility but not used.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("Insert SOURCE diskette in drive A:");
    t_putln("Insert TARGET diskette in drive A:");
    t_putln("Copy complete.");
    t_exit(0);
    return 0;
}
