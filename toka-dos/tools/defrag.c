/* defrag.c - DEFRAG: a cosmetic disk defragmenter for Toka-DOS.
 *
 * Usage: DEFRAG [drive]:
 *
 * Toka-DOS files are never fragmented, so this tool only reports a tidy disk
 * and exits. The optional drive argument is parsed for display but the result
 * is the same for any drive.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    char drive = 'C';

    /* Skip leading blanks, then take the first letter as the drive. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    if (*p >= 'a' && *p <= 'z') {
        drive = (char)(*p - 'a' + 'A');
    } else if (*p >= 'A' && *p <= 'Z') {
        drive = *p;
    }

    t_putln("Toka-DOS Defragmenter");
    t_puts("Drive ");
    t_putc(drive);
    t_putln(": is 0% fragmented.");
    t_putln("You do not need to defragment this drive.");

    t_exit(0);
    return 0;
}
