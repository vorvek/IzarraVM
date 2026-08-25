/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "desktop.h"
#include "color.h"
#include "margo.h"

void desk_draw(void)
{
    int ty;
    int i;
    static const char *menus[] = {"TokaDESK", "System", "Disk", "Tools"};
    static const char *tabs[] = {"Directory"};

    margo_fill(0, 0, DESK_W, DESK_H, COL_FIELD);

    margo_raised(0, 0, DESK_W, MENU_H, COL_PANEL_FACE);
    {
        int mx = 8;
        unsigned mi;
        for (mi = 0; mi < 4; mi++) {
            margo_text8(mx, 8, menus[mi], COL_INK);
            mx += 8 * 10;
        }
    }

    margo_recessed(0, TAB_Y, RAIL_W, TAB_H, COL_FACEPLATE);
    ty = TAB_Y + 8;
    for (i = 0; i < 1; i++) {
        margo_recessed(4, ty, RAIL_W - 8, 24, COL_PANEL_FACE);
        margo_fill(6, ty + 4, 4, 16, COL_LOGO_RED);
        margo_text8(14, ty + 8, tabs[i], COL_INK);
        ty += 28;
    }

    margo_raised(RAIL_W, TAB_Y, DESK_W - RAIL_W, TAB_H, COL_FIELD);
    margo_text8(RAIL_W + 16, TAB_Y + 16, "Directory", COL_LABEL);

    margo_recessed(0, CONS_Y, DESK_W, CONS_H, COL_FACEPLATE);
    margo_text8(8, CONS_Y + CONS_H - 20, "C:\\>", COL_INK);
}
