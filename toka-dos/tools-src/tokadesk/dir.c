/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

#include "dir.h"
#include "color.h"
#include "desktop.h"
#include "editor.h"
#include "margo.h"
#include "v86.h"

#define KIND_UP 1
#define KIND_DIR 2
#define KIND_FILE 3
#define DIR_MAX 256
#define DIR_ROWS 27
#define PANE_W 448
#define LIST_Y 48
#define LIST_H 444
#define BTN_Y 492
#define BTN_H 32
#define MOD_NONE 0
#define MOD_DEL 1
#define MOD_MKDIR 2
#define CLIP_NONE 0
#define CLIP_COPY 1
#define CLIP_MOVE 2
#define ATTR_DIR 0x10

#pragma pack(1)
typedef struct {
    unsigned char reserved[21];
    unsigned char attr;
    unsigned short time;
    unsigned short date;
    unsigned long size;
    char name[13];
} FindDta;
#pragma pack()

typedef struct {
    unsigned char kind;
    unsigned char attr;
    unsigned long size;
    char name[13];
} DirEntry;

typedef struct {
    unsigned char drive;
    unsigned char ready;
    char path[80];
    int cursor;
    int scroll;
    int n;
    DirEntry ents[DIR_MAX];
} Pane;

static Pane panes[2];
static int active;
static int modal;
static char prompt[13];
static int prompt_len;
static char clip_path[80];
static int clip_op;
static int last_click_pane;
static int last_click_row;

static const unsigned short icon_up[16] = {
    0x0000, 0x0100, 0x0380, 0x07C0, 0x0FE0, 0x1FF0, 0x3FF8, 0x7FFC,
    0x0380, 0x0380, 0x0380, 0x0380, 0x0380, 0x0380, 0x0000, 0x0000
};
static const unsigned short icon_dir[16] = {
    0x0000, 0x7C00, 0x7E00, 0x7FFF, 0x4001, 0x4001, 0x4001, 0x4001,
    0x4001, 0x4001, 0x4001, 0x4001, 0x4001, 0x7FFF, 0x0000, 0x0000
};
static const unsigned short icon_file[16] = {
    0x0000, 0x7FF0, 0x4048, 0x4044, 0x4042, 0x407E, 0x4002, 0x4002,
    0x4002, 0x4002, 0x4002, 0x4002, 0x4002, 0x7FFE, 0x0000, 0x0000
};

static int is_root(const char *p)
{
    return p[0] && p[1] == ':' && p[2] == '\\' && p[3] == 0;
}

static int ci_eq(const char *a, const char *b)
{
    unsigned char ca;
    unsigned char cb;

    while (*a || *b) {
        ca = (unsigned char)*a;
        cb = (unsigned char)*b;
        if (ca >= 'a' && ca <= 'z') {
            ca = (unsigned char)(ca - 32);
        }
        if (cb >= 'a' && cb <= 'z') {
            cb = (unsigned char)(cb - 32);
        }
        if (ca != cb) {
            return 0;
        }
        a++;
        b++;
    }
    return 1;
}

static int name_cmp(const char *a, const char *b)
{
    unsigned char ca;
    unsigned char cb;

    while (*a || *b) {
        ca = (unsigned char)*a;
        cb = (unsigned char)*b;
        if (ca >= 'a' && ca <= 'z') {
            ca = (unsigned char)(ca - 32);
        }
        if (cb >= 'a' && cb <= 'z') {
            cb = (unsigned char)(cb - 32);
        }
        if (ca != cb) {
            return (int)ca - (int)cb;
        }
        a++;
        b++;
    }
    return 0;
}

static void set_root(char *p, unsigned char drive)
{
    p[0] = (char)('A' + drive);
    p[1] = ':';
    p[2] = '\\';
    p[3] = 0;
}

static void path_join(char *dst, const char *dir, const char *name)
{
    unsigned i;

    i = 0;
    while (dir[i] && i < 78u) {
        dst[i] = dir[i];
        i++;
    }
    if (!(i == 3u && dst[2] == '\\') && i && dst[i - 1] != '\\' && i < 78u) {
        dst[i++] = '\\';
    }
    while (*name && i < 79u) {
        dst[i++] = *name++;
    }
    dst[i] = 0;
}

static void path_parent(char *p)
{
    int i;

    if (is_root(p)) {
        return;
    }
    i = 0;
    while (p[i]) {
        i++;
    }
    if (i == 0) {
        return;
    }
    i--;
    while (i > 2 && p[i] != '\\') {
        i--;
    }
    if (i == 2) {
        p[3] = 0;
    } else {
        p[i] = 0;
    }
}

static void filespec(char *dst, const char *dir)
{
    unsigned i;

    i = 0;
    while (dir[i] && i < 76u) {
        dst[i] = dir[i];
        i++;
    }
    if (i && dst[i - 1] != '\\') {
        dst[i++] = '\\';
    }
    dst[i++] = '*';
    dst[i++] = '.';
    dst[i++] = '*';
    dst[i] = 0;
}

static void sort_pane(Pane *p)
{
    int i;
    int j;
    DirEntry tmp;

    for (i = 1; i < p->n; i++) {
        tmp = p->ents[i];
        j = i;
        while (j > 0) {
            DirEntry *a = &p->ents[j - 1];
            int less;

            if (tmp.kind == KIND_UP) {
                less = 1;
            } else if (a->kind == KIND_UP) {
                less = 0;
            } else if (tmp.kind != a->kind) {
                less = tmp.kind < a->kind;
            } else {
                less = name_cmp(tmp.name, a->name) < 0;
            }
            if (!less) {
                break;
            }
            p->ents[j] = *a;
            j--;
        }
        p->ents[j] = tmp;
    }
}

static void set_dta(void)
{
    dos_call(0x1A00, 0, 0, v86_bounce_off() + B_DTA, 0, 0);
}

static void scan_pane(Pane *p)
{
    FindDta dta;
    char spec[84];
    unsigned ax;

    p->n = 0;
    p->cursor = 0;
    p->scroll = 0;
    filespec(spec, p->path);
    set_dta();
    bounce_str(B_PATH, spec);
    ax = dos_call(0x4E00, 0, 0x17, v86_bounce_off() + B_PATH, 0, 0);
    if (dos_err() || ax != 0) {
        p->ready = 0;
        return;
    }
    p->ready = 1;
    for (;;) {
        DirEntry *e;

        bounce_get(B_DTA, &dta, sizeof(dta));
        if (dta.name[0] == '.' && dta.name[1] == 0) {
            /* skip . */
        } else if (dta.name[0] == '.' && dta.name[1] == '.' && dta.name[2] == 0) {
            if (!is_root(p->path) && p->n < DIR_MAX) {
                e = &p->ents[p->n++];
                e->kind = KIND_UP;
                e->attr = ATTR_DIR;
                e->size = 0;
                e->name[0] = 'U';
                e->name[1] = 'p';
                e->name[2] = 0;
            }
        } else if (p->n < DIR_MAX) {
            unsigned i;

            e = &p->ents[p->n++];
            e->attr = dta.attr;
            e->size = dta.size;
            e->kind = (dta.attr & ATTR_DIR) ? KIND_DIR : KIND_FILE;
            for (i = 0; i < 12u; i++) {
                e->name[i] = dta.name[i];
                if (dta.name[i] == 0) {
                    break;
                }
            }
            e->name[12] = 0;
        }
        dos_call(0x4F00, 0, 0, 0, 0, 0);
        if (dos_err()) {
            break;
        }
    }
    sort_pane(p);
}

static int ext_is(const char *name, const char *ext)
{
    const char *dot;

    dot = name;
    while (*dot && *dot != '.') {
        dot++;
    }
    if (*dot != '.') {
        return 0;
    }
    return ci_eq(dot + 1, ext);
}

static unsigned icon_fg(const DirEntry *e)
{
    if (e->kind == KIND_UP) {
        return COL_LOGO_RED;
    }
    if (e->kind == KIND_DIR) {
        return COL_FOLDER;
    }
    if (ext_is(e->name, "COM") || ext_is(e->name, "EXE")) {
        return COL_LOGO_RED;
    }
    if (ext_is(e->name, "BAT")) {
        return COL_MUTED;
    }
    return COL_INK;
}

static const unsigned short *icon_bits(const DirEntry *e)
{
    if (e->kind == KIND_UP) {
        return icon_up;
    }
    if (e->kind == KIND_DIR) {
        return icon_dir;
    }
    return icon_file;
}

static int pane_x(int which)
{
    return RAIL_W + which * PANE_W;
}

static void fmt_size(char *dst, unsigned long n)
{
    char tmp[11];
    int i;
    int j;

    if (n == 0) {
        dst[0] = '0';
        dst[1] = 0;
        return;
    }
    i = 0;
    while (n && i < 10) {
        tmp[i++] = (char)('0' + (n % 10u));
        n /= 10u;
    }
    j = 0;
    while (i) {
        dst[j++] = tmp[--i];
    }
    dst[j] = 0;
}

static int d_readonly(const Pane *p)
{
    return p->drive == 3;
}

static void draw_pane(int which)
{
    Pane *p;
    int x;
    int i;
    int y;
    int row;
    static const char *btns[] = {
        "MkDir", "Copy", "Move", "Paste", "Del", "View", "Edit"
    };

    p = &panes[which];
    x = pane_x(which);
    margo_raised(x, TAB_Y, PANE_W, TAB_H, COL_FIELD);
    if (which == 0) {
        margo_fill(x + PANE_W - 1, TAB_Y, 1, TAB_H, COL_BLACK);
    }
    if (which == active) {
        margo_outline(x + 2, TAB_Y + 2, PANE_W - 4, TAB_H - 4, COL_LOGO_RED);
    }

    {
        int dx = x + 8;
        unsigned char d;
        for (d = 0; d < 4; d++) {
            unsigned face;
            if (d == 1) {
                continue;
            }
            face = (p->drive == d) ? COL_PANEL_FACE : COL_FACEPLATE;
            if (p->drive == d) {
                margo_recessed(dx, TAB_Y + 4, 28, 16, face);
            } else {
                margo_raised(dx, TAB_Y + 4, 28, 16, face);
            }
            {
                char lab[3];
                lab[0] = (char)('A' + d);
                lab[1] = ':';
                lab[2] = 0;
                margo_text8(dx + 6, TAB_Y + 8, lab, COL_INK);
            }
            dx += 36;
        }
        margo_text8(dx + 4, TAB_Y + 8, p->path, COL_LABEL);
    }

    margo_recessed(x + 4, LIST_Y, PANE_W - 8, LIST_H, COL_FIELD);
    if (!p->ready) {
        margo_text16(x + 16, LIST_Y + 16, "Not ready", COL_LABEL);
    } else {
        for (row = 0; row < DIR_ROWS; row++) {
            i = p->scroll + row;
            y = LIST_Y + 6 + row * 16;
            if (i >= p->n) {
                break;
            }
            if (i == p->cursor) {
                margo_fill(x + 6, y, PANE_W - 12, 16, COL_PANEL_FACE);
            }
            margo_icon16(x + 8, y, icon_bits(&p->ents[i]), icon_fg(&p->ents[i]));
            margo_text16(x + 26, y, p->ents[i].name,
                         (i == p->cursor) ? COL_INK : COL_INK);
            if (p->ents[i].kind == KIND_FILE) {
                char sz[12];
                fmt_size(sz, p->ents[i].size);
                margo_text16(x + PANE_W - 8 - 8 * 7, y, sz, COL_LABEL);
            }
        }
    }

    {
        int bx = x + 4;
        int b;
        for (b = 0; b < 7; b++) {
            int muted = (b == 5) || (d_readonly(p) && (b == 0 || b == 2 || b == 4 || b == 6));
            if (muted) {
                margo_raised(bx, BTN_Y + 4, 60, 24, COL_FACEPLATE);
                margo_text8(bx + 6, BTN_Y + 12, btns[b], COL_MUTED);
            } else {
                margo_raised(bx, BTN_Y + 4, 60, 24, COL_PANEL_FACE);
                margo_text8(bx + 6, BTN_Y + 12, btns[b], COL_INK);
            }
            bx += 63;
        }
    }
}

static void draw_modal(void)
{
    int x = RAIL_W + 160;
    int y = 200;

    margo_raised(x, y, 400, 120, COL_PANEL_FACE);
    if (modal == MOD_DEL) {
        margo_text8(x + 16, y + 24, "Delete this file?", COL_INK);
        margo_text8(x + 16, y + 40, prompt, COL_LABEL);
        margo_text8(x + 16, y + 80, "Enter = yes   Esc = no", COL_LABEL);
    } else if (modal == MOD_MKDIR) {
        margo_text8(x + 16, y + 24, "Make directory:", COL_INK);
        margo_recessed(x + 16, y + 48, 200, 24, COL_FIELD);
        margo_text8(x + 20, y + 54, prompt, COL_INK);
        margo_text8(x + 16, y + 88, "Enter = create   Esc = cancel", COL_LABEL);
    }
}

void dir_draw(void)
{
    draw_pane(0);
    draw_pane(1);
    if (modal) {
        draw_modal();
    }
}

static DirEntry *cur_ent(Pane *p)
{
    if (!p->ready || p->cursor < 0 || p->cursor >= p->n) {
        return 0;
    }
    return &p->ents[p->cursor];
}

static int has_tokadesk(const Pane *p)
{
    int i;

    for (i = 0; i < p->n; i++) {
        if (p->ents[i].kind == KIND_FILE && ci_eq(p->ents[i].name, "TOKADESK.EXE")) {
            return 1;
        }
    }
    return 0;
}

int dir_init(void)
{
    set_root(panes[0].path, 2);
    set_root(panes[1].path, 2);
    panes[0].drive = 2;
    panes[1].drive = 2;
    active = 0;
    modal = 0;
    clip_op = CLIP_NONE;
    last_click_pane = -1;
    last_click_row = -1;
    set_dta();
    scan_pane(&panes[0]);
    scan_pane(&panes[1]);
    return has_tokadesk(&panes[0]);
}

static void rescan_matching(void)
{
    scan_pane(&panes[0]);
    scan_pane(&panes[1]);
}

static void enter_row(Pane *p)
{
    DirEntry *e = cur_ent(p);
    char next[80];

    if (!e) {
        return;
    }
    if (e->kind == KIND_UP) {
        path_parent(p->path);
        scan_pane(p);
        return;
    }
    if (e->kind == KIND_DIR) {
        path_join(next, p->path, e->name);
        if (next[79]) {
            return;
        }
        {
            unsigned i = 0;
            while (next[i]) {
                p->path[i] = next[i];
                i++;
            }
            p->path[i] = 0;
        }
        scan_pane(p);
    }
}

static void set_drive(Pane *p, unsigned char d)
{
    if (d == 1) {
        return;
    }
    p->drive = d;
    set_root(p->path, d);
    scan_pane(p);
}

static int copy_file(const char *src, const char *dst)
{
    unsigned inh;
    unsigned outh;
    unsigned n;

    bounce_str(B_PATH, src);
    inh = dos_call(0x3D00, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    if (dos_err()) {
        return 0;
    }
    bounce_str(B_PATH2, dst);
    outh = dos_call(0x3C00, 0, 0, v86_bounce_off() + B_PATH2, 0, 0);
    if (dos_err()) {
        dos_call(0x3E00, inh, 0, 0, 0, 0);
        return 0;
    }
    for (;;) {
        n = dos_call(0x3F00, inh, B_BUF_SZ, v86_bounce_off() + B_BUF, 0, 0);
        if (dos_err()) {
            dos_call(0x3E00, inh, 0, 0, 0, 0);
            dos_call(0x3E00, outh, 0, 0, 0, 0);
            return 0;
        }
        if (n == 0) {
            break;
        }
        dos_call(0x4000, outh, n, v86_bounce_off() + B_BUF, 0, 0);
        if (dos_err()) {
            dos_call(0x3E00, inh, 0, 0, 0, 0);
            dos_call(0x3E00, outh, 0, 0, 0, 0);
            return 0;
        }
    }
    dos_call(0x3E00, inh, 0, 0, 0, 0);
    dos_call(0x3E00, outh, 0, 0, 0, 0);
    return 1;
}

static int del_file(const char *path)
{
    bounce_str(B_PATH, path);
    dos_call(0x4100, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    return !dos_err();
}

static int mkdir_path(const char *path)
{
    bounce_str(B_PATH, path);
    dos_call(0x3900, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    return !dos_err();
}

static int rmdir_path(const char *path)
{
    bounce_str(B_PATH, path);
    dos_call(0x3A00, 0, 0, v86_bounce_off() + B_PATH, 0, 0);
    return !dos_err();
}

static int rename_path(const char *src, const char *dst)
{
    bounce_str(B_PATH, src);
    bounce_str(B_PATH2, dst);
    dos_call(0x5600, 0, 0, v86_bounce_off() + B_PATH, 0,
             v86_bounce_off() + B_PATH2);
    return !dos_err();
}

static int copy_tree(const char *src, const char *dst, unsigned char attr)
{
    FindDta saved;
    FindDta dta;
    char spec[84];
    char child_src[80];
    char child_dst[80];

    if (!(attr & ATTR_DIR)) {
        return copy_file(src, dst);
    }
    mkdir_path(dst);
    filespec(spec, src);
    set_dta();
    bounce_str(B_PATH, spec);
    dos_call(0x4E00, 0, 0x17, v86_bounce_off() + B_PATH, 0, 0);
    if (dos_err()) {
        return 1;
    }
    for (;;) {
        bounce_get(B_DTA, &dta, sizeof(dta));
        if (!(dta.name[0] == '.' && (dta.name[1] == 0 ||
                                     (dta.name[1] == '.' && dta.name[2] == 0)))) {
            path_join(child_src, src, dta.name);
            path_join(child_dst, dst, dta.name);
            bounce_get(B_DTA, &saved, sizeof(saved));
            if (!copy_tree(child_src, child_dst, dta.attr)) {
                return 0;
            }
            bounce_mem(B_DTA, &saved, sizeof(saved));
            set_dta();
        }
        dos_call(0x4F00, 0, 0, 0, 0, 0);
        if (dos_err()) {
            break;
        }
    }
    return 1;
}

static int del_tree(const char *path, unsigned char attr)
{
    FindDta saved;
    FindDta dta;
    char spec[84];
    char child[80];

    if (!(attr & ATTR_DIR)) {
        return del_file(path);
    }
    filespec(spec, path);
    set_dta();
    bounce_str(B_PATH, spec);
    dos_call(0x4E00, 0, 0x17, v86_bounce_off() + B_PATH, 0, 0);
    if (!dos_err()) {
        for (;;) {
            bounce_get(B_DTA, &dta, sizeof(dta));
            if (!(dta.name[0] == '.' && (dta.name[1] == 0 ||
                                         (dta.name[1] == '.' && dta.name[2] == 0)))) {
                path_join(child, path, dta.name);
                bounce_get(B_DTA, &saved, sizeof(saved));
                del_tree(child, dta.attr);
                bounce_mem(B_DTA, &saved, sizeof(saved));
                set_dta();
            }
            dos_call(0x4F00, 0, 0, 0, 0, 0);
            if (dos_err()) {
                break;
            }
        }
    }
    return rmdir_path(path);
}

static void clip_from_current(int op)
{
    Pane *p = &panes[active];
    DirEntry *e = cur_ent(p);

    if (!e || e->kind == KIND_UP) {
        return;
    }
    path_join(clip_path, p->path, e->name);
    clip_op = op;
}

static void do_paste(void)
{
    Pane *dstp;
    char dst[80];
    const char *slash;
    const char *name;
    DirEntry *src_kind;
    unsigned char attr;
    int i;

    if (clip_op == CLIP_NONE || clip_path[0] == 0) {
        return;
    }
    dstp = &panes[1 - active];
    if (d_readonly(dstp) && clip_op == CLIP_MOVE) {
        return;
    }
    slash = clip_path;
    name = clip_path;
    while (*slash) {
        if (*slash == '\\') {
            name = slash + 1;
        }
        slash++;
    }
    path_join(dst, dstp->path, name);
    attr = ATTR_DIR;
    src_kind = 0;
    for (i = 0; i < 2; i++) {
        int k;
        for (k = 0; k < panes[i].n; k++) {
            char full[80];
            path_join(full, panes[i].path, panes[i].ents[k].name);
            if (ci_eq(full, clip_path)) {
                src_kind = &panes[i].ents[k];
                break;
            }
        }
        if (src_kind) {
            break;
        }
    }
    attr = src_kind ? src_kind->attr : 0;
    if (clip_op == CLIP_MOVE && clip_path[0] == dst[0]) {
        if (rename_path(clip_path, dst)) {
            clip_op = CLIP_NONE;
            rescan_matching();
            return;
        }
    }
    if (copy_tree(clip_path, dst, attr)) {
        if (clip_op == CLIP_MOVE) {
            del_tree(clip_path, attr);
            clip_op = CLIP_NONE;
        }
        rescan_matching();
    }
}

static void do_delete(void)
{
    Pane *p = &panes[active];
    DirEntry *e = cur_ent(p);
    char full[80];

    if (!e || e->kind == KIND_UP || d_readonly(p)) {
        modal = MOD_NONE;
        return;
    }
    path_join(full, p->path, e->name);
    del_tree(full, e->attr);
    modal = MOD_NONE;
    rescan_matching();
}

static void do_mkdir(void)
{
    Pane *p = &panes[active];
    char full[80];

    if (prompt[0] == 0 || d_readonly(p)) {
        modal = MOD_NONE;
        return;
    }
    path_join(full, p->path, prompt);
    mkdir_path(full);
    modal = MOD_NONE;
    prompt[0] = 0;
    prompt_len = 0;
    rescan_matching();
}

static void start_delete(void)
{
    Pane *p = &panes[active];
    DirEntry *e = cur_ent(p);
    unsigned i;

    if (!e || e->kind == KIND_UP || d_readonly(p)) {
        return;
    }
    i = 0;
    while (e->name[i] && i < 12u) {
        prompt[i] = e->name[i];
        i++;
    }
    prompt[i] = 0;
    modal = MOD_DEL;
}

static void start_mkdir(void)
{
    if (d_readonly(&panes[active])) {
        return;
    }
    prompt[0] = 0;
    prompt_len = 0;
    modal = MOD_MKDIR;
}

static void open_edit(void)
{
    Pane *p = &panes[active];
    DirEntry *e = cur_ent(p);
    char full[80];

    if (!e || e->kind != KIND_FILE || d_readonly(p) || !p->ready) {
        return;
    }
    path_join(full, p->path, e->name);
    if (editor_open(full)) {
        desk_set_tab(DESK_TAB_EDIT);
    }
}

static void modal_key(unsigned ax)
{
    unsigned char ah = (unsigned char)(ax >> 8);
    unsigned char al = (unsigned char)ax;

    if (ah == 0x01) {
        modal = MOD_NONE;
        return;
    }
    if (ah == 0x1C) {
        if (modal == MOD_DEL) {
            do_delete();
        } else {
            do_mkdir();
        }
        return;
    }
    if (modal == MOD_MKDIR) {
        if (ah == 0x0E && prompt_len) {
            prompt[--prompt_len] = 0;
        } else if (al >= 32 && al < 127 && prompt_len < 12) {
            prompt[prompt_len++] = (char)al;
            prompt[prompt_len] = 0;
        }
    }
}

void dir_key(unsigned ax)
{
    Pane *p = &panes[active];
    unsigned char ah = (unsigned char)(ax >> 8);

    if (modal) {
        modal_key(ax);
        return;
    }
    if (ah == 0x0F) {
        active = 1 - active;
    } else if (ah == 0x48) {
        if (p->cursor > 0) {
            p->cursor--;
            if (p->cursor < p->scroll) {
                p->scroll = p->cursor;
            }
        }
    } else if (ah == 0x50) {
        if (p->cursor + 1 < p->n) {
            p->cursor++;
            if (p->cursor >= p->scroll + DIR_ROWS) {
                p->scroll = p->cursor - DIR_ROWS + 1;
            }
        }
    } else if (ah == 0x1C) {
        enter_row(p);
    } else if (ah == 0x4B) {
        if (p->drive == 2) {
            set_drive(p, 0);
        } else if (p->drive == 3) {
            set_drive(p, 2);
        }
    } else if (ah == 0x4D) {
        if (p->drive == 0) {
            set_drive(p, 2);
        } else if (p->drive == 2) {
            set_drive(p, 3);
        }
    } else if (ah == 0x3F) {
        clip_from_current(CLIP_COPY);
    } else if (ah == 0x40) {
        if (!d_readonly(p)) {
            clip_from_current(CLIP_MOVE);
        }
    } else if (ah == 0x41) {
        start_mkdir();
    } else if (ah == 0x42) {
        start_delete();
    } else if (ah == 0x3E) {
        open_edit();
    }
}

void dir_click(int x, int y)
{
    int which;
    int local_x;
    Pane *p;
    int row;
    int b;

    if (x < RAIL_W || y < TAB_Y || y >= CONS_Y) {
        return;
    }
    which = (x >= RAIL_W + PANE_W) ? 1 : 0;
    p = &panes[which];
    local_x = x - pane_x(which);
    active = which;
    if (y < LIST_Y) {
        int dx = 8;
        unsigned char d;
        for (d = 0; d < 4; d++) {
            if (d == 1) {
                continue;
            }
            if (local_x >= dx && local_x < dx + 28) {
                set_drive(p, d);
                return;
            }
            dx += 36;
        }
        return;
    }
    if (y < BTN_Y) {
        row = (y - (LIST_Y + 6)) / 16;
        if (row >= 0 && row < DIR_ROWS) {
            int idx = p->scroll + row;
            if (idx < p->n) {
                if (which == last_click_pane && idx == last_click_row &&
                    idx == p->cursor) {
                    if (p->ents[idx].kind == KIND_FILE) {
                        open_edit();
                    } else {
                        enter_row(p);
                    }
                    last_click_row = -1;
                } else {
                    p->cursor = idx;
                    last_click_pane = which;
                    last_click_row = idx;
                }
            }
        }
        return;
    }
    b = (local_x - 4) / 63;
    if (b == 0) {
        start_mkdir();
    } else if (b == 1) {
        clip_from_current(CLIP_COPY);
    } else if (b == 2) {
        if (!d_readonly(p)) {
            clip_from_current(CLIP_MOVE);
        }
    } else if (b == 3) {
        do_paste();
    } else if (b == 4) {
        start_delete();
    } else if (b == 6) {
        open_edit();
    }
}
