/* undelete.c - UNDELETE: cosmetic stub for Toka-DOS.
 *
 * Usage: UNDELETE [filespec]
 *
 * Toka-DOS keeps no deletion log yet, so there is nothing to recover. This
 * tool prints a banner and reports that no entries were found, then exits 0
 * so it behaves like the real command without ever touching the disk.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("UNDELETE - Toka-DOS");
    t_putln("No entries found.");
    return 0;
}
