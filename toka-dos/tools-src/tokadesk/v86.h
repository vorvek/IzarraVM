/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#ifndef TOKADESK_V86_H
#define TOKADESK_V86_H

/* Must match stubabi.inc: StubAbi at stub offset 40, first 32 bytes. */
#define STUB_ABI_OFF 40u

#pragma pack(1)
typedef struct {
    unsigned short vector;
    unsigned short ax;
    unsigned short bx;
    unsigned short cx;
    unsigned short dx;
    unsigned short si;
    unsigned short di;
    unsigned short ds;
    unsigned short es;
    unsigned short flags;
    unsigned short err;
    unsigned short psp_seg;
    unsigned long bounce_lin;
    unsigned short bounce_off;
    unsigned short rm_seg;
} V86Abi;
#pragma pack()

#define YIELD_ONESHOT 1u
#define YIELD_DIRTY   2u

extern unsigned long stub_lin_slot;
void v86_call(void);

V86Abi *v86_abi(void);
void *v86_bounce(void);
unsigned v86_intx(unsigned vector, unsigned ax, unsigned bx, unsigned cx,
                  unsigned dx);
void v86_yield(unsigned ctl);

#endif
