/* icdex.c - ICDEX: Izarra CD-ROM Extensions (clean-room MSCDEX).
 *
 * Usage: ICDEX [args]
 *
 * Cosmetic stand-in for the CD-ROM redirector. It reports a version banner
 * and a single mapped drive, then claims the extensions are installed. Real
 * D: access is not wired yet, so no driver is actually attached and any
 * arguments are ignored.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("ICDEX Version 2.23");
    t_putln("Drive D: = Driver ICD0001 unit 0");
    t_putln("    CD-ROM extensions installed.");
    t_exit(0);
    return 0;
}
