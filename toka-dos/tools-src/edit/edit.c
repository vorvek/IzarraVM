/* TokaEdit main editing loop. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include "buffer.h"
#include "tui.h"
#include "ui.h"

#define AT_TEXT 0x07
#define AT_SEL  0x70
#define AT_BAR  0x70
#define AT_HOT  0x7F
#define AT_MSEL 0x07
#define AT_DIS  0x78

#define SB_TRACK 0xB0
#define SB_THUMB 0xDB
#define SB_UP    0x18
#define SB_DOWN  0x19
#define SB_LEFT  0x1B
#define SB_RIGHT 0x1A

#define TEXT_TOP    2
#define TEXT_BOTTOM 22
#define TEXT_ROWS   21
#define TEXT_COLS   79 /* visible cols 0..78 */

/* ---- state ---- */
static Buf doc;
static char docname[80];        /* "" = Untitled */
static int  cur_row, cur_col;   /* cursor in the document */
static int  top_row, left_col;  /* scroll origin */
static int  sel_active;         /* selection anchor valid */
static int  anch_row, anch_col; /* selection anchor */
static char *clipboard;         /* malloc'd CRLF text or NULL */
static char last_find[64];      /* remembered across Find and F3/repeat */
static int  overwrite;          /* Ins toggle */
static int  quit;

static int imax(int a, int b) { return a > b ? a : b; }

/* shared by redraw() and the scrollbar click handler so the thumb position
 * used for hit-testing can never drift from the one that gets painted */
static int vthumb_row(void) {
    int t = 3 + (int)((long)top_row * 18L / (long)imax(1, doc.nlines - 1));
    if (t < 3) t = 3;
    if (t > 21) t = 21;
    return t;
}

static int hthumb_col(void) {
    int t = 1 + (int)((long)left_col * 76L / 255L);
    if (t < 1) t = 1;
    if (t > 77) t = 77;
    return t;
}

/* ---- selection ---- */

/* normalize anchor/cursor into r; returns 1 if a real (non-empty) selection */
static int get_sel(Range *r) {
    int ar, ac, cr, cc;
    if (!sel_active)
        return 0;
    ar = anch_row; ac = anch_col;
    cr = cur_row;  cc = cur_col;
    if (ar == cr && ac == cc)
        return 0;
    if (ar < cr || (ar == cr && ac < cc)) {
        r->r1 = ar; r->c1 = ac; r->r2 = cr; r->c2 = cc;
    } else {
        r->r1 = cr; r->c1 = cc; r->r2 = ar; r->c2 = ac;
    }
    return 1;
}

static int in_sel(const Range *r, int row, int col) {
    if (row < r->r1 || row > r->r2)
        return 0;
    if (r->r1 == r->r2)
        return col >= r->c1 && col < r->c2;
    if (row == r->r1)
        return col >= r->c1;
    if (row == r->r2)
        return col < r->c2;
    return 1;
}

/* ---- rendering ---- */

static void redraw(void) {
    int have_mouse = mouse_present();
    int row, col, len, thumb;
    char line[TEXT_COLS + 1];
    Range sel;
    int has_sel = get_sel(&sel);
    char status_l[] = "F1=About  F3=Repeat Find";
    char status_r[32];
    char title[82];
    int tlen, tstart;
    const char *nm = docname[0] ? docname : "Untitled";

    if (have_mouse)
        mouse_hide();

    /* text area */
    for (row = 0; row < TEXT_ROWS; row++) {
        int doc_row = top_row + row;
        int i;
        if (doc_row < doc.nlines) {
            len = doc.lens[doc_row];
            for (i = 0; i < TEXT_COLS; i++) {
                int dc = left_col + i;
                line[i] = (dc < len) ? doc.lines[doc_row][dc] : ' ';
            }
        } else {
            for (i = 0; i < TEXT_COLS; i++)
                line[i] = ' ';
        }
        line[TEXT_COLS] = '\0';
        scr_put(TEXT_TOP + row, 0, line, AT_TEXT);
        if (has_sel && doc_row < doc.nlines) {
            for (i = 0; i < TEXT_COLS; i++) {
                int dc = left_col + i;
                if (in_sel(&sel, doc_row, dc))
                    scr_putc(TEXT_TOP + row, i, line[i], AT_SEL);
            }
        }
    }

    /* menu bar row 0 */
    menu_draw_bar();

    /* title bar row 1 */
    scr_fill(1, 0, 80, 1, ' ', AT_BAR);
    tlen = (int)strlen(nm);
    if (tlen > 78)
        tlen = 78;
    title[0] = ' ';
    memcpy(title + 1, nm, tlen);
    title[1 + tlen] = ' ';
    title[2 + tlen] = '\0';
    tstart = (80 - (tlen + 2)) / 2;
    if (tstart < 0)
        tstart = 0;
    scr_put(1, tstart, title, AT_BAR);

    /* status row 24 */
    scr_fill(24, 0, 80, 1, ' ', AT_BAR);
    scr_put(24, 0, status_l, AT_BAR);
    sprintf(status_r, "Line:%d  Col:%d", cur_row + 1, cur_col + 1);
    {
        int rl = (int)strlen(status_r);
        int rstart = 79 - rl;
        if (rstart < 0)
            rstart = 0;
        scr_put(24, rstart, status_r, AT_BAR);
    }

    /* vertical scrollbar, col 79 */
    scr_putc(TEXT_TOP, 79, (char)SB_UP, AT_BAR);
    scr_putc(TEXT_BOTTOM, 79, (char)SB_DOWN, AT_BAR);
    for (row = TEXT_TOP + 1; row < TEXT_BOTTOM; row++)
        scr_putc(row, 79, (char)SB_TRACK, AT_BAR);
    thumb = vthumb_row();
    scr_putc(thumb, 79, (char)SB_THUMB, AT_BAR);

    /* horizontal scrollbar, row 23 */
    scr_putc(23, 0, (char)SB_LEFT, AT_BAR);
    scr_putc(23, 78, (char)SB_RIGHT, AT_BAR);
    for (col = 1; col < 78; col++)
        scr_putc(23, col, (char)SB_TRACK, AT_BAR);
    thumb = hthumb_col();
    scr_putc(23, thumb, (char)SB_THUMB, AT_BAR);

    scr_cursor(TEXT_TOP + cur_row - top_row, cur_col - left_col);

    if (have_mouse)
        mouse_show();
}

/* ---- clamping ---- */

static void clamp_cursor_col(void) {
    int len = doc.lens[cur_row];
    if (cur_col > len)
        cur_col = len;
    if (cur_col < 0)
        cur_col = 0;
}

static void clamp_scroll(void) {
    int screen_row = TEXT_TOP + cur_row - top_row;
    int screen_col = cur_col - left_col;

    if (screen_row < TEXT_TOP)
        top_row = cur_row;
    else if (screen_row > TEXT_BOTTOM)
        top_row = cur_row - (TEXT_ROWS - 1);

    if (screen_col < 0)
        left_col = cur_col;
    else if (screen_col > 78)
        left_col = cur_col - 78;

    if (top_row < 0)
        top_row = 0;
    if (left_col < 0)
        left_col = 0;
}

/* ---- editing helpers ---- */

static void begin_extend(void) {
    if (!sel_active) {
        anch_row = cur_row;
        anch_col = cur_col;
        sel_active = 1;
    }
}

static void delete_selection(void) {
    Range r;
    if (!get_sel(&r))
        return;
    if (buf_delete_range(&doc, &r)) {
        cur_row = r.r1;
        cur_col = r.c1;
    }
    sel_active = 0;
}

static int do_copy(void) {
    Range r;
    if (!get_sel(&r))
        return 0;
    free(clipboard);
    clipboard = buf_get_range(&doc, &r);
    if (!clipboard) {
        /* range over BUF_MAX_RANGE or oom */
        dlg_msg("Edit", "Selection too large to copy.");
        return 0;
    }
    return 1;
}

static void do_cut(void) {
    /* only delete what actually reached the clipboard */
    if (do_copy())
        delete_selection();
}

static void do_paste(void) {
    int er, ec;
    if (!clipboard)
        return;
    delete_selection();
    if (buf_insert_text(&doc, cur_row, cur_col, clipboard, &er, &ec)) {
        cur_row = er;
        cur_col = ec;
    }
}

/* previous/next word-start scan; alnum/non-alnum boundaries, crosses lines */
static void word_left(void) {
    int row = cur_row, col = cur_col;
    if (col == 0) {
        if (row == 0)
            return;
        row--;
        col = doc.lens[row];
        cur_row = row;
        cur_col = col;
        return;
    }
    col--;
    /* skip whitespace/non-word backwards */
    while (col > 0 && !(isalnum((unsigned char)doc.lines[row][col]) || doc.lines[row][col] == '_') )
        col--;
    /* skip word chars backwards to the start of the word */
    while (col > 0 && (isalnum((unsigned char)doc.lines[row][col - 1]) || doc.lines[row][col - 1] == '_'))
        col--;
    cur_row = row;
    cur_col = col;
}

static void word_right(void) {
    int row = cur_row, col = cur_col;
    int len = doc.lens[row];
    if (col >= len) {
        if (row >= doc.nlines - 1)
            return;
        cur_row = row + 1;
        cur_col = 0;
        return;
    }
    /* skip current word chars forward */
    while (col < len && (isalnum((unsigned char)doc.lines[row][col]) || doc.lines[row][col] == '_'))
        col++;
    /* skip non-word forward */
    while (col < len && !(isalnum((unsigned char)doc.lines[row][col]) || doc.lines[row][col] == '_'))
        col++;
    if (col >= len && row < doc.nlines - 1) {
        cur_row = row + 1;
        cur_col = 0;
    } else {
        cur_row = row;
        cur_col = col;
    }
}

/* ---- file plumbing ---- */

static int save_file(void); /* forward: save_as() calls back into it */

/* reset cursor/scroll/selection after loading a fresh buffer */
static void reset_view(void) {
    cur_row = cur_col = 0;
    top_row = left_col = 0;
    sel_active = 0;
}

static int load_file(const char *path) {
    FILE *f;
    int ok;

    if (path != docname) {
        strncpy(docname, path, sizeof(docname) - 1);
        docname[sizeof(docname) - 1] = '\0';
    }
    f = fopen(path, "rb");
    if (!f) {
        reset_view();
        return 1; /* new file: keep empty buffer */
    }

    ok = buf_load_stream(&doc, f);
    fclose(f);

    if (!ok)
        return 0;

    reset_view();
    return 1;
}

/* prompts for a new file name and, if given, saves under it; returns 1 on
 * a completed save, 0 on cancel/refusal (dlg_run already reported errors) */
static int save_as(void) {
    char field[64];
    static const char *btns[2] = { "OK", "Cancel" };
    int r;

    strncpy(field, docname, sizeof(field) - 1);
    field[sizeof(field) - 1] = '\0';

    r = dlg_run("Save As", "File Name:", field, sizeof(field), btns, 2);
    if (r != 0 || !field[0])
        return 0;

    strupr(field);
    strncpy(docname, field, sizeof(docname) - 1);
    docname[sizeof(docname) - 1] = '\0';
    return save_file();
}

static int save_file(void) {
    FILE *f;
    int ok;

    if (!docname[0])
        return save_as();

    f = fopen(docname, "wb");
    if (!f) {
        dlg_msg("Save", "Could not write file.");
        return 0;
    }
    ok = buf_save_stream(&doc, f);
    if (fclose(f) != 0) ok = 0;      /* buffered writes flush at fclose */
    if (!ok) {
        dlg_msg("Save", "Write error.");
        return 0;
    }
    doc.dirty = 0;
    return 1;
}

/* ---- menu action plumbing ---- */

/* shared by the F3 key and the Search|Repeat Last Find menu item */
static void repeat_find(void) {
    if (last_find[0]) {
        int fr, fc;
        if (buf_find(&doc, cur_row, cur_col, last_find, 1, &fr, &fc)) {
            int mlen = (int)strlen(last_find);
            anch_row = fr;
            anch_col = fc;
            cur_row = fr;
            cur_col = fc + mlen;
            sel_active = 1;
        } else {
            dlg_msg("Find", "Match not found");
        }
    }
}

/* returns 1 if it is OK to proceed (discard/replace the buffer), 0 if the
 * caller should abort the pending action (user chose Cancel) */
static int unsaved_check(void) {
    int r;
    if (!doc.dirty)
        return 1;
    r = dlg_yesnocancel("EDIT", "Loaded file is not saved. Save it now?");
    if (r == 0) /* Yes */
        return save_file();
    if (r == 1) /* No */
        return 1;
    return 0; /* Cancel */
}

static void do_open(void) {
    char field[64];
    static const char *btns[2] = { "OK", "Cancel" };
    int r;

    if (!unsaved_check())
        return;

    field[0] = '\0';
    r = dlg_run("Open", "File Name:", field, sizeof(field), btns, 2);
    if (r != 0 || !field[0])
        return;

    strupr(field);
    if (!load_file(field)) {
        dlg_msg("Open", "Cannot load file.");
        return;
    }
}

static void do_find(void) {
    char field[64];
    static const char *btns[2] = { "OK", "Cancel" };
    int r;

    strncpy(field, last_find, sizeof(field) - 1);
    field[sizeof(field) - 1] = '\0';

    r = dlg_run("Find", "Find What:", field, sizeof(field), btns, 2);
    if (r != 0 || !field[0])
        return;

    strncpy(last_find, field, sizeof(last_find) - 1);
    last_find[sizeof(last_find) - 1] = '\0';
    repeat_find();
}

static void do_change(void) {
    char findbuf[64];
    char replbuf[64];
    static const char *ok_btn[2] = { "OK", "Cancel" };
    static const char *chg_btn[4] = { "Change", "Skip", "Change All", "Cancel" };
    int r;
    int count = 0;
    int all = 0;
    int sr, sc;
    int fr, fc, mlen, rlen;
    int have_match;
    char msg[40];

    findbuf[0] = '\0';
    r = dlg_run("Change", "Find What:", findbuf, sizeof(findbuf), ok_btn, 2);
    if (r != 0 || !findbuf[0])
        return;

    replbuf[0] = '\0';
    r = dlg_run("Change", "Change To:", replbuf, sizeof(replbuf), ok_btn, 2);
    if (r != 0)
        return;

    /* Scan origin. buf_find itself searches from col+1, which would skip
     * a match starting exactly at the cursor -- the common "Ctrl+Home
     * then Change All" flow -- so probe that one position by hand. */
    sr = cur_row;
    sc = cur_col;
    mlen = (int)strlen(findbuf);
    rlen = (int)strlen(replbuf);
    have_match = 0;
    if (sr < doc.nlines && sc + mlen <= doc.lens[sr]) {
        int i;
        have_match = 1;
        for (i = 0; i < mlen; i++) {
            if (tolower((unsigned char)doc.lines[sr][sc + i]) !=
                tolower((unsigned char)findbuf[i])) {
                have_match = 0;
                break;
            }
        }
        if (have_match) {
            fr = sr;
            fc = sc;
        }
    }

    /*
     * Strictly-forward, no-wrap scan. buf_find always wraps to the top
     * when it runs off the end of the document, so we must detect a
     * wrapped result ourselves and stop instead of accepting it: a
     * wrapped match is positioned at or before the point we searched
     * from, i.e. (fr < sr) || (fr == sr && fc <= sc).
     *
     * Termination: each iteration either (a) advances (sr,sc) strictly
     * forward -- Skip and refusal both set sr/sc to just past the
     * matched needle -- or (b) performs a replacement, which either
     * grows the document (sr/sc still advances past the replacement,
     * strictly forward of fc) or shrinks/holds it (bounded below).
     * Since buf_find never returns a position at or before the search
     * origin without that being treated as "no more matches" (wrap
     * rejected below), the scan origin is always strictly increasing
     * within the bounded document, so the loop always terminates. No
     * wrap: Change covers cursor..end-of-document only, deliberately
     * (a user wanting the whole document should Ctrl+Home first).
     */
    for (;;) {
        int er, ec;

        if (!have_match) {
            if (!buf_find(&doc, sr, sc, findbuf, 1, &fr, &fc))
                break; /* no more matches at all */

            if (fr < sr || (fr == sr && fc <= sc))
                break; /* wrapped back to/before the scan origin: stop */
        }
        have_match = 0;

        anch_row = fr;
        anch_col = fc;
        cur_row = fr;
        cur_col = fc + mlen;
        sel_active = 1;

        if (!all) {
            redraw();
            r = dlg_run("Change", NULL, NULL, 0, chg_btn, 4);
            if (r < 0 || r == 3) /* Esc or Cancel */
                break;
            if (r == 1) { /* Skip */
                sr = fr;
                sc = fc + mlen - 1; /* buf_find resumes at sc+1 == fc+mlen */
                continue;
            }
            if (r == 2) /* Change All: fall through to replace, then stop asking */
                all = 1;
            /* r == 0: Change -- fall through to replace */
        }

        {
            Range rng;
            rng.r1 = fr; rng.c1 = fc;
            rng.r2 = fr; rng.c2 = fc + mlen;
            /* Validate the grown length BEFORE deleting: a delete that
             * succeeds followed by an insert that refuses would lose the
             * matched text with nothing put back. */
            if (doc.lens[fr] - mlen + rlen <= BUF_MAX_LINE &&
                buf_delete_range(&doc, &rng) &&
                buf_insert_text(&doc, fr, fc, replbuf, &er, &ec)) {
                count++;
                sr = er;
                sc = ec - 1; /* buf_find resumes at sc+1 == ec, just past
                              * the inserted replacement (ec==0 is safe:
                              * col+1==0 in buf_find scans from col 0) */
                cur_row = er;
                cur_col = ec;
                sel_active = 0;
            } else {
                /* refused (line-too-long etc): leave text as-is, advance
                 * past the unmodified match so we don't loop forever */
                sr = fr;
                sc = fc + mlen - 1;
            }
        }
    }

    sel_active = 0;
    sprintf(msg, "%d change(s) made", count);
    dlg_msg("Change", msg);
}

static int item_enabled(int id) {
    Range r;
    switch (id) {
    case MI_CUT:
    case MI_COPY:
    case MI_CLEAR:
        return get_sel(&r);
    case MI_PASTE:
        return clipboard != NULL;
    default:
        return 1;
    }
}

static void act(int id) {
    switch (id) {
    case 0:
        break;
    case MI_NEW:
        if (!unsaved_check())
            break;
        buf_free(&doc);
        buf_init(&doc);
        docname[0] = '\0';
        cur_row = cur_col = 0;
        top_row = left_col = 0;
        sel_active = 0;
        anch_row = anch_col = 0;
        overwrite = 0;
        break;
    case MI_OPEN:
        do_open();
        break;
    case MI_SAVE:
        save_file();
        break;
    case MI_SAVEAS:
        save_as();
        break;
    case MI_EXIT:
        if (!unsaved_check())
            break;
        quit = 1;
        break;
    case MI_CUT:
        do_cut();
        break;
    case MI_COPY:
        do_copy();
        break;
    case MI_PASTE:
        do_paste();
        break;
    case MI_CLEAR:
        delete_selection();
        break;
    case MI_FIND:
        do_find();
        break;
    case MI_FINDNEXT:
        repeat_find();
        break;
    case MI_CHANGE:
        do_change();
        break;
    case MI_ABOUT:
        dlg_msg("About", "TokaEdit  Version 1.0\nCopyright 1997 General Simulation Works");
        break;
    default:
        break;
    }
}

/* ---- mouse ---- */

/* true while the button is down and the press started in the text area;
 * gates EV_MOUSE_DRAG so drags starting on a scrollbar/menu don't select */
static int mouse_text_down;

/* map a text-area mouse cell to a clamped document position */
static void mouse_to_doc(int mrow, int mcol, int *out_row, int *out_col) {
    int row = top_row + mrow - TEXT_TOP;
    int col;
    if (row < 0)
        row = 0;
    if (row > doc.nlines - 1)
        row = doc.nlines - 1;
    col = left_col + mcol;
    if (col > doc.lens[row])
        col = doc.lens[row];
    if (col < 0)
        col = 0;
    *out_row = row;
    *out_col = col;
}

/* after a scrollbar click moves top_row/left_col, keep the cursor inside
 * the newly visible window instead of leaving it off-screen */
static void keep_cursor_in_view(void) {
    if (cur_row < top_row)
        cur_row = top_row;
    if (cur_row > top_row + TEXT_ROWS - 1)
        cur_row = top_row + TEXT_ROWS - 1;
    if (cur_row > doc.nlines - 1)
        cur_row = doc.nlines - 1;
    if (cur_row < 0)
        cur_row = 0;
    if (cur_col < left_col)
        cur_col = left_col;
    if (cur_col > left_col + 78)
        cur_col = left_col + 78;
}

static void mouse_event(const Event *e) {
    if (e->kind == EV_MOUSE_UP) {
        mouse_text_down = 0;
        return;
    }

    if (e->kind == EV_MOUSE_DOWN && e->mrow == 0) {
        int mi = menu_hit(e->mcol);
        if (mi >= 0)
            act(menu_run(mi, item_enabled));
        return;
    }

    /* text area: rows TEXT_TOP..TEXT_BOTTOM, cols 0..78 */
    if (e->mrow >= TEXT_TOP && e->mrow <= TEXT_BOTTOM && e->mcol >= 0 && e->mcol <= 78) {
        if (e->kind == EV_MOUSE_DOWN) {
            mouse_text_down = 1;
            mouse_to_doc(e->mrow, e->mcol, &cur_row, &cur_col);
            sel_active = 0;
            anch_row = cur_row;
            anch_col = cur_col;
            return;
        }
        if (e->kind == EV_MOUSE_DRAG && mouse_text_down) {
            mouse_to_doc(e->mrow, e->mcol, &cur_row, &cur_col);
            if (cur_row != anch_row || cur_col != anch_col)
                sel_active = 1;
            return;
        }
        return;
    }

    /* only DOWN acts on scrollbars; drags off a scrollbar are ignored
     * because mouse_text_down stays 0 for presses that start here */
    if (e->kind != EV_MOUSE_DOWN)
        return;

    /* vertical scrollbar, col 79, rows TEXT_TOP..TEXT_BOTTOM */
    if (e->mcol == 79 && e->mrow >= TEXT_TOP && e->mrow <= TEXT_BOTTOM) {
        if (e->mrow == TEXT_TOP) {
            top_row--;
        } else if (e->mrow == TEXT_BOTTOM) {
            top_row++;
        } else {
            int thumb = vthumb_row();
            if (e->mrow < thumb)
                top_row -= TEXT_ROWS;
            else if (e->mrow > thumb)
                top_row += TEXT_ROWS;
        }
        if (top_row < 0)
            top_row = 0;
        if (top_row > doc.nlines - 1)
            top_row = doc.nlines - 1;
        keep_cursor_in_view();
        return;
    }

    /* horizontal scrollbar, row 23, cols 0..78 */
    if (e->mrow == 23 && e->mcol >= 0 && e->mcol <= 78) {
        if (e->mcol == 0) {
            left_col--;
        } else if (e->mcol == 78) {
            left_col++;
        } else {
            int thumb = hthumb_col();
            if (e->mcol < thumb)
                left_col -= TEXT_COLS;
            else if (e->mcol > thumb)
                left_col += TEXT_COLS;
        }
        if (left_col < 0)
            left_col = 0;
        if (left_col > 255)
            left_col = 255;
        keep_cursor_in_view();
        return;
    }

    /* row 1 (title) and other dead zones: no-op */
}

/* ---- key dispatch ---- */

static void dispatch(const Event *e) {
    int shift, ctrl, alt;

    if (e->kind == EV_ALT_TAP) {
        act(menu_run(-1, item_enabled));
        return;
    }
    if (e->kind != EV_KEY) {
        mouse_event(e);
        return;
    }

    shift = e->mods & 1;
    ctrl  = e->mods & 2;
    alt   = e->mods & 4;

    switch (e->scan) {
    case 0x48: /* Up */
        if (shift) begin_extend(); else sel_active = 0;
        if (cur_row > 0) cur_row--;
        return;
    case 0x50: /* Down */
        if (shift) begin_extend(); else sel_active = 0;
        if (cur_row < doc.nlines - 1) cur_row++;
        return;
    case 0x4B: /* Left */
        if (shift) begin_extend(); else sel_active = 0;
        if (cur_col > 0) {
            cur_col--;
        } else if (cur_row > 0) {
            cur_row--;
            cur_col = doc.lens[cur_row];
        }
        return;
    case 0x4D: /* Right */
        if (shift) begin_extend(); else sel_active = 0;
        if (cur_col < doc.lens[cur_row]) {
            cur_col++;
        } else if (cur_row < doc.nlines - 1) {
            cur_row++;
            cur_col = 0;
        }
        return;
    case 0x47: /* Home */
        if (shift) begin_extend(); else sel_active = 0;
        cur_col = 0;
        return;
    case 0x4F: /* End */
        if (shift) begin_extend(); else sel_active = 0;
        cur_col = doc.lens[cur_row];
        return;
    case 0x49: /* PgUp */
        if (shift) begin_extend(); else sel_active = 0;
        cur_row -= TEXT_ROWS;
        top_row -= TEXT_ROWS;
        if (cur_row < 0) cur_row = 0;
        if (top_row < 0) top_row = 0;
        return;
    case 0x51: /* PgDn */
        if (shift) begin_extend(); else sel_active = 0;
        cur_row += TEXT_ROWS;
        top_row += TEXT_ROWS;
        if (cur_row > doc.nlines - 1) cur_row = doc.nlines - 1;
        return;
    case 0x77: /* Ctrl+Home */
        sel_active = 0;
        cur_row = 0;
        cur_col = 0;
        return;
    case 0x75: /* Ctrl+End */
        sel_active = 0;
        cur_row = doc.nlines - 1;
        cur_col = doc.lens[cur_row];
        return;
    case 0x73: /* Ctrl+Left */
        sel_active = 0;
        word_left();
        return;
    case 0x74: /* Ctrl+Right */
        sel_active = 0;
        word_right();
        return;
    case 0x52: /* Ins */
        if (shift) {
            do_paste();
        } else if (ctrl) {
            do_copy();
        } else {
            overwrite = !overwrite;
        }
        return;
    case 0x53: /* Del */
        if (shift) {
            do_cut();
        } else {
            Range r;
            if (get_sel(&r))
                delete_selection();
            else
                buf_delete_char(&doc, cur_row, cur_col);
        }
        return;
    case 0x3D: /* F3 */
        repeat_find();
        return;
    default:
        break;
    }

    /* alt combos: Alt+F/E/S/H open the matching pulldown directly (ascii
     * can be 0 with Alt held, so dispatch on scancode); any other Alt+key
     * is the mouse/menu layer's concern (Task 9) or unbound */
    if (alt) {
        switch (e->scan) {
        case 0x21: /* Alt+F */
            act(menu_run(0, item_enabled));
            return;
        case 0x12: /* Alt+E */
            act(menu_run(1, item_enabled));
            return;
        case 0x1F: /* Alt+S */
            act(menu_run(2, item_enabled));
            return;
        case 0x23: /* Alt+H */
            act(menu_run(3, item_enabled));
            return;
        default:
            return;
        }
    }

    switch (e->ascii) {
    case 8: /* Backspace */
        {
            Range r;
            if (get_sel(&r)) {
                delete_selection();
            } else if (cur_col > 0) {
                cur_col--;
                buf_delete_char(&doc, cur_row, cur_col);
            } else if (cur_row > 0) {
                cur_row--;
                cur_col = doc.lens[cur_row];
                buf_delete_char(&doc, cur_row, cur_col);
            }
        }
        return;
    case 13: /* Enter */
        delete_selection();
        if (buf_split_line(&doc, cur_row, cur_col)) {
            cur_row++;
            cur_col = 0;
        }
        return;
    case 9: /* Tab */
        {
            int stop = (cur_col / 8 + 1) * 8;
            int n = stop - cur_col;
            int i;
            delete_selection();
            for (i = 0; i < n; i++) {
                if (buf_insert_char(&doc, cur_row, cur_col, ' '))
                    cur_col++;
                else
                    break;
            }
        }
        return;
    case 27: /* Esc */
        sel_active = 0;
        return;
    default:
        break;
    }

    if (e->ascii >= 32 && !ctrl && !alt) {
        delete_selection();
        if (overwrite && cur_col < doc.lens[cur_row]) {
            if (buf_delete_char(&doc, cur_row, cur_col) &&
                buf_insert_char(&doc, cur_row, cur_col, (char)e->ascii))
                cur_col++;
        } else {
            if (buf_insert_char(&doc, cur_row, cur_col, (char)e->ascii))
                cur_col++;
        }
    }
}

int main(int argc, char *argv[]) {
    Event e;

    buf_init(&doc);
    docname[0] = '\0';
    cur_row = cur_col = 0;
    top_row = left_col = 0;
    sel_active = 0;
    anch_row = anch_col = 0;
    clipboard = NULL;
    last_find[0] = '\0';
    overwrite = 0;
    quit = 0;
    mouse_text_down = 0;

    if (argc > 1) {
        strncpy(docname, argv[1], sizeof(docname) - 1);
        docname[sizeof(docname) - 1] = '\0';
        strupr(docname);
        if (!load_file(docname)) {
            printf("EDIT: cannot load %s (too large?)\n", docname);
            return 1;
        }
    }

    scr_init();
    mouse_init();

    redraw();

    while (!quit) {
        ev_wait(&e);
        dispatch(&e);
        clamp_cursor_col();
        clamp_scroll();
        redraw();
    }

    scr_exit();
    return 0;
}
