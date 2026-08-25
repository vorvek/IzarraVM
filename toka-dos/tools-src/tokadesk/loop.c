/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "loop.h"
#include "color.h"
#include "dir.h"
#include "margo.h"
#include "v86.h"

void desk_loop(void)
{
    V86Abi *a;
    unsigned last_bx;

    v86_intx(0x33, 2, 0, 0, 0);
    margo_cursor_on(COL_INK, COL_FIELD);
    last_bx = 0;
    for (;;) {
        v86_yield(0);
        a = v86_abi();
        margo_cursor_pos((int)(short)a->cx, (int)(short)a->dx);
        if (a->si) {
            dir_key(v86_intx(0x16, 0, 0, 0, 0));
        }
        if ((a->bx & 1u) && !(last_bx & 1u)) {
            dir_click((int)(short)a->cx, (int)(short)a->dx);
        }
        last_bx = a->bx;
    }
}
