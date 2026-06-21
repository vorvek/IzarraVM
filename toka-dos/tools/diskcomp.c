/* diskcomp.c - DISKCOMP: cosmetic floppy compare for Toka-DOS.
 *
 * Usage: DISKCOMP [d1:] [d2:]
 *
 * Prints the source/target insertion prompts and a successful compare, then
 * exits 0. This is a stub: it does no real floppy access. The drive arguments
 * are accepted and ignored so existing batch files keep working.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();

    /* Accept and ignore any drive arguments; their presence does not change
     * the cosmetic behavior. Skipping the tail keeps the parser honest. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    t_putln("Insert SOURCE diskette in drive A:");
    t_putln("Insert TARGET diskette in drive A:");
    t_putln("Compare OK");
    t_exit(0);
    return 0;
}
