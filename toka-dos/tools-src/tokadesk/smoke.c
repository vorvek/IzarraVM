/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "color.h"
#include "desktop.h"
#include "lotura.h"
#include "margo.h"

void desk_main(void)
{
    unsigned long width;
    unsigned long height;
    unsigned long bpp;

    margo_cursor_off();
    width = MARGO_REG(MG_DISP_WIDTH);
    height = MARGO_REG(MG_DISP_HEIGHT);
    bpp = MARGO_REG(MG_DISP_BPP);
    if (width != 1024UL || height != 768UL || bpp != 16UL) {
        ut_exit(0xE6);
    }
    desk_draw();
    ut_exit(0);
}
