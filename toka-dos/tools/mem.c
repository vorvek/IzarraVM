/* mem.c - MEM: report live system memory usage.
 *
 * Queries the running machine the way MS-DOS MEM.EXE does: it walks the real
 * MCB chain (from the List of Lists at INT 21h AH=52h, first MCB at [ES:BX-2]),
 * asks the XMS driver for free extended memory, and probes INT 67h for the
 * expanded-memory state IEMM.EXE provides. Modeled on iemm.c, which is already
 * live.
 */
#include <dos.h>
#include <i86.h>   /* MK_FP and the `far` keyword in Open Watcom */
#include "toka.h"

/* An MCB header is 16 bytes: [0] signature 'M' (link) or 'Z' (last), [1..2]
 * owner PSP (0 = free), [3..4] size in paragraphs. Data starts one paragraph
 * past the header, and the next header is at seg + 1 + size. */
#define MCB_LINK 0x4D
#define MCB_LAST 0x5A
#define UPPER_BASE 0xA000  /* first upper-memory segment; below this is DOS */

int main(void)
{
    union REGS r;
    struct SREGS s;
    unsigned lol_seg, lol_off, first_mcb, seg;
    unsigned long conv_used = 0, conv_free = 0;
    unsigned long upper_used = 0, upper_free = 0;
    unsigned char far *mcb;
    unsigned char sig;
    int guard;
    unsigned owner, paras;
    unsigned long bytes;

    t_putln("Toka-DOS Memory");
    t_putln("");

    /* AH=52h: List of Lists into ES:BX. First MCB segment sits at [ES:BX-2]. */
    segread(&s);
    r.h.ah = 0x52;
    int86x(0x21, &r, &r, &s);
    lol_seg = s.es;
    lol_off = r.x.bx;
    first_mcb = *(unsigned far *)MK_FP(lol_seg, (unsigned)(lol_off - 2));

    /* Walk the chain. Cap the walk so a damaged chain cannot spin forever. */
    seg = first_mcb;
    for (guard = 0; guard < 255; guard++) {
        mcb = (unsigned char far *)MK_FP(seg, 0);
        sig = mcb[0];
        if (sig != MCB_LINK && sig != MCB_LAST) {
            break; /* not a valid header: stop the walk */
        }
        owner = (unsigned)mcb[1] | ((unsigned)mcb[2] << 8);
        paras = (unsigned)mcb[3] | ((unsigned)mcb[4] << 8);
        bytes = (unsigned long)paras * 16UL;
        if (seg < UPPER_BASE) {
            if (owner == 0) {
                conv_free += bytes;
            } else {
                conv_used += bytes;
            }
        } else {
            if (owner == 0) {
                upper_free += bytes;
            } else {
                upper_used += bytes;
            }
        }
        if (sig == MCB_LAST) {
            break;
        }
        seg = seg + 1 + paras; /* next header */
    }

    t_puts("  Conventional used .... ");
    t_putul(conv_used);
    t_putln(" bytes");
    t_puts("  Conventional free .... ");
    t_putul(conv_free);
    t_putln(" bytes");
    t_puts("  Upper used ........... ");
    t_putul(upper_used);
    t_putln(" bytes");
    t_puts("  Upper free ........... ");
    t_putul(upper_free);
    t_putln(" bytes");
    t_putln("");

    /* Extended / XMS. In this machine the XMS entry stub handed back by INT 2Fh
     * AX=4310h is just `INT 66h; RETF`, so query free extended memory by calling
     * INT 66h with AH=08h directly: AX = largest free KB, DX = total free KB. */
    r.x.ax = 0x4300;
    int86(0x2f, &r, &r);
    if (r.h.al == 0x80) {
        r.h.ah = 0x08;
        int86(0x66, &r, &r);
        t_puts("  Extended free ........ ");
        t_putu(r.x.dx);
        t_puts(" KB (largest ");
        t_putu(r.x.ax);
        t_putln(" KB)");
    } else {
        t_putln("  Extended memory manager not present.");
    }
    t_putln("");

    /* Expanded / EMS via INT 67h, exactly as iemm.c. AH=40h status (AH=0 means a
     * manager is installed), AH=41h page frame (AH=0 RAM, else NOEMS/frameless),
     * AH=42h page counts (BX free, DX total), each page 16 KB. */
    r.x.ax = 0x4000;
    int86(0x67, &r, &r);
    if (r.h.ah != 0) {
        t_putln("  Expanded memory is not installed.");
    } else {
        r.x.ax = 0x4100;
        int86(0x67, &r, &r);
        if (r.h.ah == 0) {
            r.x.ax = 0x4200;
            int86(0x67, &r, &r);
            t_puts("  Expanded free ........ ");
            t_putu((unsigned)r.x.bx * 16u);
            t_putln(" KB");
            t_puts("  Expanded total ....... ");
            t_putu((unsigned)r.x.dx * 16u);
            t_putln(" KB");
        } else {
            t_putln("  Expanded: NOEMS, no page frame (upper memory active).");
        }
    }

    t_exit(0);
    return 0;
}
