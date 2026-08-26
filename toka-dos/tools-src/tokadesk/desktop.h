/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_DESKTOP_H
#define TOKADESK_DESKTOP_H

#define DESK_W 1024
#define DESK_H 768
#define MENU_H 24
#define TAB_Y  24
#define TAB_H  500
#define RAIL_W 128
#define CONS_Y 524
#define CONS_H 244

#define DESK_TAB_DIR  0
#define DESK_TAB_EDIT 1

void desk_draw(void);
int desk_tab(void);
void desk_set_tab(int t);
int desk_rail_click(int x, int y);

#endif
