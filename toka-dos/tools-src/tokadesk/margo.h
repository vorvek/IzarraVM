/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_MARGO_H
#define TOKADESK_MARGO_H

#define MARGO_LFB  ((volatile unsigned char *)0xE0000000UL)
#define MARGO_BASE 0xE0400000UL
#define MARGO_REG(off) (((volatile unsigned long *)MARGO_BASE)[(off) >> 2])

#define MG_ID          0x0000
#define MG_STATUS      0x0008
#define MG_DISP_WIDTH  0x0014
#define MG_DISP_HEIGHT 0x0018
#define MG_DISP_BPP    0x001C
#define MG_DISP_PITCH  0x0020
#define MG_CURSOR_CTRL 0x0028
#define MG_CURSOR_ADDR 0x002C
#define MG_CURSOR_POS  0x0030
#define MG_CURSOR_FG   0x0034
#define MG_CURSOR_BG   0x0038
#define MG_DST_BASE    0x0100
#define MG_DST_PITCH   0x0104
#define MG_SRC_BASE    0x0108
#define MG_SRC_PITCH   0x010C
#define MG_DEPTH       0x0110
#define MG_DST_XY      0x0114
#define MG_SRC_XY      0x0118
#define MG_DIM         0x011C
#define MG_FG_COLOR    0x0120
#define MG_ROP         0x0128
#define MG_FLAGS       0x0130
#define MG_COMMAND     0x0150

#define MG_BG_COLOR    0x0124
#define MG_COLORKEY    0x012c
#define MG_CLIP_TL     0x0134
#define MG_CLIP_BR     0x0138
#define MG_MONO_DATA   0x0160

#define MG_CMD_FILL    0x01
#define MG_CMD_COPY    0x02
#define MG_CMD_EXPAND  0x03
#define MG_ROP_PATCOPY 0xF0
#define MG_ROP_SRCCOPY 0xCC
#define MG_FLAG_COLORKEY_EN        0x01
#define MG_FLAG_CLIP_EN            0x02
#define MG_FLAG_EXPAND_TRANSPARENT 0x04

#define CURSOR_OFF 0x180000UL

void margo_wait(void);
void margo_fill(int x, int y, int w, int h, unsigned color);
void margo_cursor_off(void);
void margo_cursor_on(unsigned fg, unsigned bg);
void margo_cursor_pos(int x, int y);
void margo_glyph8(int x, int y, unsigned char ch, unsigned fg);
void margo_text8(int x, int y, const char *s, unsigned fg);
void margo_glyph16(int x, int y, unsigned char ch, unsigned fg);
void margo_text16(int x, int y, const char *s, unsigned fg);
void margo_icon16(int x, int y, const unsigned short *bits, unsigned fg);
void margo_outline(int x, int y, int w, int h, unsigned color);
void margo_raised(int x, int y, int w, int h, unsigned face);
void margo_recessed(int x, int y, int w, int h, unsigned face);

#endif
