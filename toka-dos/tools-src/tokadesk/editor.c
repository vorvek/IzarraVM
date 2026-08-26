/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "editor.h"
#include "color.h"
#include "desktop.h"
#include "margo.h"
#include "v86.h"

#define ED_CAP 65534u
#define ED_SPLIT 32767u
#define ED_LINES 2048
#define ED_LINELEN 255
#define ED_ROWS 29
#define ED_COLS 111
#define ED_TEXT_Y (TAB_Y + 24)
#define MOD_NONE 0
#define MOD_CLOSE 1
#define MOD_FAIL 2

static char ed_lo[32767];
static char ed_hi[32767];
static unsigned short ed_off[ED_LINES];
static unsigned short ed_ll[ED_LINES];
static unsigned ed_used;
static int ed_n;
static int ed_open;
static int ed_dirty;
static int ed_row;
static int ed_col;
static int ed_top;
static int ed_left;
static int ed_modal;
static char ed_path[80];
static char ed_name[13];

static char ed_get(unsigned i)
{
    if (i < ED_SPLIT) {
        return ed_lo[i];
    }
    return ed_hi[i - ED_SPLIT];
}

static void ed_put(unsigned i, char c)
{
    if (i < ED_SPLIT) {
        ed_lo[i] = c;
    } else {
        ed_hi[i - ED_SPLIT] = c;
    }
}

static void ed_move(unsigned dst, unsigned src, unsigned n)
{
    if (dst == src || n == 0) {
        return;
    }
    if (dst > src) {
        while (n) {
            n--;
            ed_put(dst + n, ed_get(src + n));
        }
    } else {
        unsigned k;
        for (k = 0; k < n; k++) {
            ed_put(dst + k, ed_get(src + k));
        }
    }
}

static void ed_copy_out(char *dst, unsigned src, unsigned n)
{
    unsigned k;
    for (k = 0; k < n; k++) {
        dst[k] = ed_get(src + k);
    }
}

static void ed_reset(void)
{
    ed_used = 0;
    ed_n = 0;
    ed_row = 0;
    ed_col = 0;
    ed_top = 0;
    ed_left = 0;
    ed_dirty = 0;
    ed_modal = MOD_NONE;
}

static int commit_line(const char *s, int len)
{
    int i;

    if (ed_n >= ED_LINES) {
        return 0;
    }
    if ((unsigned)len > ED_LINELEN) {
        return 0;
    }
    if (ed_used + (unsigned)len > ED_CAP) {
        return 0;
    }
    ed_off[ed_n] = (unsigned short)ed_used;
    ed_ll[ed_n] = (unsigned short)len;
    for (i = 0; i < len; i++) {
        ed_put(ed_used + (unsigned)i, s[i]);
    }
    ed_used += (unsigned)len;
    ed_n++;
    return 1;
}

static int feed_file(void)
{
    char line[ED_LINELEN + 1];
    int col;
    unsigned inh;
    unsigned got;
    unsigned i;
    unsigned char chunk[256];
    int stop;

    ed_reset();
    bounce_str(B_PATH, ed_path);
    inh = dos_call(0x3D00, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    if (dos_err()) {
        return 0;
    }
    col = 0;
    stop = 0;
    while (!stop) {
        got = dos_call(0x3F00, inh, B_BUF_SZ, v86_bounce_off() + B_BUF, 0, 0);
        if (dos_err()) {
            dos_call(0x3E00, inh, 0, 0, 0, 0);
            ed_reset();
            return 0;
        }
        if (got == 0) {
            break;
        }
        {
            unsigned off = 0;
            while (off < got) {
                unsigned n = got - off;
                if (n > 256u) {
                    n = 256u;
                }
                bounce_get(B_BUF + off, chunk, n);
                for (i = 0; i < n; i++) {
                    unsigned char ch = (unsigned char)chunk[i];
                    if (ch == 0x1A) {
                        stop = 1;
                        break;
                    }
                    if (ch == '\r') {
                        continue;
                    }
                    if (ch == '\n') {
                        line[col] = 0;
                        if (!commit_line(line, col)) {
                            dos_call(0x3E00, inh, 0, 0, 0, 0);
                            ed_reset();
                            return 0;
                        }
                        col = 0;
                        continue;
                    }
                    if (ch == '\t') {
                        int sp = 8 - (col & 7);
                        while (sp && col < ED_LINELEN) {
                            line[col++] = ' ';
                            sp--;
                        }
                        if (sp) {
                            dos_call(0x3E00, inh, 0, 0, 0, 0);
                            ed_reset();
                            return 0;
                        }
                        continue;
                    }
                    if (col >= ED_LINELEN) {
                        dos_call(0x3E00, inh, 0, 0, 0, 0);
                        ed_reset();
                        return 0;
                    }
                    line[col++] = (char)ch;
                }
                off += n;
                if (stop) {
                    break;
                }
            }
        }
    }
    dos_call(0x3E00, inh, 0, 0, 0, 0);
    line[col] = 0;
    if (!commit_line(line, col)) {
        ed_reset();
        return 0;
    }
    if (ed_n < 1) {
        ed_reset();
        ed_n = 1;
        ed_off[0] = 0;
        ed_ll[0] = 0;
    }
    return 1;
}

int editor_is_open(void)
{
    return ed_open;
}

static void take_name(const char *path)
{
    const char *s;
    unsigned i;

    s = path;
    i = 0;
    while (path[i]) {
        if (path[i] == '\\') {
            s = path + i + 1;
        }
        i++;
    }
    i = 0;
    while (s[i] && i < 12u) {
        ed_name[i] = s[i];
        i++;
    }
    ed_name[i] = 0;
    i = 0;
    while (path[i] && i < 79u) {
        ed_path[i] = path[i];
        i++;
    }
    ed_path[i] = 0;
}

int editor_open(const char *path)
{
    take_name(path);
    if (!feed_file()) {
        ed_open = 0;
        return 0;
    }
    ed_open = 1;
    ed_dirty = 0;
    ed_modal = MOD_NONE;
    return 1;
}

static void clamp_view(void)
{
    if (ed_row < 0) {
        ed_row = 0;
    }
    if (ed_row >= ed_n) {
        ed_row = ed_n - 1;
    }
    if (ed_col < 0) {
        ed_col = 0;
    }
    if (ed_col > (int)ed_ll[ed_row]) {
        ed_col = (int)ed_ll[ed_row];
    }
    if (ed_row < ed_top) {
        ed_top = ed_row;
    }
    if (ed_row >= ed_top + ED_ROWS) {
        ed_top = ed_row - ED_ROWS + 1;
    }
    if (ed_col < ed_left) {
        ed_left = ed_col;
    }
    if (ed_col >= ed_left + ED_COLS) {
        ed_left = ed_col - ED_COLS + 1;
    }
}

static int insert_char(char ch)
{
    unsigned pos;
    unsigned tail;
    int i;

    if (ed_ll[ed_row] >= ED_LINELEN) {
        return 0;
    }
    if (ed_used + 1u > ED_CAP) {
        return 0;
    }
    pos = (unsigned)ed_off[ed_row] + (unsigned)ed_col;
    tail = ed_used - pos;
    ed_move(pos + 1u, pos, tail);
    ed_put(pos, ch);
    ed_used++;
    ed_ll[ed_row]++;
    for (i = ed_row + 1; i < ed_n; i++) {
        ed_off[i] = (unsigned short)((unsigned)ed_off[i] + 1u);
    }
    ed_col++;
    ed_dirty = 1;
    return 1;
}

static int split_line(void)
{
    unsigned pos;
    unsigned left;
    unsigned right;
    int i;

    if (ed_n >= ED_LINES) {
        return 0;
    }
    pos = (unsigned)ed_off[ed_row] + (unsigned)ed_col;
    left = (unsigned)ed_col;
    right = (unsigned)ed_ll[ed_row] - left;
    for (i = ed_n; i > ed_row + 1; i--) {
        ed_off[i] = ed_off[i - 1];
        ed_ll[i] = ed_ll[i - 1];
    }
    ed_n++;
    ed_ll[ed_row] = (unsigned short)left;
    ed_off[ed_row + 1] = (unsigned short)pos;
    ed_ll[ed_row + 1] = (unsigned short)right;
    ed_row++;
    ed_col = 0;
    ed_dirty = 1;
    return 1;
}

static int delete_char(void)
{
    unsigned pos;
    unsigned tail;
    int i;

    if (ed_col < (int)ed_ll[ed_row]) {
        pos = (unsigned)ed_off[ed_row] + (unsigned)ed_col;
        tail = ed_used - pos - 1u;
        ed_move(pos, pos + 1u, tail);
        ed_used--;
        ed_ll[ed_row]--;
        for (i = ed_row + 1; i < ed_n; i++) {
            ed_off[i] = (unsigned short)((unsigned)ed_off[i] - 1u);
        }
        ed_dirty = 1;
        return 1;
    }
    if (ed_row + 1 >= ed_n) {
        return 0;
    }
    if ((unsigned)ed_ll[ed_row] + (unsigned)ed_ll[ed_row + 1] > ED_LINELEN) {
        return 0;
    }
    ed_ll[ed_row] = (unsigned short)((unsigned)ed_ll[ed_row] +
                                    (unsigned)ed_ll[ed_row + 1]);
    for (i = ed_row + 1; i < ed_n - 1; i++) {
        ed_off[i] = ed_off[i + 1];
        ed_ll[i] = ed_ll[i + 1];
    }
    ed_n--;
    ed_dirty = 1;
    return 1;
}

static int backspace(void)
{
    if (ed_col > 0) {
        ed_col--;
        return delete_char();
    }
    if (ed_row == 0) {
        return 0;
    }
    ed_row--;
    ed_col = (int)ed_ll[ed_row];
    return delete_char();
}

static int save_file(void)
{
    unsigned outh;
    unsigned fill;
    int r;
    unsigned k;

    bounce_str(B_PATH, ed_path);
    outh = dos_call(0x3C00, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    if (dos_err()) {
        return 0;
    }
    fill = 0;
    for (r = 0; r < ed_n; r++) {
        unsigned len = (unsigned)ed_ll[r];
        unsigned src = (unsigned)ed_off[r];
        if (fill + len + 2u > B_BUF_SZ) {
            dos_call(0x4000, outh, fill, v86_bounce_off() + B_BUF, 0, 0);
            if (dos_err()) {
                dos_call(0x3E00, outh, 0, 0, 0, 0);
                return 0;
            }
            fill = 0;
        }
        for (k = 0; k < len; k++) {
            unsigned char ch = (unsigned char)ed_get(src + k);
            bounce_mem(B_BUF + fill, &ch, 1);
            fill++;
        }
        {
            unsigned char cr = 13;
            unsigned char lf = 10;
            bounce_mem(B_BUF + fill, &cr, 1);
            fill++;
            bounce_mem(B_BUF + fill, &lf, 1);
            fill++;
        }
    }
    if (fill) {
        dos_call(0x4000, outh, fill, v86_bounce_off() + B_BUF, 0, 0);
        if (dos_err()) {
            dos_call(0x3E00, outh, 0, 0, 0, 0);
            return 0;
        }
    }
    dos_call(0x3E00, outh, 0, 0, 0, 0);
    ed_dirty = 0;
    return 1;
}

static void draw_close_modal(void)
{
    int x = RAIL_W + 180;
    int y = 220;

    margo_raised(x, y, 360, 110, COL_PANEL_FACE);
    if (ed_modal == MOD_FAIL) {
        margo_text8(x + 16, y + 24, "Could not save the file.", COL_INK);
        margo_text8(x + 16, y + 72, "Esc = keep editing", COL_LABEL);
        return;
    }
    margo_text8(x + 16, y + 20, "File has changes.", COL_INK);
    margo_raised(x + 16, y + 56, 80, 24, COL_PANEL_FACE);
    margo_text8(x + 28, y + 64, "Save", COL_INK);
    margo_raised(x + 108, y + 56, 96, 24, COL_PANEL_FACE);
    margo_text8(x + 116, y + 64, "Discard", COL_INK);
    margo_raised(x + 216, y + 56, 80, 24, COL_PANEL_FACE);
    margo_text8(x + 228, y + 64, "Cancel", COL_INK);
}

void editor_draw(void)
{
    int r;
    int x0 = RAIL_W + 4;
    char line[ED_LINELEN + 1];
    char st[40];
    int n;

    margo_raised(RAIL_W, TAB_Y, DESK_W - RAIL_W, TAB_H, COL_FIELD);
    margo_text8(x0, TAB_Y + 8, ed_name, COL_INK);
    if (ed_dirty) {
        margo_text8(x0 + 8 * 14, TAB_Y + 8, "*", COL_LOGO_RED);
    }
    margo_raised(RAIL_W + 200, TAB_Y + 4, 56, 16, COL_PANEL_FACE);
    margo_text8(RAIL_W + 208, TAB_Y + 8, "Save", COL_INK);
    n = 0;
    st[n++] = 'L';
    {
        int v = ed_row + 1;
        char tmp[8];
        int t = 0;
        if (v == 0) {
            tmp[t++] = '0';
        }
        while (v && t < 7) {
            tmp[t++] = (char)('0' + (v % 10));
            v /= 10;
        }
        while (t) {
            st[n++] = tmp[--t];
        }
    }
    st[n++] = ':';
    {
        int v = ed_col + 1;
        char tmp[8];
        int t = 0;
        if (v == 0) {
            tmp[t++] = '0';
        }
        while (v && t < 7) {
            tmp[t++] = (char)('0' + (v % 10));
            v /= 10;
        }
        while (t) {
            st[n++] = tmp[--t];
        }
    }
    st[n] = 0;
    margo_text8(DESK_W - 80, TAB_Y + 8, st, COL_LABEL);

    margo_recessed(RAIL_W + 4, ED_TEXT_Y, DESK_W - RAIL_W - 8, TAB_H - 28, COL_FIELD);
    for (r = 0; r < ED_ROWS; r++) {
        int yr = ED_TEXT_Y + 4 + r * 16;
        int idx = ed_top + r;
        int len;
        int start;
        int vis;
        if (idx >= ed_n) {
            break;
        }
        len = (int)ed_ll[idx];
        start = ed_left;
        if (start > len) {
            start = len;
        }
        vis = len - start;
        if (vis > ED_COLS) {
            vis = ED_COLS;
        }
        ed_copy_out(line, (unsigned)ed_off[idx] + (unsigned)start, (unsigned)vis);
        line[vis] = 0;
        if (idx == ed_row) {
            int cx = RAIL_W + 8 + (ed_col - ed_left) * 8;
            if (ed_col >= ed_left && ed_col - ed_left < ED_COLS) {
                margo_fill(cx, yr, 8, 16, COL_PANEL_FACE);
            }
        }
        margo_text16(RAIL_W + 8, yr, line, COL_INK);
    }
    if (ed_modal) {
        draw_close_modal();
    }
}

static void request_close(void)
{
    if (ed_dirty) {
        ed_modal = MOD_CLOSE;
        return;
    }
    ed_open = 0;
    ed_reset();
}

int editor_want_close(void)
{
    return ed_open == 0;
}

int editor_busy(void)
{
    return ed_open && ed_modal;
}

static void modal_key(unsigned ax)
{
    unsigned char ah = (unsigned char)(ax >> 8);
    unsigned char al = (unsigned char)ax;

    if (ed_modal == MOD_FAIL) {
        if (ah == 0x01) {
            ed_modal = MOD_NONE;
        }
        return;
    }
    if (ah == 0x01) {
        ed_modal = MOD_NONE;
        return;
    }
    if (ah == 0x1C) {
        if (save_file()) {
            ed_open = 0;
            ed_reset();
        } else {
            ed_modal = MOD_FAIL;
        }
        return;
    }
    if (al == 'd' || al == 'D') {
        ed_open = 0;
        ed_reset();
    }
}

int editor_key(unsigned ax)
{
    unsigned char ah = (unsigned char)(ax >> 8);
    unsigned char al = (unsigned char)ax;

    if (ed_modal) {
        modal_key(ax);
        return editor_want_close();
    }
    if (ah == 0x01) {
        request_close();
        return editor_want_close();
    }
    if (ah == 0x3C) {
        if (!save_file()) {
            ed_modal = MOD_FAIL;
        }
        return 0;
    }
    if (ah == 0x4B) {
        if (ed_col > 0) {
            ed_col--;
        } else if (ed_row > 0) {
            ed_row--;
            ed_col = (int)ed_ll[ed_row];
        }
    } else if (ah == 0x4D) {
        if (ed_col < (int)ed_ll[ed_row]) {
            ed_col++;
        } else if (ed_row + 1 < ed_n) {
            ed_row++;
            ed_col = 0;
        }
    } else if (ah == 0x48) {
        if (ed_row > 0) {
            ed_row--;
        }
    } else if (ah == 0x50) {
        if (ed_row + 1 < ed_n) {
            ed_row++;
        }
    } else if (ah == 0x47) {
        ed_col = 0;
    } else if (ah == 0x4F) {
        ed_col = (int)ed_ll[ed_row];
    } else if (ah == 0x49) {
        ed_row -= ED_ROWS;
        if (ed_row < 0) {
            ed_row = 0;
        }
    } else if (ah == 0x51) {
        ed_row += ED_ROWS;
        if (ed_row >= ed_n) {
            ed_row = ed_n - 1;
        }
    } else if (ah == 0x0E) {
        backspace();
    } else if (ah == 0x53) {
        delete_char();
    } else if (ah == 0x1C) {
        split_line();
    } else if (ah == 0x0F) {
        int sp = 8 - (ed_col & 7);
        if (sp > ED_LINELEN - (int)ed_ll[ed_row]) {
            sp = ED_LINELEN - (int)ed_ll[ed_row];
        }
        while (sp > 0) {
            if (!insert_char(' ')) {
                break;
            }
            sp--;
        }
    } else if (al >= 32 && al < 127) {
        insert_char((char)al);
    }
    clamp_view();
    return editor_want_close();
}

void editor_click(int x, int y)
{
    int mx = RAIL_W + 180;
    int my = 220;

    if (ed_modal == MOD_CLOSE) {
        if (y >= my + 56 && y < my + 80) {
            if (x >= mx + 16 && x < mx + 96) {
                if (save_file()) {
                    ed_open = 0;
                    ed_reset();
                } else {
                    ed_modal = MOD_FAIL;
                }
            } else if (x >= mx + 108 && x < mx + 204) {
                ed_open = 0;
                ed_reset();
            } else if (x >= mx + 216 && x < mx + 296) {
                ed_modal = MOD_NONE;
            }
        }
        return;
    }
    if (ed_modal == MOD_FAIL) {
        ed_modal = MOD_NONE;
        return;
    }
    if (y < ED_TEXT_Y && x >= RAIL_W + 200 && x < RAIL_W + 256) {
        if (!save_file()) {
            ed_modal = MOD_FAIL;
        }
        return;
    }
    if (y >= ED_TEXT_Y && y < CONS_Y && x >= RAIL_W + 8) {
        int row = (y - (ED_TEXT_Y + 4)) / 16;
        int col = (x - (RAIL_W + 8)) / 8;
        ed_row = ed_top + row;
        ed_col = ed_left + col;
        clamp_view();
    }
}
