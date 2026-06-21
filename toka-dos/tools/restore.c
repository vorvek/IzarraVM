/* restore.c - RESTORE: cosmetic counterpart to BACKUP for Toka-DOS.
 *
 * Usage: RESTORE sourcedrive filespec
 *   sourcedrive   the drive that holds the backup set (e.g. A:)
 *   filespec      the files to restore (e.g. C:\*.*)
 *
 * Toka-DOS has no backup catalog yet, so RESTORE just reports that nothing
 * was found to restore. The arguments are accepted for compatibility but are
 * otherwise unused. Always exits 0.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("Restoring files...");
    t_putln("No backup files found.");
    t_exit(0);
    return 0;
}
