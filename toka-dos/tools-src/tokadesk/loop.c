/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "loop.h"
#include "color.h"
#include "desktop.h"
#include "dir.h"
#include "editor.h"
#include "margo.h"
#include "v86.h"

static void redraw(void)
{
    desk_draw();
    if (desk_tab() == DESK_TAB_EDIT && editor_is_open()) {
        editor_draw();
    } else {
        dir_draw();
    }
}

void desk_loop(void)
{
    V86Abi *a;
    unsigned last_bx;
    int rail;

    v86_intx(0x33, 2, 0, 0, 0);
    margo_cursor_on(COL_INK, COL_FIELD);
    last_bx = 0;
    desk_set_tab(DESK_TAB_DIR);
    redraw();
    for (;;) {
        v86_yield(0);
        a = v86_abi();
        margo_cursor_pos((int)(short)a->cx, (int)(short)a->dx);
        if (a->si) {
            unsigned key = v86_intx(0x16, 0, 0, 0, 0);
            if (desk_tab() == DESK_TAB_EDIT && editor_is_open()) {
                editor_key(key);
                if (!editor_is_open()) {
                    desk_set_tab(DESK_TAB_DIR);
                }
            } else {
                dir_key(key);
            }
            redraw();
        }
        if ((a->bx & 1u) && !(last_bx & 1u)) {
            int x = (int)(short)a->cx;
            int y = (int)(short)a->dx;
            rail = desk_rail_click(x, y);
            if (rail == 2) {
                editor_key(0x011B);
                if (!editor_is_open()) {
                    desk_set_tab(DESK_TAB_DIR);
                }
            } else if (rail == 0) {
                if (desk_tab() == DESK_TAB_EDIT && editor_is_open()) {
                    editor_click(x, y);
                    if (!editor_is_open()) {
                        desk_set_tab(DESK_TAB_DIR);
                    }
                } else {
                    dir_click(x, y);
                }
            }
            redraw();
        }
        last_bx = a->bx;
    }
}
