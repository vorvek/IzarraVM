/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_EDITOR_H
#define TOKADESK_EDITOR_H

int editor_is_open(void);
int editor_open(const char *path);
int editor_key(unsigned ax);
void editor_click(int x, int y);
void editor_draw(void);
int editor_want_close(void);
int editor_busy(void);

#endif
