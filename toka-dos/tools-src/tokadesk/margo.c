/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "margo.h"

void margo_wait(void)
{
    while (MARGO_REG(MG_STATUS) & 1UL)
        ;
}

void margo_cursor_off(void)
{
    MARGO_REG(MG_CURSOR_CTRL) = 0;
}

void margo_fill(int x, int y, int w, int h, unsigned color)
{
    unsigned long pitch;
    unsigned long bpp;

    margo_wait();
    pitch = MARGO_REG(MG_DISP_PITCH);
    bpp = MARGO_REG(MG_DISP_BPP);
    MARGO_REG(MG_DST_BASE) = 0;
    MARGO_REG(MG_DST_PITCH) = pitch;
    MARGO_REG(MG_DEPTH) = bpp / 8UL;
    MARGO_REG(MG_DST_XY) = ((unsigned long)y << 16) | (unsigned long)x;
    MARGO_REG(MG_DIM) = ((unsigned long)h << 16) | (unsigned long)w;
    MARGO_REG(MG_FG_COLOR) = color;
    MARGO_REG(MG_ROP) = MG_ROP_PATCOPY;
    MARGO_REG(MG_FLAGS) = 0;
    MARGO_REG(MG_COMMAND) = MG_CMD_FILL;
    margo_wait();
}
