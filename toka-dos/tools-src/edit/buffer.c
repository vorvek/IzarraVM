/* TokaEdit text buffer core implementation. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <stdlib.h>
#include <string.h>
#include "buffer.h"

int buf_init(Buf *b) {
    b->cap = 64;
    b->nlines = 1;
    b->dirty = 0;
    b->lines = malloc((size_t)b->cap * sizeof(char *));
    b->lens  = malloc((size_t)b->cap * sizeof(int));
    if (!b->lines || !b->lens) return 0;
    b->lines[0] = malloc(1);
    if (!b->lines[0]) return 0;
    b->lines[0][0] = 0;
    b->lens[0] = 0;
    return 1;
}

void buf_free(Buf *b) {
    int i;
    for (i = 0; i < b->nlines; i++) free(b->lines[i]);
    free(b->lines);
    free(b->lens);
    b->lines = NULL;
    b->lens = NULL;
    b->nlines = 0;
    b->cap = 0;
}

static int buf_reserve(Buf *b, int n) {
    char **nl; int *nn; int c = b->cap;
    if (n <= c) return 1;
    while (c < n) c *= 2;
    nl = realloc(b->lines, (size_t)c * sizeof(char *));
    if (!nl) return 0;
    b->lines = nl;
    nn = realloc(b->lens, (size_t)c * sizeof(int));
    if (!nn) return 0;
    b->lens = nn;
    b->cap = c;
    return 1;
}

static int append_line(Buf *b, const char *s, int len) {
    char *dup = malloc((size_t)len + 1);
    if (!dup || !buf_reserve(b, b->nlines + 1)) { free(dup); return 0; }
    memcpy(dup, s, (size_t)len);
    dup[len] = 0;
    b->lines[b->nlines] = dup;
    b->lens[b->nlines] = len;
    b->nlines++;
    return 1;
}

int buf_load(Buf *b, const char *data, long n) {
    char cur[BUF_MAX_LINE + 1];
    int col = 0;
    long i;
    if (n > BUF_MAX_LOAD) return 0;
    if (n > 0 && data[n - 1] == 0x1A) n--;     /* one trailing DOS EOF char */
    b->nlines = 0;                              /* rebuild over the init line */
    free(b->lines[0]);
    for (i = 0; i < n; i++) {
        char ch = data[i];
        if (ch == '\r') continue;               /* CRLF: LF ends the line */
        if (ch == '\n') {
            if (!append_line(b, cur, col)) return 0;
            col = 0;
            continue;
        }
        if (ch == '\t') {                       /* expand to the next 8-col stop */
            int stop = (col / 8 + 1) * 8;
            if (stop > BUF_MAX_LINE) return 0;
            while (col < stop) cur[col++] = ' ';
            continue;
        }
        if (col >= BUF_MAX_LINE) return 0;
        cur[col++] = ch;
    }
    if (col > 0 || b->nlines == 0) {
        if (!append_line(b, cur, col)) return 0;
    }
    b->dirty = 0;
    return 1;
}

long buf_save_size(const Buf *b) {
    long n = 0; int i;
    for (i = 0; i < b->nlines; i++) n += b->lens[i] + 2;
    return n;
}

void buf_serialize(const Buf *b, char *out) {
    int i;
    for (i = 0; i < b->nlines; i++) {
        memcpy(out, b->lines[i], (size_t)b->lens[i]);
        out += b->lens[i];
        *out++ = '\r';
        *out++ = '\n';
    }
}

int buf_insert_char(Buf *b, int row, int col, char ch) {
    (void)b; (void)row; (void)col; (void)ch;
    return 0;
}

int buf_delete_char(Buf *b, int row, int col) {
    (void)b; (void)row; (void)col;
    return 0;
}

int buf_split_line(Buf *b, int row, int col) {
    (void)b; (void)row; (void)col;
    return 0;
}

char *buf_get_range(const Buf *b, const Range *r) {
    (void)b; (void)r;
    return NULL;
}

int buf_delete_range(Buf *b, const Range *r) {
    (void)b; (void)r;
    return 0;
}

int buf_insert_text(Buf *b, int row, int col, const char *text,
                     int *end_row, int *end_col) {
    (void)b; (void)row; (void)col; (void)text; (void)end_row; (void)end_col;
    return 0;
}

int buf_find(const Buf *b, int row, int col, const char *needle, int fold,
             int *fr, int *fc) {
    (void)b; (void)row; (void)col; (void)needle; (void)fold; (void)fr; (void)fc;
    return 0;
}
