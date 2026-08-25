/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_DIR_H
#define TOKADESK_DIR_H

int dir_init(void);
void dir_draw(void);
void dir_key(unsigned ax);
void dir_click(int x, int y);

#endif
