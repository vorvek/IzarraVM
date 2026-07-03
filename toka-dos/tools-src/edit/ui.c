/* TokaEdit menu bar and pulldown engine implementation. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <string.h>
#include <ctype.h>
#include "tui.h"
#include "ui.h"

#define AT_BAR  0x70
#define AT_HOT  0x7F
#define AT_MSEL 0x07
#define AT_DIS  0x78

#define NMENUS 4

static const MenuItem file_items[] = {
    { "&New",       NULL,          MI_NEW },
    { "&Open...",   NULL,          MI_OPEN },
    { "&Save",      NULL,          MI_SAVE },
    { "Save &As...",NULL,          MI_SAVEAS },
    { NULL,         NULL,          0 },
    { "E&xit",      NULL,          MI_EXIT }
};
static const MenuItem edit_items[] = {
    { "Cu&t",   "Shift+Del", MI_CUT },
    { "&Copy",  "Ctrl+Ins",  MI_COPY },
    { "&Paste", "Shift+Ins", MI_PASTE },
    { "C&lear", "Del",       MI_CLEAR }
};
static const MenuItem search_items[] = {
    { "&Find...",             NULL, MI_FIND },
    { "&Repeat Last Find",    "F3", MI_FINDNEXT },
    { "&Change...",           NULL, MI_CHANGE }
};
static const MenuItem help_items[] = {
    { "&About...", NULL, MI_ABOUT }
};

static const Menu menus[NMENUS] = {
    { "&File",   file_items,   sizeof(file_items) / sizeof(file_items[0]) },
    { "&Edit",   edit_items,   sizeof(edit_items) / sizeof(edit_items[0]) },
    { "&Search", search_items, sizeof(search_items) / sizeof(search_items[0]) },
    { "&Help",   help_items,   sizeof(help_items) / sizeof(help_items[0]) }
};

/* bar layout: computed once, col of each title's cell start (the leading
 * space before the title text) and its total cell width (2 spaces pad +
 * title text length without '&') */
static int bar_start[NMENUS];
static int bar_width[NMENUS];
static int bar_ready = 0;

static int title_len_nohot(const char *title) {
    int n = 0;
    while (*title) {
        if (*title != '&')
            n++;
        title++;
    }
    return n;
}

static void bar_layout(void) {
    int col = 1;
    int i;
    if (bar_ready)
        return;
    for (i = 0; i < NMENUS; i++) {
        bar_start[i] = col;
        bar_width[i] = title_len_nohot(menus[i].title) + 4; /* "  Title  " */
        col += bar_width[i];
    }
    bar_ready = 1;
}

/* draw a title (with its leading/trailing pad spaces) at its bar cell,
 * hotkey letter in AT_HOT unless sel forces the whole cell to one attr */
static void draw_title(int idx, int sel) {
    const char *t = menus[idx].title;
    int col = bar_start[idx];
    unsigned char base = sel ? AT_MSEL : AT_BAR;

    scr_putc(0, col, ' ', base);
    col++;
    while (*t) {
        unsigned char a = base;
        if (*t == '&') {
            t++;
            if (!*t)
                break;
            if (!sel)
                a = AT_HOT;
        }
        scr_putc(0, col, *t, a);
        col++;
        t++;
    }
    scr_putc(0, col, ' ', base);
    scr_putc(0, col + 1, ' ', base);
}

void menu_draw_bar(void) {
    int i;
    bar_layout();
    scr_fill(0, 0, 80, 1, ' ', AT_BAR);
    for (i = 0; i < NMENUS; i++)
        draw_title(i, 0);
}

int menu_hit(int col) {
    int i;
    bar_layout();
    for (i = 0; i < NMENUS; i++) {
        if (col >= bar_start[i] && col < bar_start[i] + bar_width[i])
            return i;
    }
    return -1;
}

/* ---- pulldown geometry ---- */

static int is_separator(const MenuItem *it) {
    return it->id == 0;
}

static int item_label_len(const MenuItem *it) {
    return title_len_nohot(it->label);
}

static int pd_width(const Menu *m) {
    int i, w = 0;
    for (i = 0; i < m->nitems; i++) {
        const MenuItem *it = &m->items[i];
        int len;
        if (is_separator(it))
            continue; /* separator doesn't drive width */
        len = item_label_len(it);
        if (it->rlabel)
            len += 2 + (int)strlen(it->rlabel);
        if (len > w)
            w = len;
    }
    return w + 4; /* 2 borders + 1 leading space + 1 trailing space */
}

static int pd_col(int idx, int width) {
    int col = bar_start[idx] - 1;
    if (col < 0)
        col = 0;
    if (col + width + 1 > 80)
        col = 80 - (width + 1);
    if (col < 0)
        col = 0;
    return col;
}

/* ---- pulldown drawing ---- */

static void draw_border(int row, int col, int w, int h) {
    int i;
    scr_putc(row, col, (char)0xDA, AT_BAR);
    for (i = 1; i < w - 1; i++)
        scr_putc(row, col + i, (char)0xC4, AT_BAR);
    scr_putc(row, col + w - 1, (char)0xBF, AT_BAR);
    for (i = 1; i < h - 1; i++) {
        scr_putc(row + i, col, (char)0xB3, AT_BAR);
        scr_putc(row + i, col + w - 1, (char)0xB3, AT_BAR);
    }
    scr_putc(row + h - 1, col, (char)0xC0, AT_BAR);
    for (i = 1; i < w - 1; i++)
        scr_putc(row + h - 1, col + i, (char)0xC4, AT_BAR);
    scr_putc(row + h - 1, col + w - 1, (char)0xD9, AT_BAR);
}

static void draw_separator(int row, int col, int w) {
    int i;
    scr_putc(row, col, (char)0xC3, AT_BAR);
    for (i = 1; i < w - 1; i++)
        scr_putc(row, col + i, (char)0xC4, AT_BAR);
    scr_putc(row, col + w - 1, (char)0xB4, AT_BAR);
}

static void draw_item(int row, int col, int w, const Menu *m, int i,
                       int sel, int (*enabled)(int id)) {
    const MenuItem *it = &m->items[i];
    unsigned char base;
    int en;
    const char *lp;
    int c;
    int rlen;

    if (is_separator(it)) {
        draw_separator(row, col, w);
        return;
    }

    en = enabled ? enabled(it->id) : 1;
    base = sel ? AT_MSEL : (en ? AT_BAR : AT_DIS);

    scr_putc(row, col, (char)0xB3, AT_BAR);
    scr_putc(row, col + w - 1, (char)0xB3, AT_BAR);
    scr_fill(row, col + 1, w - 2, 1, ' ', base);

    c = col + 2;
    lp = it->label;
    while (*lp) {
        unsigned char a = base;
        if (*lp == '&') {
            lp++;
            if (!*lp)
                break;
            if (!sel && en)
                a = AT_HOT;
        }
        scr_putc(row, c, *lp, a);
        c++;
        lp++;
    }

    if (it->rlabel) {
        rlen = (int)strlen(it->rlabel);
        scr_put(row, col + w - 1 - rlen, it->rlabel, base);
    }
}

static void draw_pulldown(int idx, int cur, int (*enabled)(int id)) {
    const Menu *m = &menus[idx];
    int w = pd_width(m);
    int h = m->nitems + 2;
    int col = pd_col(idx, w);
    int row = 1;
    int i;

    draw_border(row, col, w, h);
    for (i = 0; i < m->nitems; i++)
        draw_item(row + 1 + i, col, w, m, i, i == cur, enabled);
}

static int first_enabled(const Menu *m, int (*enabled)(int id)) {
    int i;
    for (i = 0; i < m->nitems; i++) {
        const MenuItem *it = &m->items[i];
        if (is_separator(it))
            continue;
        if (!enabled || enabled(it->id))
            return i;
    }
    return -1;
}

static int next_enabled(const Menu *m, int cur, int dir, int (*enabled)(int id)) {
    int i, n = m->nitems;
    if (cur < 0)
        return first_enabled(m, enabled);
    i = cur;
    for (;;) {
        i = (i + dir + n) % n;
        if (i == cur)
            return cur;
        if (is_separator(&m->items[i]))
            continue;
        if (!enabled || enabled(m->items[i].id))
            return i;
    }
}

static int hotkey_match(const char *label, int ch) {
    while (*label) {
        if (*label == '&') {
            label++;
            if (!*label)
                return 0;
            return tolower((unsigned char)*label) == tolower((unsigned char)ch);
        }
        label++;
    }
    return 0;
}

static int find_by_hotkey(const Menu *m, int ch, int (*enabled)(int id)) {
    int i;
    for (i = 0; i < m->nitems; i++) {
        const MenuItem *it = &m->items[i];
        if (is_separator(it))
            continue;
        if (hotkey_match(it->label, ch)) {
            if (!enabled || enabled(it->id))
                return i;
            return -1; /* disabled: matched but not selectable */
        }
    }
    return -1;
}

static int find_title_hotkey(int ch) {
    int i;
    for (i = 0; i < NMENUS; i++) {
        if (hotkey_match(menus[i].title, ch))
            return i;
    }
    return -1;
}

int menu_run(int open_menu, int (*enabled)(int id)) {
    int active;
    int pd_open;
    int cur;
    int have_mouse = mouse_present();
    /* scratch big enough for any pulldown rect: widest item text here is
     * "&Repeat Last Find" + "F3" rlabel (~24 cols), tallest is the File
     * menu at 6 items + 2 borders = 8 rows; 32x10 cells * 2 bytes/cell
     * leaves comfortable headroom without needing a runtime malloc. */
    char saved[32 * 10 * 2];
    int saved_valid = 0;
    int sv_row = 0, sv_col = 0, sv_w = 0, sv_h = 0;
    Event e;
    int result = -1; /* -1 = keep looping */

    bar_layout();

    active = (open_menu >= 0) ? open_menu : 0;
    pd_open = (open_menu >= 0);
    cur = pd_open ? first_enabled(&menus[active], enabled) : -1;

    for (;;) {
        /* repaint bar with active title highlighted (armed or open) */
        {
            int i;
            scr_fill(0, 0, 80, 1, ' ', AT_BAR);
            for (i = 0; i < NMENUS; i++)
                draw_title(i, i == active);
        }

        if (pd_open) {
            const Menu *m = &menus[active];
            int w = pd_width(m);
            int h = m->nitems + 2;
            int col = pd_col(active, w);

            if (have_mouse)
                mouse_hide();
            if (!saved_valid) {
                sv_row = 1; sv_col = col; sv_w = w; sv_h = h;
                if ((long)sv_w * sv_h * 2 <= (long)sizeof(saved)) {
                    scr_save_rect(sv_row, sv_col, sv_w, sv_h, saved);
                    saved_valid = 1;
                }
            }
            draw_pulldown(active, cur, enabled);
            if (have_mouse)
                mouse_show();
        }

        ev_wait(&e);

        if (e.kind == EV_KEY) {
            unsigned char scan = e.scan;
            unsigned char ascii = e.ascii;

            if (ascii == 27) { /* Esc: always closes, returns 0 */
                result = 0;
                break;
            }

            if (scan == 0x4B || scan == 0x4D) { /* Left/Right */
                int dir = (scan == 0x4B) ? -1 : 1;
                active = (active + dir + NMENUS) % NMENUS;
                if (pd_open) {
                    if (saved_valid) {
                        if (have_mouse) mouse_hide();
                        scr_restore_rect(sv_row, sv_col, sv_w, sv_h, saved);
                        if (have_mouse) mouse_show();
                        saved_valid = 0;
                    }
                    cur = first_enabled(&menus[active], enabled);
                }
                continue;
            }

            if (!pd_open) {
                /* armed bar mode */
                if (scan == 0x50 || ascii == 13) { /* Down or Enter opens */
                    pd_open = 1;
                    cur = first_enabled(&menus[active], enabled);
                    continue;
                }
                if (ascii > 0) {
                    int hit = find_title_hotkey((int)ascii);
                    if (hit >= 0) {
                        active = hit;
                        pd_open = 1;
                        cur = first_enabled(&menus[active], enabled);
                        continue;
                    }
                }
                continue;
            }

            /* pulldown open */
            if (scan == 0x50) { /* Down */
                cur = next_enabled(&menus[active], cur, 1, enabled);
                continue;
            }
            if (scan == 0x48) { /* Up */
                cur = next_enabled(&menus[active], cur, -1, enabled);
                continue;
            }
            if (ascii == 13) { /* Enter */
                if (cur >= 0) {
                    result = menus[active].items[cur].id;
                    break;
                }
                continue;
            }
            if (ascii > 0) {
                int hit = find_by_hotkey(&menus[active], (int)ascii, enabled);
                if (hit >= 0) {
                    result = menus[active].items[hit].id;
                    break;
                }
                continue;
            }
            continue;
        }

        if (e.kind == EV_MOUSE_DOWN || e.kind == EV_MOUSE_DRAG) {
            if (e.mrow == 0) {
                int hit = menu_hit(e.mcol);
                if (e.kind == EV_MOUSE_DOWN && hit < 0) {
                    result = 0;
                    break;
                }
                if (hit >= 0 && hit != active) {
                    if (pd_open && saved_valid) {
                        if (have_mouse) mouse_hide();
                        scr_restore_rect(sv_row, sv_col, sv_w, sv_h, saved);
                        if (have_mouse) mouse_show();
                        saved_valid = 0;
                    }
                    active = hit;
                    if (pd_open)
                        cur = first_enabled(&menus[active], enabled);
                }
                if (e.kind == EV_MOUSE_DOWN && !pd_open) {
                    pd_open = 1;
                    cur = first_enabled(&menus[active], enabled);
                }
                continue;
            }
            if (pd_open) {
                const Menu *m = &menus[active];
                int w = pd_width(m);
                int col = pd_col(active, w);
                int row0 = 2; /* first item row (border at row 1) */
                if (e.mcol >= col && e.mcol < col + w &&
                    e.mrow >= row0 && e.mrow < row0 + m->nitems) {
                    int idx = e.mrow - row0;
                    const MenuItem *it = &m->items[idx];
                    if (e.kind == EV_MOUSE_DOWN && !is_separator(it) &&
                        (!enabled || enabled(it->id))) {
                        result = it->id;
                        break;
                    }
                    continue;
                }
                if (e.kind == EV_MOUSE_DOWN) {
                    result = 0;
                    break;
                }
            } else if (e.kind == EV_MOUSE_DOWN) {
                result = 0;
                break;
            }
            continue;
        }

        /* EV_MOUSE_UP, EV_ALT_TAP while inside the menu: ignore */
    }

    if (saved_valid) {
        if (have_mouse) mouse_hide();
        scr_restore_rect(sv_row, sv_col, sv_w, sv_h, saved);
        if (have_mouse) mouse_show();
    }
    menu_draw_bar();
    return result;
}
