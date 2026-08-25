/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "loop.h"
#include "color.h"
#include "margo.h"
#include "v86.h"

void desk_loop(void)
{
    V86Abi *a;

    v86_intx(0x33, 2, 0, 0, 0);
    margo_cursor_on(COL_INK, COL_FIELD);
    for (;;) {
        v86_yield(0);
        a = v86_abi();
        margo_cursor_pos((int)(short)a->cx, (int)(short)a->dx);
        if (a->si) {
            v86_intx(0x16, 0, 0, 0, 0);
        }
    }
}
