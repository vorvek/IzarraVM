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
#define MG_DST_BASE    0x0100
#define MG_DST_PITCH   0x0104
#define MG_DEPTH       0x0110
#define MG_DST_XY      0x0114
#define MG_DIM         0x011C
#define MG_FG_COLOR    0x0120
#define MG_ROP         0x0128
#define MG_FLAGS       0x0130
#define MG_COMMAND     0x0150

#define MG_CMD_FILL    0x01
#define MG_ROP_PATCOPY 0xF0

void margo_wait(void);
void margo_fill(int x, int y, int w, int h, unsigned color);
void margo_cursor_off(void);

#endif
