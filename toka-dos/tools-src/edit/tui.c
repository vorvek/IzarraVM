/* TokaEdit DOS TUI hardware layer implementation. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <bios.h>
#include <dos.h>
#include <i86.h>
#include <string.h>
#include "tui.h"

static char far *vram = (char far *)MK_FP(0xB800, 0);

static unsigned cell_off(int row, int col) {
    return (unsigned)((unsigned)(row * 80 + col) * 2);
}

void scr_init(void) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.h.ah = 0x0F;
    int86(0x10, &r, &r);
    if (r.h.al != 3) {
        memset(&r, 0, sizeof(r));
        r.x.ax = 0x0003;
        int86(0x10, &r, &r);
    }
}

void scr_exit(void) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.x.ax = 0x0003;
    int86(0x10, &r, &r);
}

void scr_put(int row, int col, const char *s, unsigned char attr) {
    unsigned off;
    off = cell_off(row, col);
    while (*s) {
        vram[off] = *s;
        vram[off + 1] = (char)attr;
        off += 2;
        s++;
    }
}

void scr_putc(int row, int col, char ch, unsigned char attr) {
    unsigned off;
    off = cell_off(row, col);
    vram[off] = ch;
    vram[off + 1] = (char)attr;
}

void scr_fill(int row, int col, int w, int h, char ch, unsigned char attr) {
    int r, c;
    unsigned off;
    for (r = 0; r < h; r++) {
        off = cell_off(row + r, col);
        for (c = 0; c < w; c++) {
            vram[off] = ch;
            vram[off + 1] = (char)attr;
            off += 2;
        }
    }
}

void scr_cursor(int row, int col) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.h.ah = 0x02;
    r.h.bh = 0;
    r.h.dh = (unsigned char)row;
    r.h.dl = (unsigned char)col;
    int86(0x10, &r, &r);
}

void scr_cursor_hide(void) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.h.ah = 0x02;
    r.h.bh = 0;
    r.h.dh = 25;
    r.h.dl = 0;
    int86(0x10, &r, &r);
}

void scr_save_rect(int row, int col, int w, int h, char *buf) {
    int r;
    unsigned off;
    for (r = 0; r < h; r++) {
        off = cell_off(row + r, col);
        _fmemcpy(buf + (long)r * w * 2, vram + off, (unsigned)(w * 2));
    }
}

void scr_restore_rect(int row, int col, int w, int h, const char *buf) {
    int r;
    unsigned off;
    for (r = 0; r < h; r++) {
        off = cell_off(row + r, col);
        _fmemcpy(vram + off, buf + (long)r * w * 2, (unsigned)(w * 2));
    }
}

/* ---- mouse ---- */

static int mouse_ok = 0;
static int mouse_inited = 0;

void mouse_init(void) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.x.ax = 0x0000;
    int86(0x33, &r, &r);
    mouse_ok = (r.x.ax == 0xFFFF);
    mouse_inited = 1;
}

int mouse_present(void) {
    return mouse_inited ? mouse_ok : 0;
}

void mouse_show(void) {
    union REGS r;
    if (!mouse_ok) return;
    memset(&r, 0, sizeof(r));
    r.x.ax = 0x0001;
    int86(0x33, &r, &r);
}

void mouse_hide(void) {
    union REGS r;
    if (!mouse_ok) return;
    memset(&r, 0, sizeof(r));
    r.x.ax = 0x0002;
    int86(0x33, &r, &r);
}

static void mouse_status(int *buttons, int *row, int *col) {
    union REGS r;
    memset(&r, 0, sizeof(r));
    r.x.ax = 0x0003;
    int86(0x33, &r, &r);
    *buttons = r.x.bx & 1;
    *col = r.x.cx / 8;
    *row = r.x.dx / 8;
}

/* ---- input ---- */

void ev_wait(Event *e) {
    /* Edge-detection state must survive across calls: ev_wait returns after
     * ONE event, but an Alt tap or a mouse press-drag-release spans several
     * returned events. Automatic locals here would re-arm on every call
     * (spurious ALT_TAP after Alt+letter, DOWN refiring instead of DRAG/UP). */
    static int prev_alt_down = 0;
    static int key_seen_while_alt = 0;
    static int prev_btn = 0;
    static int prev_row = 0, prev_col = 0;
    static int have_prev_mouse = 0;

    for (;;) {
        int shift_state;
        int alt_down;
        union REGS r;

        shift_state = _bios_keybrd(_KEYBRD_SHIFTSTATUS);
        alt_down = (shift_state & 0x08) ? 1 : 0;

        if (_bios_keybrd(_KEYBRD_READY)) {
            unsigned key;
            unsigned char scan, ascii;

            key = _bios_keybrd(_KEYBRD_READ);
            scan = (unsigned char)(key >> 8);
            ascii = (unsigned char)(key & 0xFF);
            if (ascii == 0xE0 && scan != 0) {
                ascii = 0;
            }

            if (alt_down) {
                key_seen_while_alt = 1;
            }

            e->kind = EV_KEY;
            e->scan = scan;
            e->ascii = ascii;
            e->mods = 0;
            if (shift_state & 0x03) e->mods |= 0x01;
            if (shift_state & 0x04) e->mods |= 0x02;
            if (shift_state & 0x08) e->mods |= 0x04;
            e->mrow = 0;
            e->mcol = 0;
            return;
        }

        if (prev_alt_down && !alt_down) {
            if (!key_seen_while_alt) {
                e->kind = EV_ALT_TAP;
                e->scan = 0;
                e->ascii = 0;
                e->mods = 0;
                e->mrow = 0;
                e->mcol = 0;
                prev_alt_down = alt_down;
                return;
            }
            key_seen_while_alt = 0;
        }
        if (alt_down && !prev_alt_down) {
            key_seen_while_alt = 0;
        }
        prev_alt_down = alt_down;

        if (mouse_present()) {
            int buttons, row, col;
            mouse_status(&buttons, &row, &col);

            if (buttons && !prev_btn) {
                e->kind = EV_MOUSE_DOWN;
                e->mrow = row;
                e->mcol = col;
                e->scan = 0;
                e->ascii = 0;
                e->mods = 0;
                prev_btn = buttons;
                prev_row = row;
                prev_col = col;
                have_prev_mouse = 1;
                return;
            }
            if (buttons && prev_btn) {
                if (!have_prev_mouse || row != prev_row || col != prev_col) {
                    e->kind = EV_MOUSE_DRAG;
                    e->mrow = row;
                    e->mcol = col;
                    e->scan = 0;
                    e->ascii = 0;
                    e->mods = 0;
                    prev_row = row;
                    prev_col = col;
                    have_prev_mouse = 1;
                    return;
                }
            }
            if (!buttons && prev_btn) {
                e->kind = EV_MOUSE_UP;
                e->mrow = row;
                e->mcol = col;
                e->scan = 0;
                e->ascii = 0;
                e->mods = 0;
                prev_btn = buttons;
                prev_row = row;
                prev_col = col;
                have_prev_mouse = 1;
                return;
            }
            prev_btn = buttons;
        }

        memset(&r, 0, sizeof(r));
        int86(0x28, &r, &r);
    }
}
