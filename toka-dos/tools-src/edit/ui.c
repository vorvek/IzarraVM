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

/* ---- modal dialog engine ---- */

#define AT_TEXT 0x07

#define DLG_MAXBTN   4
#define DLG_FIELD_W  40  /* preferred visible field width */
#define DLG_MAX_INTERIOR 74 /* clamp: box width = interior + 2 <= 76 */

static int str_line_len(const char *s) {
    const char *nl = strchr(s, '\n');
    return nl ? (int)(nl - s) : (int)strlen(s);
}

static void dlg_draw_border(int row, int col, int w, int h, const char *title) {
    draw_border(row, col, w, h);
    if (title && title[0]) {
        int tlen = (int)strlen(title);
        int tcol;
        char buf[3 + 40];
        if (tlen > 40)
            tlen = 40;
        buf[0] = ' ';
        memcpy(buf + 1, title, tlen);
        buf[1 + tlen] = ' ';
        buf[2 + tlen] = '\0';
        tcol = col + (w - (tlen + 2)) / 2;
        if (tcol < col + 1)
            tcol = col + 1;
        scr_put(row, tcol, buf, AT_BAR);
    }
}

/* button row layout: fills cols[] with each button's left col (label
 * starts 2 chars in from "< "), returns total row width */
static int dlg_btn_layout(const char **buttons, int nbuttons, int *cols, int *widths) {
    int i, total = 0;
    for (i = 0; i < nbuttons; i++) {
        widths[i] = (int)strlen(buttons[i]) + 4; /* "< X >" */
        total += widths[i];
        if (i > 0)
            total += 2; /* gap */
    }
    {
        int c = 0;
        for (i = 0; i < nbuttons; i++) {
            cols[i] = c;
            c += widths[i] + 2;
        }
    }
    return total;
}

static void dlg_draw_button(int row, int col, int w, const char *label, int focused) {
    unsigned char base = focused ? AT_MSEL : AT_BAR;
    int i;
    scr_fill(row, col, w, 1, ' ', base);
    scr_putc(row, col, '<', base);
    scr_putc(row, col + w - 1, '>', base);
    for (i = 0; label[i]; i++)
        scr_putc(row, col + 2 + i, label[i], base);
}

/* field: draws interior-width visible chars starting at hscroll, no cursor
 * (hardware cursor handled by caller) */
static void dlg_draw_field(int row, int col, int w, const char *text, int hscroll) {
    int len = (int)strlen(text);
    int i;
    scr_fill(row, col, w, 1, ' ', AT_TEXT);
    for (i = 0; i < w; i++) {
        int sc = hscroll + i;
        if (sc < len)
            scr_putc(row, col + i, text[sc], AT_TEXT);
    }
}

int dlg_run(const char *title, const char *prompt,
            char *field, int fieldcap,
            const char **buttons, int nbuttons) {
    int have_mouse = mouse_present();
    int p1len = 0, p2len = 0;
    const char *p2 = NULL;
    int nplines = 0;
    int interior, boxw, boxh;
    int row0, col0;
    int field_w = 0, field_row = 0, field_col = 0;
    int btn_row = 0;
    int btn_cols[DLG_MAXBTN], btn_widths[DLG_MAXBTN];
    int btn_total = 0, btn_start = 0;
    /* box is at most 76 wide x ~9 tall: 76*9*2 = 1368 bytes */
    static char saved[76 * 9 * 2];
    int saved_valid = 0;
    int flen;
    int hscroll = 0, curpos = 0;
    int nfocus, focus; /* nfocus = nbuttons (+1 if field); focus 0 = field
                         * when present, else focus indexes buttons */
    int has_field = (field != NULL);
    Event e;
    int result = -2; /* -2 = keep looping */

    if (nbuttons > DLG_MAXBTN)
        nbuttons = DLG_MAXBTN;

    if (prompt) {
        p1len = str_line_len(prompt);
        nplines = 1;
        if (prompt[p1len] == '\n') {
            p2 = prompt + p1len + 1;
            p2len = (int)strlen(p2);
            nplines = 2;
        }
    }

    interior = (int)strlen(title) + 2;
    if (p1len > interior) interior = p1len;
    if (p2len > interior) interior = p2len;
    if (has_field && 40 > interior) interior = 40;
    btn_total = dlg_btn_layout(buttons, nbuttons, btn_cols, btn_widths);
    if (btn_total > interior)
        interior = btn_total;
    interior += 2; /* left/right padding inside the borders */
    if (interior > DLG_MAX_INTERIOR)
        interior = DLG_MAX_INTERIOR;

    boxw = interior + 2;
    boxh = 2 /* borders */ + nplines + (has_field ? 1 : 0) + 1 /* blank */ + 1 /* button row */;

    row0 = (25 - boxh) / 2;
    if (row0 < 0) row0 = 0;
    col0 = (80 - boxw) / 2;
    if (col0 < 0) col0 = 0;

    field_w = interior - 2;
    if (field_w > DLG_FIELD_W)
        field_w = DLG_FIELD_W;
    if (field_w < 1)
        field_w = 1;
    field_col = col0 + 1 + (interior - field_w) / 2;
    field_row = row0 + 1 + nplines;

    btn_row = row0 + boxh - 2;
    btn_start = col0 + 1 + (interior - btn_total) / 2;

    nfocus = nbuttons + (has_field ? 1 : 0);
    focus = has_field ? 0 : 0; /* field first if present, else button 0 */

    if (has_field) {
        flen = (int)strlen(field);
        curpos = flen;
        if (curpos > fieldcap - 1)
            curpos = fieldcap - 1;
        if (curpos >= field_w)
            hscroll = curpos - field_w + 1;
    }

    if (have_mouse)
        mouse_hide();
    if ((long)boxw * boxh * 2 <= (long)sizeof(saved)) {
        scr_save_rect(row0, col0, boxw, boxh, saved);
        saved_valid = 1;
    }

    for (;;) {
        int i;

        dlg_draw_border(row0, col0, boxw, boxh, title);
        scr_fill(row0 + 1, col0 + 1, interior, boxh - 2, ' ', AT_BAR);

        if (nplines >= 1) {
            /* Only the first line: prompt runs to NUL through any '\n', so
             * bound it to p1len (scr_put stops at NUL, not at '\n'). */
            char line1[80];
            int n = p1len;
            if (n > 78) n = 78;
            memcpy(line1, prompt, n);
            line1[n] = '\0';
            scr_put(row0 + 1, col0 + 1, line1, AT_BAR);
        }
        if (nplines == 2) {
            char line2[80];
            int n = p2len;
            if (n > 78) n = 78;
            memcpy(line2, p2, n);
            line2[n] = '\0';
            scr_put(row0 + 2, col0 + 1, line2, AT_BAR);
        }

        if (has_field)
            dlg_draw_field(field_row, field_col, field_w, field, hscroll);

        for (i = 0; i < nbuttons; i++) {
            int focused = has_field ? (focus == i + 1) : (focus == i);
            dlg_draw_button(btn_row, btn_start + btn_cols[i], btn_widths[i],
                             buttons[i], focused);
        }

        if (has_field && focus == 0) {
            scr_cursor(field_row, field_col + (curpos - hscroll));
        } else {
            scr_cursor_hide();
        }

        if (have_mouse)
            mouse_show();

        ev_wait(&e);

        if (have_mouse)
            mouse_hide();

        if (e.kind == EV_KEY) {
            unsigned char scan = e.scan;
            unsigned char ascii = e.ascii;
            int shift = e.mods & 1;

            if (ascii == 27) {
                result = -1;
                break;
            }

            /* Tab / Shift+Tab: cycle focus. Some INT16h layers deliver
             * Shift+Tab as scan 0x0F ascii 0 rather than ascii 9 + shift
             * mod, so accept either form. */
            if (ascii == 9 || (scan == 0x0F && ascii == 0)) {
                int back = (ascii == 9) ? shift : 1;
                if (back) {
                    focus = (focus - 1 + nfocus) % nfocus;
                } else {
                    focus = (focus + 1) % nfocus;
                }
                if (has_field && focus == 0) {
                    curpos = (int)strlen(field);
                    if (curpos > fieldcap - 1) curpos = fieldcap - 1;
                    hscroll = (curpos >= field_w) ? curpos - field_w + 1 : 0;
                }
                if (have_mouse) mouse_show();
                continue;
            }

            if (has_field && focus == 0) {
                /* field editing */
                if (scan == 0x4B) { /* Left */
                    if (curpos > 0) curpos--;
                    if (curpos < hscroll) hscroll = curpos;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (scan == 0x4D) { /* Right */
                    flen = (int)strlen(field);
                    if (curpos < flen) curpos++;
                    if (curpos - hscroll >= field_w) hscroll = curpos - field_w + 1;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (scan == 0x47) { /* Home */
                    curpos = 0;
                    hscroll = 0;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (scan == 0x4F) { /* End */
                    curpos = (int)strlen(field);
                    if (curpos > fieldcap - 1) curpos = fieldcap - 1;
                    hscroll = (curpos >= field_w) ? curpos - field_w + 1 : 0;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (ascii == 8) { /* Backspace */
                    flen = (int)strlen(field);
                    if (curpos > 0) {
                        memmove(field + curpos - 1, field + curpos, flen - curpos + 1);
                        curpos--;
                        if (curpos < hscroll) hscroll = curpos;
                    }
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (scan == 0x53) { /* Del */
                    flen = (int)strlen(field);
                    if (curpos < flen)
                        memmove(field + curpos, field + curpos + 1, flen - curpos);
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (ascii == 13) { /* Enter: press button 0 */
                    result = 0;
                    break;
                }
                if (ascii >= 32 && ascii < 127) {
                    flen = (int)strlen(field);
                    if (flen < fieldcap - 1) {
                        memmove(field + curpos + 1, field + curpos, flen - curpos + 1);
                        field[curpos] = (char)ascii;
                        curpos++;
                        if (curpos - hscroll >= field_w) hscroll = curpos - field_w + 1;
                    }
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (have_mouse) mouse_show();
                continue;
            }

            /* a button has focus */
            {
                int bidx = has_field ? focus - 1 : focus;
                if (scan == 0x4B) { /* Left */
                    bidx = (bidx - 1 + nbuttons) % nbuttons;
                    focus = has_field ? bidx + 1 : bidx;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (scan == 0x4D) { /* Right */
                    bidx = (bidx + 1) % nbuttons;
                    focus = has_field ? bidx + 1 : bidx;
                    if (have_mouse) mouse_show();
                    continue;
                }
                if (ascii == 13) {
                    result = bidx;
                    break;
                }
            }
            if (have_mouse) mouse_show();
            continue;
        }

        if (e.kind == EV_MOUSE_DOWN) {
            int i;
            /* click inside a button? */
            int hit = -1;
            for (i = 0; i < nbuttons; i++) {
                int bc = btn_start + btn_cols[i];
                if (e.mrow == btn_row && e.mcol >= bc && e.mcol < bc + btn_widths[i]) {
                    hit = i;
                    break;
                }
            }
            if (hit >= 0) {
                result = hit;
                break;
            }
            if (has_field && e.mrow == field_row &&
                e.mcol >= field_col && e.mcol < field_col + field_w) {
                int click = hscroll + (e.mcol - field_col);
                flen = (int)strlen(field);
                if (click > flen) click = flen;
                if (click < 0) click = 0;
                curpos = click;
                focus = 0;
                if (have_mouse) mouse_show();
                continue;
            }
            /* inside the box but not on a control: ignore. outside the
             * box: also ignore -- modal dialogs do not dismiss on
             * click-away. */
            if (have_mouse) mouse_show();
            continue;
        }

        /* EV_MOUSE_DRAG, EV_MOUSE_UP, EV_ALT_TAP while modal: ignore */
        if (have_mouse) mouse_show();
    }

    if (saved_valid) {
        if (have_mouse) mouse_hide();
        scr_restore_rect(row0, col0, boxw, boxh, saved);
        if (have_mouse) mouse_show();
    }
    scr_cursor_hide();

    return result;
}

void dlg_msg(const char *title, const char *text) {
    static const char *ok_btn[1] = { "OK" };
    dlg_run(title, text, NULL, 0, ok_btn, 1);
}

int dlg_yesnocancel(const char *title, const char *text) {
    static const char *ync_btn[3] = { "Yes", "No", "Cancel" };
    int r = dlg_run(title, text, NULL, 0, ync_btn, 3);
    return (r < 0) ? 2 : r;
}
