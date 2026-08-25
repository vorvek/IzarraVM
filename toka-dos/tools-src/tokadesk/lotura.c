/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "lotura.h"

unsigned char inb(unsigned port);
#pragma aux inb = "in al, dx" parm[dx] value[al] modify exact[al];

void outb(unsigned port, unsigned char value);
#pragma aux outb = "out dx, al" parm[dx][al] modify exact[];

void ut_put8(unsigned char index, unsigned char value)
{
    outb(PORT_UT_INDEX, index);
    outb(PORT_UT_DATA, value);
}

void ut_put16(unsigned char index, unsigned value)
{
    ut_put8(index, (unsigned char)value);
    ut_put8((unsigned char)(index + 1), (unsigned char)(value >> 8));
}

void ut_exit(unsigned char code)
{
    ut_put8(UT_REG_EXIT, code);
    outb(PORT_UT_COMMAND, UT_CMD_EXIT);
    for (;;)
        ;
}
