/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "color.h"
#include "desktop.h"
#include "loop.h"
#include "lotura.h"
#include "margo.h"
#include "v86.h"

static int has_switch_t(void)
{
    unsigned char *p;
    unsigned n;
    unsigned i;

    p = (unsigned char *)(((unsigned long)v86_abi()->psp_seg << 4) + 0x80u);
    n = p[0];
    if (n > 126u) {
        n = 126u;
    }
    for (i = 1; i + 1 <= n; i++) {
        if (p[i] == '/' && (p[i + 1] == 'T' || p[i + 1] == 't')) {
            return 1;
        }
    }
    return 0;
}

void desk_main(void)
{
    unsigned long width;
    unsigned long height;
    unsigned long bpp;
    unsigned ax;

    margo_cursor_off();
    width = MARGO_REG(MG_DISP_WIDTH);
    height = MARGO_REG(MG_DISP_HEIGHT);
    bpp = MARGO_REG(MG_DISP_BPP);
    if (width != 1024UL || height != 768UL || bpp != 16UL) {
        ut_exit(0xE6);
    }
    desk_draw();
    if (!has_switch_t()) {
        desk_loop();
    }

    /* INT 21h AH=19h: current drive, 0=A so C: is 2. */
    ax = v86_intx(0x21, 0x1900, 0, 0, 0);
    if ((ax & 0xFFu) != 2u) {
        ut_exit(0xE7);
    }
    /* INT 21h AH=30h: DOS major in AL. */
    ax = v86_intx(0x21, 0x3000, 0, 0, 0);
    if ((ax & 0xFFu) == 0u) {
        ut_exit(0xE8);
    }
    /* INT 10h AX=4F03h: current VBE mode. BX keeps LFB bit 14. */
    ax = v86_intx(0x10, 0x4F03, 0, 0, 0);
    if (ax != 0x004Fu || (v86_abi()->bx & 0x1FFu) != 0x117u) {
        ut_exit(0xE9);
    }
    /* INT 33h AX=0000h: TOKAMOUS reports installed. */
    ax = v86_intx(0x33, 0, 0, 0, 0);
    if (ax != 0xFFFFu) {
        ut_exit(0xEA);
    }

    v86_yield(YIELD_ONESHOT);
    margo_cursor_off();
    ut_exit(0);
}
