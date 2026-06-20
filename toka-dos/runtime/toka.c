/* toka.c - shared Toka-DOS runtime. See toka.h. */
#include <dos.h>
#include <string.h>
#include "toka.h"

void t_putc(char c)
{
    union REGS r;
    r.h.ah = 0x02;
    r.h.dl = (unsigned char)c;
    int86(0x21, &r, &r);
}

void t_puts(const char *s)
{
    while (*s) {
        t_putc(*s++);
    }
}

void t_putln(const char *s)
{
    t_puts(s);
    t_putc('\r');
    t_putc('\n');
}

void t_putu(unsigned value)
{
    char buf[6];
    int i = 0;
    if (value == 0) {
        t_putc('0');
        return;
    }
    while (value > 0 && i < 6) {
        buf[i++] = (char)('0' + (value % 10));
        value /= 10;
    }
    while (i > 0) {
        t_putc(buf[--i]);
    }
}

int t_getline(char *out, int max)
{
    /* DOS AH=0Ah wants a buffer of [max][len][bytes...]. SS = DS in the .COM
     * model, so a stack buffer is reachable via DS:DX. */
    unsigned char b[130];
    union REGS r;
    int n, i;

    if (max > 127) {
        max = 127;
    }
    b[0] = (unsigned char)max;
    b[1] = 0;
    r.h.ah = 0x0a;
    r.x.dx = (unsigned)b;
    int86(0x21, &r, &r);

    n = b[1];
    for (i = 0; i < n; i++) {
        out[i] = (char)b[2 + i];
    }
    out[n] = 0;
    /* AH=0Ah leaves the cursor on the CR; move to the next line. */
    t_putc('\r');
    t_putc('\n');
    return n;
}

void t_setmode3(void)
{
    union REGS r;
    r.x.ax = 0x0003;
    int86(0x10, &r, &r);
}

void t_cls(void)
{
    /* Setting the mode reclears the framebuffer, which is what we want. */
    t_setmode3();
}

char *t_cmdtail(void)
{
    static char tail[128];
    unsigned char *psp = (unsigned char *)0x80; /* DS:0x80 = PSP count byte */
    int len = psp[0];
    int i;
    if (len > 126) {
        len = 126;
    }
    for (i = 0; i < len; i++) {
        tail[i] = (char)psp[1 + i];
    }
    tail[len] = 0;
    return tail;
}

void t_exit(int code)
{
    union REGS r;
    r.h.ah = 0x4c;
    r.h.al = (unsigned char)code;
    int86(0x21, &r, &r);
}

/* The EXEC parameter block (INT 21h AH=4B00h). */
struct toka_epb {
    unsigned env_seg;     /* 0 = inherit the parent environment */
    unsigned tail_off;
    unsigned tail_seg;
    unsigned fcb1_off;
    unsigned fcb1_seg;
    unsigned fcb2_off;
    unsigned fcb2_seg;
};

int t_exec(const char *path, const char *tail)
{
    static unsigned char tailbuf[130];
    static unsigned char fcb[16];
    struct toka_epb epb;
    union REGS r;
    struct SREGS s;
    int tlen = (int)strlen(tail);

    if (tlen > 127) {
        tlen = 127;
    }
    tailbuf[0] = (unsigned char)tlen;
    memcpy(&tailbuf[1], tail, (unsigned)tlen);
    tailbuf[1 + tlen] = 0x0d;

    segread(&s);
    epb.env_seg = 0;
    epb.tail_off = (unsigned)tailbuf;
    epb.tail_seg = s.ds;
    epb.fcb1_off = (unsigned)fcb;
    epb.fcb1_seg = s.ds;
    epb.fcb2_off = (unsigned)fcb;
    epb.fcb2_seg = s.ds;

    r.x.ax = 0x4b00;
    r.x.dx = (unsigned)path;  /* DS:DX -> program path */
    r.x.bx = (unsigned)&epb;  /* ES:BX -> parameter block */
    s.es = s.ds;
    int86x(0x21, &r, &r, &s);

    if (r.x.cflag) {
        return (int)r.x.ax;   /* DOS error code (2 = not found) */
    }
    return 0;
}
