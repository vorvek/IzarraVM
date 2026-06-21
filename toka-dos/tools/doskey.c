/* doskey.c - DOSKEY: cosmetic command-line editor / macro recorder.
 *
 * Toka-DOS has no resident command-line history or macro layer, so this is a
 * compatibility stub: it accepts any arguments (and ignores them), reports that
 * the editor is installed, and exits cleanly. Programs and batch files that run
 * DOSKEY on startup get the message they expect and a zero exit code.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    /* The command tail is read so callers like DOSKEY /INSERT or
     * DOSKEY name=command do not produce an error. Nothing is acted on. */
    (void)t_cmdtail();

    t_putln("DOSKEY installed.");
    t_exit(0);
    return 0;
}
