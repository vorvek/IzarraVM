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

void t_putul(unsigned long value)
{
    char buf[10];
    int i = 0;
    if (value == 0) {
        t_putc('0');
        return;
    }
    while (value > 0 && i < 10) {
        buf[i++] = (char)('0' + (int)(value % 10));
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

void t_putcell(int row, int col, char ch, unsigned char attr)
{
    unsigned short far *cell = (unsigned short far *)MK_FP(
        0xB800, (unsigned)((row * 80 + col) * 2));
    unsigned short value = (unsigned short)(unsigned char)ch |
                           ((unsigned short)attr << 8);
    if (*cell != value) {
        *cell = value;
    }
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

int t_exec_env(const char *path, const char *tail, unsigned env_seg)
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
    epb.env_seg = env_seg;
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

int t_exec(const char *path, const char *tail)
{
    return t_exec_env(path, tail, 0);
}

int t_open(const char *path, int mode)
{
    union REGS r;
    r.h.ah = 0x3d;
    r.h.al = (unsigned char)mode;
    r.x.dx = (unsigned)path;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : (int)r.x.ax;
}

int t_create(const char *path)
{
    union REGS r;
    r.h.ah = 0x3c;
    r.x.cx = 0; /* normal attribute */
    r.x.dx = (unsigned)path;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : (int)r.x.ax;
}

int t_read(int handle, void *buf, int count)
{
    union REGS r;
    r.h.ah = 0x3f;
    r.x.bx = (unsigned)handle;
    r.x.cx = (unsigned)count;
    r.x.dx = (unsigned)buf;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : (int)r.x.ax;
}

int t_write(int handle, const void *buf, int count)
{
    union REGS r;
    r.h.ah = 0x40;
    r.x.bx = (unsigned)handle;
    r.x.cx = (unsigned)count;
    r.x.dx = (unsigned)buf;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : (int)r.x.ax;
}

void t_close(int handle)
{
    union REGS r;
    r.h.ah = 0x3e;
    r.x.bx = (unsigned)handle;
    int86(0x21, &r, &r);
}

/* Duplicate `handle` (AH=45h): a new handle aliasing the same open file, sharing
 * its seek position. -1 on error. */
int t_dup(int handle)
{
    union REGS r;
    r.h.ah = 0x45;
    r.x.bx = (unsigned)handle;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : (int)r.x.ax;
}

/* Force `dst` to refer to the same open file as `src` (AH=46h), closing `dst`
 * first. 0 on success, -1 on error. The shell points handle 1 or 0 at a file. */
int t_dup2(int src, int dst)
{
    union REGS r;
    r.h.ah = 0x46;
    r.x.bx = (unsigned)src;
    r.x.cx = (unsigned)dst;
    int86(0x21, &r, &r);
    return r.x.cflag ? -1 : 0;
}

/* Seek (AH=42h). `whence`: 0 start, 1 current, 2 end. Returns the new offset, or
 * -1 on error. */
long t_lseek(int handle, long offset, int whence)
{
    union REGS r;
    r.h.ah = 0x42;
    r.h.al = (unsigned char)whence;
    r.x.bx = (unsigned)handle;
    r.x.cx = (unsigned)(offset >> 16);
    r.x.dx = (unsigned)(offset & 0xffff);
    int86(0x21, &r, &r);
    if (r.x.cflag) {
        return -1L;
    }
    return ((long)r.x.dx << 16) | (long)r.x.ax;
}

/* Open `path` for appending: open read/write if it exists, else create it, then
 * seek to end. Returns a handle or -1. Used for the `>>` redirect. */
int t_open_append(const char *path)
{
    int handle = t_open(path, 2);
    if (handle < 0) {
        return t_create(path); /* fresh file is already at offset 0 == end */
    }
    if (t_lseek(handle, 0L, 2) < 0L) {
        t_close(handle);
        return -1;
    }
    return handle;
}

/* Shared body for the path-only directory calls that return CF/AX. */
static int dir_call(unsigned char ah, const char *path)
{
    union REGS r;
    r.h.ah = ah;
    r.x.dx = (unsigned)path;
    int86(0x21, &r, &r);
    return r.x.cflag ? (int)r.x.ax : 0;
}

int t_mkdir(const char *path)
{
    return dir_call(0x39, path);
}

int t_rmdir(const char *path)
{
    return dir_call(0x3a, path);
}

int t_chdir(const char *path)
{
    return dir_call(0x3b, path);
}

int t_delete(const char *path)
{
    return dir_call(0x41, path);
}

int t_rename(const char *oldpath, const char *newpath)
{
    union REGS r;
    struct SREGS s;
    segread(&s);
    r.h.ah = 0x56;
    r.x.dx = (unsigned)oldpath; /* DS:DX = old name */
    r.x.di = (unsigned)newpath; /* ES:DI = new name */
    s.es = s.ds;
    int86x(0x21, &r, &r, &s);
    return r.x.cflag ? (int)r.x.ax : 0;
}

void t_getcwd(char *buf)
{
    union REGS r;
    r.h.ah = 0x47;
    r.h.dl = 0; /* default drive */
    r.x.si = (unsigned)buf;
    int86(0x21, &r, &r);
}

int t_findfirst(const char *spec, int attr, void *dta)
{
    union REGS r;
    r.h.ah = 0x1a; /* set DTA to our buffer */
    r.x.dx = (unsigned)dta;
    int86(0x21, &r, &r);
    r.h.ah = 0x4e;
    r.x.cx = (unsigned)attr;
    r.x.dx = (unsigned)spec;
    int86(0x21, &r, &r);
    return r.x.cflag ? (int)r.x.ax : 0;
}

int t_findnext(void *dta)
{
    union REGS r;
    r.h.ah = 0x1a;
    r.x.dx = (unsigned)dta;
    int86(0x21, &r, &r);
    r.h.ah = 0x4f;
    int86(0x21, &r, &r);
    return r.x.cflag ? (int)r.x.ax : 0;
}

void t_getdate(int *year, int *month, int *day, int *dow)
{
    union REGS r;
    r.h.ah = 0x2a;
    int86(0x21, &r, &r);
    *year = (int)r.x.cx;
    *month = (int)r.h.dh;
    *day = (int)r.h.dl;
    *dow = (int)r.h.al;
}

void t_gettime(int *hour, int *min, int *sec)
{
    union REGS r;
    r.h.ah = 0x2c;
    int86(0x21, &r, &r);
    *hour = (int)r.h.ch;
    *min = (int)r.h.cl;
    *sec = (int)r.h.dh;
}

int t_lastexit(void)
{
    union REGS r;
    r.h.ah = 0x4d;
    int86(0x21, &r, &r);
    return (int)r.h.al;
}

int t_getkey(void)
{
    union REGS r;
    r.h.ah = 0x07; /* read without echo, no Ctrl-C check */
    int86(0x21, &r, &r);
    return (int)r.h.al;
}

int t_readkey16(void)
{
    union REGS r;
    r.h.ah = 0x00; /* read keystroke: AH=scancode, AL=ASCII */
    int86(0x16, &r, &r);
    return (int)r.x.ax;
}

void t_setcursor(int row, int col)
{
    union REGS r;
    r.h.ah = 0x02;
    r.h.bh = 0; /* page 0 */
    r.h.dh = (unsigned char)row;
    r.h.dl = (unsigned char)col;
    int86(0x10, &r, &r);
}

int t_exists(const char *path)
{
    unsigned char dta[43];
    return t_findfirst(path, 0x10, dta) == 0;
}
