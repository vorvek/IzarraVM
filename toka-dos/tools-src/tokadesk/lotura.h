/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_LOTURA_H
#define TOKADESK_LOTURA_H

#define PORT_UT_INDEX   0xE4
#define PORT_UT_DATA    0xE5
#define PORT_UT_COMMAND 0xE6
#define UT_REG_X        0
#define UT_REG_Y        2
#define UT_REG_W        4
#define UT_REG_H        6
#define UT_REG_CRC      8
#define UT_REG_EXIT     12
#define UT_CMD_CRC      1
#define UT_CMD_EXIT     3

unsigned char inb(unsigned port);
void outb(unsigned port, unsigned char value);

void ut_put16(unsigned char index, unsigned value);
void ut_put8(unsigned char index, unsigned char value);
void ut_exit(unsigned char code);

#endif
