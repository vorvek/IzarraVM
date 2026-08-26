/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "desktop.h"
#include "color.h"
#include "editor.h"
#include "margo.h"

static int focused_tab;

int desk_tab(void)
{
    return focused_tab;
}

void desk_set_tab(int t)
{
    focused_tab = t;
}

int desk_rail_click(int x, int y)
{
    int ty;

    if (x >= RAIL_W || y < TAB_Y || y >= CONS_Y) {
        return 0;
    }
    if (editor_busy()) {
        return 1;
    }
    ty = TAB_Y + 8;
    if (y >= ty && y < ty + 24) {
        focused_tab = DESK_TAB_DIR;
        return 1;
    }
    if (editor_is_open()) {
        ty += 28;
        if (y >= ty && y < ty + 24) {
            if (x >= RAIL_W - 22 && x < RAIL_W - 8) {
                return 2;
            }
            focused_tab = DESK_TAB_EDIT;
            return 1;
        }
    }
    return 0;
}

void desk_draw(void)
{
    int ty;
    static const char *menus[] = {"TokaDESK", "System", "Disk", "Tools"};

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
    if (focused_tab == DESK_TAB_DIR) {
        margo_recessed(4, ty, RAIL_W - 8, 24, COL_PANEL_FACE);
        margo_fill(6, ty + 4, 4, 16, COL_LOGO_RED);
    } else {
        margo_raised(4, ty, RAIL_W - 8, 24, COL_PANEL_FACE);
    }
    margo_text8(14, ty + 8, "Directory", COL_INK);

    if (editor_is_open()) {
        ty += 28;
        if (focused_tab == DESK_TAB_EDIT) {
            margo_recessed(4, ty, RAIL_W - 8, 24, COL_PANEL_FACE);
            margo_fill(6, ty + 4, 4, 16, COL_LOGO_RED);
        } else {
            margo_raised(4, ty, RAIL_W - 8, 24, COL_PANEL_FACE);
        }
        margo_text8(14, ty + 8, "TokaEDIT", COL_INK);
        margo_raised(RAIL_W - 22, ty + 4, 14, 16, COL_FACEPLATE);
        margo_text8(RAIL_W - 18, ty + 8, "x", COL_INK);
    }

    margo_recessed(0, CONS_Y, DESK_W, CONS_H, COL_FACEPLATE);
    margo_text8(8, CONS_Y + CONS_H - 20, "C:\\>", COL_INK);
}
