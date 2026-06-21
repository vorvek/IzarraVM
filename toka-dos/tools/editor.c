/* editor.c - EDITOR: the Izarra full-screen text editor. EDIT is its alias.
 *
 * A pragmatic full-screen editor on the VGA 80x25 text screen. It opens a named
 * file (or starts empty), shows it with a status line, moves the cursor with the
 * arrow keys / Home / End / PgUp / PgDn, inserts and deletes text, saves with F2
 * or Ctrl-S, and quits with Esc (asking to save when the buffer is dirty).
 *
 * Keys come from the BIOS keyboard (INT 16h) so the editor sees raw scancodes for
 * the extended keys, which carry ASCII 0. The screen is drawn straight to the
 * B800 text buffer for a fast full redraw, and the hardware cursor is placed with
 * INT 10h.
 *
 * The file is held in a fixed line array. A file with more lines than MAX_LINES,
 * or any line longer than MAX_COLS, is refused at open time rather than loaded in
 * part: a partial load that was later saved would silently destroy the rest of
 * the file.
 */
#include <dos.h>
#include <string.h>
#include "toka.h"

#define TEXTROWS 24                /* rows 0..23 are text; row 24 is the status */
#define COLS     80
#define MAX_LINES 200
#define MAX_COLS  160

static char line[MAX_LINES][MAX_COLS + 1];
static int  len[MAX_LINES];
static int  nlines = 1;

static int  cx, cy;                /* cursor column / row within the file       */
static int  top;                   /* first visible file row                    */
static int  leftcol;               /* first visible column (horizontal scroll)  */
static int  dirty;
static char fname[80];
static const char *flash;          /* transient status message, or 0            */

static unsigned char far *screen;  /* B800 text buffer                          */

static void putcell(int row, int col, char ch, unsigned char attr)
{
    unsigned off = (unsigned)((row * COLS + col) * 2);
    screen[off] = (unsigned char)ch;
    screen[off + 1] = attr;
}

static int putnum(char *p, int v)
{
    char tmp[6];
    int i = 0, j = 0;
    if (v <= 0) {
        p[0] = '0';
        return 1;
    }
    while (v > 0 && i < 6) {
        tmp[i++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (i > 0) {
        p[j++] = tmp[--i];
    }
    return j;
}

static void draw_status(void)
{
    char buf[COLS];
    int i, n = 0;
    const char *right;
    int rl;

    buf[n++] = ' ';
    if (fname[0]) {
        for (i = 0; fname[i] && n < 60; i++) {
            buf[n++] = fname[i];
        }
    } else {
        const char *u = "(new)";
        for (i = 0; u[i] && n < 60; i++) {
            buf[n++] = u[i];
        }
    }
    if (dirty && n < 62) {
        buf[n++] = ' ';
        buf[n++] = '*';
    }
    if (n < 60) {
        buf[n++] = ' ';
        buf[n++] = ' ';
        buf[n++] = 'L';
        buf[n++] = 'n';
        buf[n++] = ' ';
        n += putnum(&buf[n], cy + 1);
        buf[n++] = ' ';
        buf[n++] = 'C';
        buf[n++] = 'o';
        buf[n++] = 'l';
        buf[n++] = ' ';
        n += putnum(&buf[n], cx + 1);
    }
    right = flash ? flash : "F2/Ctrl-S Save  Esc Quit";
    rl = (int)strlen(right);
    while (n < COLS - rl - 1) {
        buf[n++] = ' ';
    }
    for (i = 0; right[i] && n < COLS; i++) {
        buf[n++] = right[i];
    }
    while (n < COLS) {
        buf[n++] = ' ';
    }
    for (i = 0; i < COLS; i++) {
        putcell(TEXTROWS, i, buf[i], 0x70);
    }
}

static void render(void)
{
    int r, c, fr, sc;
    char ch;

    if (cx < leftcol) {
        leftcol = cx;
    }
    if (cx >= leftcol + COLS) {
        leftcol = cx - COLS + 1;
    }
    for (r = 0; r < TEXTROWS; r++) {
        fr = top + r;
        for (c = 0; c < COLS; c++) {
            ch = ' ';
            if (fr < nlines) {
                sc = leftcol + c;
                if (sc < len[fr]) {
                    ch = line[fr][sc];
                }
            }
            putcell(r, c, ch, 0x07);
        }
    }
    draw_status();
    t_setcursor(cy - top, cx - leftcol);
}

/* Load `path` into the line array. Returns 1 on success, 0 if the file is too
 * large for the editor (in which case nothing usable is loaded). A missing file
 * starts an empty buffer, which saving will create. */
static int load(const char *path)
{
    static char buf[1024];
    int h, n, i, li = 0, col = 0;

    h = t_open(path, 0);
    if (h < 0) {
        nlines = 1;
        len[0] = 0;
        line[0][0] = 0;
        return 1;
    }
    nlines = 0;
    while ((n = t_read(h, buf, sizeof(buf))) > 0) {
        for (i = 0; i < n; i++) {
            char c = buf[i];
            if (c == '\r') {
                continue;
            }
            if (c == '\n') {
                len[li] = col;
                line[li][col] = 0;
                li++;
                nlines = li;
                if (li >= MAX_LINES) {
                    t_close(h);
                    return 0;
                }
                col = 0;
            } else {
                if (col >= MAX_COLS) {
                    t_close(h);
                    return 0;
                }
                line[li][col++] = c;
            }
        }
    }
    if (col > 0 || nlines == 0) {
        len[li] = col;
        line[li][col] = 0;
        nlines = li + 1;
    }
    t_close(h);
    return 1;
}

static int save(const char *path)
{
    int h, i;
    h = t_create(path);
    if (h < 0) {
        return 0;
    }
    for (i = 0; i < nlines; i++) {
        if (len[i] > 0 && t_write(h, line[i], len[i]) < 0) {
            t_close(h);
            return 0;
        }
        if (t_write(h, "\r\n", 2) < 0) {
            t_close(h);
            return 0;
        }
    }
    t_close(h);
    return 1;
}

static void del_line(int idx)
{
    int i;
    for (i = idx; i < nlines - 1; i++) {
        memcpy(line[i], line[i + 1], (unsigned)(len[i + 1] + 1));
        len[i] = len[i + 1];
    }
    nlines--;
    if (nlines == 0) {
        nlines = 1;
        len[0] = 0;
        line[0][0] = 0;
    }
}

static void insert_char(char ch)
{
    int i;
    if (len[cy] >= MAX_COLS) {
        return;
    }
    for (i = len[cy]; i > cx; i--) {
        line[cy][i] = line[cy][i - 1];
    }
    line[cy][cx] = ch;
    len[cy]++;
    line[cy][len[cy]] = 0;
    cx++;
    dirty = 1;
}

static void newline(void)
{
    int i, tail;
    if (nlines >= MAX_LINES) {
        return;
    }
    for (i = nlines; i > cy + 1; i--) {
        memcpy(line[i], line[i - 1], (unsigned)(len[i - 1] + 1));
        len[i] = len[i - 1];
    }
    nlines++;
    tail = len[cy] - cx;
    memcpy(line[cy + 1], &line[cy][cx], (unsigned)tail);
    line[cy + 1][tail] = 0;
    len[cy + 1] = tail;
    line[cy][cx] = 0;
    len[cy] = cx;
    cy++;
    cx = 0;
    dirty = 1;
}

static void backspace(void)
{
    int i, p;
    if (cx > 0) {
        for (i = cx; i < len[cy]; i++) {
            line[cy][i - 1] = line[cy][i];
        }
        len[cy]--;
        line[cy][len[cy]] = 0;
        cx--;
        dirty = 1;
    } else if (cy > 0) {
        p = len[cy - 1];
        if (p + len[cy] <= MAX_COLS) {
            memcpy(&line[cy - 1][p], line[cy], (unsigned)(len[cy] + 1));
            len[cy - 1] = p + len[cy];
            del_line(cy);
            cy--;
            cx = p;
            dirty = 1;
        }
    }
}

static void delete_char(void)
{
    int i;
    if (cx < len[cy]) {
        for (i = cx; i < len[cy] - 1; i++) {
            line[cy][i] = line[cy][i + 1];
        }
        len[cy]--;
        line[cy][len[cy]] = 0;
        dirty = 1;
    } else if (cy < nlines - 1) {
        if (len[cy] + len[cy + 1] <= MAX_COLS) {
            memcpy(&line[cy][len[cy]], line[cy + 1], (unsigned)(len[cy + 1] + 1));
            len[cy] += len[cy + 1];
            del_line(cy + 1);
            dirty = 1;
        }
    }
}

static void clamp_cx(void)
{
    if (cx > len[cy]) {
        cx = len[cy];
    }
    if (cx < 0) {
        cx = 0;
    }
}

static void scroll(void)
{
    if (cy < top) {
        top = cy;
    }
    if (cy >= top + TEXTROWS) {
        top = cy - TEXTROWS + 1;
    }
}

/* Prompt for a file name on the status row. Returns 1 if a name was entered. */
static int prompt_name(void)
{
    char buf[COLS];
    const char *p = "Save as: ";
    int n = 0, k, asc, i, bn;

    flash = 0;
    for (;;) {
        bn = 0;
        for (i = 0; p[i]; i++) {
            buf[bn++] = p[i];
        }
        for (i = 0; i < n; i++) {
            buf[bn++] = fname[i];
        }
        while (bn < COLS) {
            buf[bn++] = ' ';
        }
        for (i = 0; i < COLS; i++) {
            putcell(TEXTROWS, i, buf[i], 0x70);
        }
        t_setcursor(TEXTROWS, 9 + n);
        k = t_readkey16();
        asc = k & 0xff;
        if (asc == 0x0d) {
            fname[n] = 0;
            return n > 0;
        }
        if (asc == 0x1b) {
            fname[0] = 0;
            return 0;
        }
        if (asc == 0x08) {
            if (n > 0) {
                n--;
            }
        } else if (asc >= 0x20 && asc <= 0x7e && n < 78) {
            fname[n++] = (char)asc;
        }
    }
}

static void do_save(void)
{
    if (!fname[0] && !prompt_name()) {
        return;
    }
    if (save(fname)) {
        dirty = 0;
        flash = "Saved";
    } else {
        flash = "Save failed";
    }
}

/* Returns 1 if it is OK to quit, 0 to stay in the editor. */
static int try_quit(void)
{
    int k;
    if (!dirty) {
        return 1;
    }
    flash = "Save changes? (Y=yes  N=no  Esc=cancel)";
    render();
    for (;;) {
        k = t_getkey();
        if (k == 'y' || k == 'Y') {
            do_save();
            return dirty ? 0 : 1; /* stay if the save failed */
        }
        if (k == 'n' || k == 'N') {
            return 1;
        }
        if (k == 0x1b) {
            flash = 0;
            return 0;
        }
    }
}

int main(void)
{
    char *tail;
    int k, sc, asc, i;

    screen = (unsigned char far *)MK_FP(0xB800, 0);

    tail = t_cmdtail();
    while (*tail == ' ') {
        tail++;
    }
    for (i = 0; tail[i] && tail[i] != ' ' && i < 78; i++) {
        fname[i] = tail[i];
    }
    fname[i] = 0;

    t_setmode3();
    if (fname[0] && !load(fname)) {
        t_setmode3();
        t_putln("File too large for the editor.");
        t_exit(1);
    }
    cx = cy = top = leftcol = 0;
    dirty = 0;

    for (;;) {
        scroll();
        render();
        k = t_readkey16();
        flash = 0;
        asc = k & 0xff;
        sc = (k >> 8) & 0xff;
        if (asc == 0) {
            switch (sc) {
            case 0x48:
                if (cy > 0) {
                    cy--;
                    clamp_cx();
                }
                break;
            case 0x50:
                if (cy < nlines - 1) {
                    cy++;
                    clamp_cx();
                }
                break;
            case 0x4b:
                if (cx > 0) {
                    cx--;
                } else if (cy > 0) {
                    cy--;
                    cx = len[cy];
                }
                break;
            case 0x4d:
                if (cx < len[cy]) {
                    cx++;
                } else if (cy < nlines - 1) {
                    cy++;
                    cx = 0;
                }
                break;
            case 0x47:
                cx = 0;
                break;
            case 0x4f:
                cx = len[cy];
                break;
            case 0x49:
                cy -= TEXTROWS;
                if (cy < 0) {
                    cy = 0;
                }
                clamp_cx();
                break;
            case 0x51:
                cy += TEXTROWS;
                if (cy > nlines - 1) {
                    cy = nlines - 1;
                }
                clamp_cx();
                break;
            case 0x53:
                delete_char();
                break;
            case 0x3c:
                do_save();
                break;
            default:
                break;
            }
        } else {
            switch (asc) {
            case 0x1b:
                if (try_quit()) {
                    t_setmode3();
                    t_exit(0);
                }
                break;
            case 0x08:
                backspace();
                break;
            case 0x0d:
                newline();
                break;
            case 0x13:
                do_save();
                break;
            default:
                if (asc >= 0x20 && asc <= 0x7e) {
                    insert_char((char)asc);
                }
                break;
            }
        }
    }
}
