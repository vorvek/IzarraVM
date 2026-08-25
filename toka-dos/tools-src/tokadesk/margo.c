/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "margo.h"
#include "color.h"
#include "font.h"

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

void margo_outline(int x, int y, int w, int h, unsigned color)
{
    if (w <= 0 || h <= 0) {
        return;
    }
    margo_fill(x, y, w, 1, color);
    margo_fill(x, y + h - 1, w, 1, color);
    if (h > 2) {
        margo_fill(x, y + 1, 1, h - 2, color);
        margo_fill(x + w - 1, y + 1, 1, h - 2, color);
    }
}

void margo_raised(int x, int y, int w, int h, unsigned face)
{
    margo_outline(x, y, w, h, COL_BLACK);
    if (w < 4 || h < 4) {
        return;
    }
    margo_fill(x + 1, y + 1, w - 2, 1, COL_BEVEL_HI);
    margo_fill(x + 1, y + 1, 1, h - 2, COL_BEVEL_HI);
    margo_fill(x + 1, y + h - 2, w - 2, 1, COL_BEVEL_LO);
    margo_fill(x + w - 2, y + 1, 1, h - 2, COL_BEVEL_LO);
    margo_fill(x + 2, y + 2, w - 4, h - 4, face);
}

void margo_recessed(int x, int y, int w, int h, unsigned face)
{
    margo_outline(x, y, w, h, COL_BLACK);
    if (w < 4 || h < 4) {
        return;
    }
    margo_fill(x + 1, y + 1, w - 2, 1, COL_BEVEL_LO);
    margo_fill(x + 1, y + 1, 1, h - 2, COL_BEVEL_LO);
    margo_fill(x + 1, y + h - 2, w - 2, 1, COL_BEVEL_HI);
    margo_fill(x + w - 2, y + 1, 1, h - 2, COL_BEVEL_HI);
    margo_fill(x + 2, y + 2, w - 4, h - 4, face);
}

void margo_glyph8(int x, int y, unsigned char ch, unsigned fg)
{
    const unsigned char *glyph;
    unsigned row;
    unsigned long pitch;
    unsigned long bpp;

    glyph = font8 + ((unsigned)ch * 8);
    margo_wait();
    pitch = MARGO_REG(MG_DISP_PITCH);
    bpp = MARGO_REG(MG_DISP_BPP);
    MARGO_REG(MG_DST_BASE) = 0;
    MARGO_REG(MG_DST_PITCH) = pitch;
    MARGO_REG(MG_DEPTH) = bpp / 8UL;
    MARGO_REG(MG_DST_XY) = ((unsigned long)y << 16) | (unsigned long)x;
    MARGO_REG(MG_DIM) = (8UL << 16) | 8UL;
    MARGO_REG(MG_FG_COLOR) = fg;
    MARGO_REG(MG_ROP) = MG_ROP_SRCCOPY;
    MARGO_REG(MG_FLAGS) = MG_FLAG_EXPAND_TRANSPARENT;
    MARGO_REG(MG_COMMAND) = MG_CMD_EXPAND;
    for (row = 0; row < 8; row++) {
        MARGO_REG(MG_MONO_DATA) = (unsigned long)glyph[row] << 24;
    }
    margo_wait();
}

void margo_text8(int x, int y, const char *s, unsigned fg)
{
    while (*s) {
        margo_glyph8(x, y, (unsigned char)*s, fg);
        x += 8;
        s++;
    }
}

void margo_cursor_on(unsigned fg, unsigned bg)
{
    volatile unsigned char *and_plane;
    unsigned row;
    unsigned col;
    /* A 11x16 Workbench-style arrow in the top-left of the 64x64 slot. */
    static const unsigned short arrow[16] = {
        0x8000, 0xC000, 0xE000, 0xF000, 0xF800, 0xFC00, 0xFE00, 0xFF00,
        0xFF80, 0xFFC0, 0xF800, 0xD800, 0x8C00, 0x0C00, 0x0600, 0x0600
    };

    and_plane = MARGO_LFB + CURSOR_OFF;
    for (row = 0; row < 64; row++) {
        for (col = 0; col < 8; col++) {
            and_plane[row * 8 + col] = 0xFF;
            and_plane[512 + row * 8 + col] = 0;
        }
    }
    for (row = 0; row < 16; row++) {
        unsigned short bits = arrow[row];
        and_plane[row * 8 + 0] = (unsigned char)((~bits >> 8) & 0xFF);
        and_plane[row * 8 + 1] = (unsigned char)((~bits) & 0xFF);
        and_plane[512 + row * 8 + 0] = (unsigned char)((bits >> 8) & 0xFF);
        and_plane[512 + row * 8 + 1] = (unsigned char)(bits & 0xFF);
    }
    MARGO_REG(0x002C) = CURSOR_OFF;
    MARGO_REG(0x0034) = fg;
    MARGO_REG(0x0038) = bg;
    MARGO_REG(MG_CURSOR_CTRL) = 1;
}
