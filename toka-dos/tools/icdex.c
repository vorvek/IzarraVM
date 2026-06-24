/* icdex.c - ICDEX: the Izarra CD-ROM extensions status tool.
 *
 * Reports the live CD-ROM redirector state the way a user checks MSCDEX at the
 * prompt. The redirector is resident in the HLE permanently, so this tool only
 * queries it through INT 2Fh and installs nothing: AX=1500h to count drives and
 * find the first drive number, AX=150Dh for the drive letter, AX=150Ch for the
 * version. With no extensions present it says so.
 *
 * Usage: ICDEX
 */
#include <dos.h>
#include "toka.h"

int main(void)
{
    union REGS r;
    struct SREGS sr;
    unsigned char letters[26];
    unsigned drive;
    unsigned ndrives;

    t_putln("ICDEX CD-ROM extensions status");

    /* AX=1500h install check. BX = number of CD-ROM drives, CX = the first
     * drive number (0 = A:). BX = 0 means no extensions. */
    segread(&sr);
    r.x.ax = 0x1500;
    r.x.bx = 0;
    int86x(0x2f, &r, &r, &sr);
    ndrives = r.x.bx;
    if (ndrives == 0) {
        t_putln("CD-ROM extensions are not present.");
        t_exit(0);
        return 0;
    }

    /* AX=150Dh: fill ES:BX with one drive-number byte per CD drive. Point ES:BX
     * at the local buffer (small model: ES = DS = our segment). */
    segread(&sr);
    r.x.ax = 0x150d;
    r.x.bx = (unsigned)letters; /* near offset in our data segment */
    sr.es = sr.ds;
    int86x(0x2f, &r, &r, &sr);
    drive = letters[0];

    t_puts("CD-ROM drive ............... ");
    t_putc((char)('A' + drive));
    t_putln(":");

    /* AX=150Ch: version in BX, BH = major, BL = minor (plain decimal here). */
    r.x.ax = 0x150c;
    int86(0x2f, &r, &r);
    t_puts("Extensions version ......... ");
    t_putu((unsigned)r.h.bh);
    t_putc('.');
    t_putu((unsigned)r.h.bl);
    t_putc('\r');
    t_putc('\n');

    t_exit(0);
    return 0;
}
