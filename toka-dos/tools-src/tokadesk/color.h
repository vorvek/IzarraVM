/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_COLOR_H
#define TOKADESK_COLOR_H

/* Packed R5G6B5. Source RGB888 from the host GUI and the POST cream field. */
#define RGB565(r, g, b) \
    (unsigned)((((r) >> 3) << 11) | (((g) >> 2) << 5) | ((b) >> 3))

#define COL_FIELD      RGB565(236, 230, 223)
#define COL_PANEL_FACE RGB565(205, 195, 164)
#define COL_FACEPLATE  RGB565(196, 186, 153)
#define COL_BEVEL_HI   RGB565(222, 214, 189)
#define COL_BEVEL_LO   RGB565(155, 145, 118)
#define COL_RECESS     RGB565(34, 31, 24)
#define COL_INK        RGB565(74, 67, 50)
#define COL_LABEL      RGB565(107, 98, 72)
#define COL_MUTED      RGB565(92, 83, 64)
#define COL_LOGO_RED   RGB565(199, 68, 70)
#define COL_WHITE      RGB565(255, 255, 255)
#define COL_BLACK      RGB565(0, 0, 0)
#define COL_KEY        RGB565(255, 0, 255)

#endif
