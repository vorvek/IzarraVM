/* iemm.c - IEMM: the Izarra memory-manager status tool.
 *
 * Reports the real EMS / upper-memory state by probing INT 67h, the way a user
 * checks expanded memory at the prompt. Reflects the three EMM386 roles the
 * CONFIG.SYS DEVICE=IEMM.EXE line selects: RAM (a page frame plus backing
 * pages), NOEMS (a frameless manager; upper memory is active but there is no
 * frame), or no manager at all (unloaded; HIMEM only).
 *
 * Usage: IEMM
 */
#include <dos.h>
#include "toka.h"

int main(void)
{
    union REGS r;

    t_putln("IEMM Expanded-Memory Manager");

    /* AH=40h: get manager status. AH=0 means a manager is installed. With no
     * manager INT 67h is not hooked, so AH comes back unchanged (nonzero) and
     * the tool reports expanded memory is not installed. */
    r.x.ax = 0x4000;
    int86(0x67, &r, &r);
    if (r.h.ah != 0) {
        t_putln("Expanded memory is not installed.");
        t_exit(0);
        return 0;
    }

    /* AH=41h: get the page-frame segment. AH=0 (BX=segment) means a real frame
     * (RAM mode); 0x80 means a frameless manager (NOEMS). */
    r.x.ax = 0x4100;
    int86(0x67, &r, &r);
    if (r.h.ah == 0) {
        /* RAM: report the backing pages. AH=42h returns BX = free pages, DX =
         * total pages, each page 16 KB. */
        r.x.ax = 0x4200;
        int86(0x67, &r, &r);
        t_puts("Available expanded memory ... ");
        t_putu((unsigned)r.x.bx * 16u);
        t_putln(" KB");
        t_puts("Total expanded memory ...... ");
        t_putu((unsigned)r.x.dx * 16u);
        t_putln(" KB");
    } else {
        t_putln("NOEMS: no page frame (upper memory active).");
    }
    t_exit(0);
    return 0;
}
