/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "v86.h"

V86Abi *v86_abi(void)
{
    return (V86Abi *)(stub_lin_slot + STUB_ABI_OFF);
}

void *v86_bounce(void)
{
    return (void *)v86_abi()->bounce_lin;
}

unsigned v86_intx(unsigned vector, unsigned ax, unsigned bx, unsigned cx,
                  unsigned dx)
{
    V86Abi *a;

    a = v86_abi();
    a->vector = (unsigned short)vector;
    a->ax = (unsigned short)ax;
    a->bx = (unsigned short)bx;
    a->cx = (unsigned short)cx;
    a->dx = (unsigned short)dx;
    a->si = 0;
    a->di = 0;
    a->ds = a->rm_seg;
    a->es = a->rm_seg;
    a->flags = 0;
    a->err = 0;
    v86_call();
    return a->ax;
}

void v86_yield(unsigned ctl)
{
    V86Abi *a;

    a = v86_abi();
    a->vector = 0;
    a->ax = (unsigned short)ctl;
    a->bx = 0;
    a->cx = 0;
    a->dx = 0;
    a->si = 0;
    a->di = 0;
    a->ds = a->rm_seg;
    a->es = a->rm_seg;
    a->flags = 0;
    a->err = 0;
    v86_call();
}

unsigned v86_bounce_off(void)
{
    return v86_abi()->bounce_off;
}

void bounce_str(unsigned off, const char *s)
{
    unsigned char *d;

    d = (unsigned char *)v86_bounce() + off;
    while (*s) {
        *d++ = (unsigned char)*s++;
    }
    *d = 0;
}

void bounce_mem(unsigned off, const void *src, unsigned n)
{
    unsigned char *d;
    const unsigned char *s;

    d = (unsigned char *)v86_bounce() + off;
    s = (const unsigned char *)src;
    while (n) {
        *d++ = *s++;
        n--;
    }
}

void bounce_get(unsigned off, void *dst, unsigned n)
{
    unsigned char *d;
    const unsigned char *s;

    d = (unsigned char *)dst;
    s = (const unsigned char *)v86_bounce() + off;
    while (n) {
        *d++ = *s++;
        n--;
    }
}

unsigned dos_call(unsigned ax, unsigned bx, unsigned cx, unsigned dx,
                  unsigned si, unsigned di)
{
    V86Abi *a;

    a = v86_abi();
    a->vector = 0x21;
    a->ax = (unsigned short)ax;
    a->bx = (unsigned short)bx;
    a->cx = (unsigned short)cx;
    a->dx = (unsigned short)dx;
    a->si = (unsigned short)si;
    a->di = (unsigned short)di;
    a->ds = a->rm_seg;
    a->es = a->rm_seg;
    a->flags = 0;
    a->err = 0;
    v86_call();
    return a->ax;
}

int dos_err(void)
{
    return v86_abi()->err;
}
