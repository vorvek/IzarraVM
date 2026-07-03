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
    int len; char *nl;
    len = b->lens[row];
    if (len >= BUF_MAX_LINE) return 0;
    nl = realloc(b->lines[row], (size_t)len + 2);
    if (!nl) return 0;
    b->lines[row] = nl;
    memmove(nl + col + 1, nl + col, (size_t)(len - col) + 1);
    nl[col] = ch;
    b->lens[row] = len + 1;
    b->dirty = 1;
    return 1;
}

int buf_delete_char(Buf *b, int row, int col) {
    int len, i;
    len = b->lens[row];
    if (col < len) {
        memmove(b->lines[row] + col, b->lines[row] + col + 1,
                (size_t)(len - col));
        b->lens[row] = len - 1;
        b->dirty = 1;
        return 1;
    }
    /* col == len: EOL, join with next line */
    if (row + 1 >= b->nlines) return 0;
    if (b->lens[row] + b->lens[row + 1] > BUF_MAX_LINE) return 0;
    {
        int nlen = b->lens[row] + b->lens[row + 1];
        char *nl = realloc(b->lines[row], (size_t)nlen + 1);
        if (!nl) return 0;
        b->lines[row] = nl;
        memcpy(nl + b->lens[row], b->lines[row + 1], (size_t)b->lens[row + 1] + 1);
        b->lens[row] = nlen;
    }
    free(b->lines[row + 1]);
    for (i = row + 1; i < b->nlines - 1; i++) {
        b->lines[i] = b->lines[i + 1];
        b->lens[i] = b->lens[i + 1];
    }
    b->nlines--;
    b->dirty = 1;
    return 1;
}

int buf_split_line(Buf *b, int row, int col) {
    int len, taillen, i;
    char *tail, *head;
    len = b->lens[row];
    taillen = len - col;
    tail = malloc((size_t)taillen + 1);
    if (!tail) return 0;
    memcpy(tail, b->lines[row] + col, (size_t)taillen);
    tail[taillen] = 0;
    if (!buf_reserve(b, b->nlines + 1)) { free(tail); return 0; }
    head = realloc(b->lines[row], (size_t)col + 1);
    if (!head) { free(tail); return 0; }
    head[col] = 0;
    b->lines[row] = head;
    b->lens[row] = col;
    for (i = b->nlines; i > row + 1; i--) {
        b->lines[i] = b->lines[i - 1];
        b->lens[i] = b->lens[i - 1];
    }
    b->lines[row + 1] = tail;
    b->lens[row + 1] = taillen;
    b->nlines++;
    b->dirty = 1;
    return 1;
}

char *buf_get_range(const Buf *b, const Range *r) {
    long total; char *out; char *p; int row;
    if (r->r1 == r->r2) {
        int len = r->c2 - r->c1;
        char *s = malloc((size_t)len + 1);
        if (!s) return NULL;
        memcpy(s, b->lines[r->r1] + r->c1, (size_t)len);
        s[len] = 0;
        return s;
    }
    total = (long)(b->lens[r->r1] - r->c1) + 2;
    for (row = r->r1 + 1; row < r->r2; row++) total += b->lens[row] + 2;
    total += r->c2;
    out = malloc((size_t)total + 1);
    if (!out) return NULL;
    p = out;
    memcpy(p, b->lines[r->r1] + r->c1, (size_t)(b->lens[r->r1] - r->c1));
    p += b->lens[r->r1] - r->c1;
    *p++ = '\r'; *p++ = '\n';
    for (row = r->r1 + 1; row < r->r2; row++) {
        memcpy(p, b->lines[row], (size_t)b->lens[row]);
        p += b->lens[row];
        *p++ = '\r'; *p++ = '\n';
    }
    memcpy(p, b->lines[r->r2], (size_t)r->c2);
    p += r->c2;
    *p = 0;
    return out;
}

int buf_delete_range(Buf *b, const Range *r) {
    if (r->r1 == r->r2) {
        int len = b->lens[r->r1];
        int cutlen = r->c2 - r->c1;
        memmove(b->lines[r->r1] + r->c1, b->lines[r->r1] + r->c2,
                (size_t)(len - r->c2) + 1);
        b->lens[r->r1] = len - cutlen;
        b->dirty = 1;
        return 1;
    }
    {
        int headlen, tailfromlen, nlen, i, gap;
        char *nl;
        headlen = r->c1;
        tailfromlen = b->lens[r->r2] - r->c2;
        nlen = headlen + tailfromlen;
        if (nlen > BUF_MAX_LINE) return 0;
        nl = malloc((size_t)nlen + 1);
        if (!nl) return 0;
        memcpy(nl, b->lines[r->r1], (size_t)headlen);
        memcpy(nl + headlen, b->lines[r->r2] + r->c2, (size_t)tailfromlen);
        nl[nlen] = 0;
        free(b->lines[r->r1]);
        b->lines[r->r1] = nl;
        b->lens[r->r1] = nlen;
        for (i = r->r1 + 1; i <= r->r2; i++) free(b->lines[i]);
        gap = r->r2 - r->r1;
        for (i = r->r1 + 1; i < b->nlines - gap; i++) {
            b->lines[i] = b->lines[i + gap];
            b->lens[i] = b->lens[i + gap];
        }
        b->nlines -= gap;
        b->dirty = 1;
        return 1;
    }
}

int buf_insert_text(Buf *b, int row, int col, const char *text,
                     int *end_row, int *end_col) {
    int nsegs = 1;
    const char *p;
    int i;
    const char **seg_start; int *seg_len;
    int headlen, taillen, ok;

    /* count segments (split on \n, skip \r) */
    for (p = text; *p; p++) if (*p == '\n') nsegs++;

    seg_start = malloc((size_t)nsegs * sizeof(char *));
    seg_len = malloc((size_t)nsegs * sizeof(int));
    if (!seg_start || !seg_len) { free(seg_start); free(seg_len); return 0; }

    {
        int idx = 0;
        const char *segbeg = text;
        for (p = text; ; p++) {
            if (*p == '\n' || *p == 0) {
                int len = (int)(p - segbeg);
                /* trim trailing \r from this segment */
                if (len > 0 && segbeg[len - 1] == '\r') len--;
                seg_start[idx] = segbeg;
                seg_len[idx] = len;
                idx++;
                if (*p == 0) break;
                segbeg = p + 1;
            }
        }
    }

    headlen = col;
    taillen = b->lens[row] - col;

    /* validate lengths first */
    ok = 1;
    if (nsegs == 1) {
        if (headlen + seg_len[0] + taillen > BUF_MAX_LINE) ok = 0;
    } else {
        if (headlen + seg_len[0] > BUF_MAX_LINE) ok = 0;
        for (i = 1; i < nsegs - 1; i++)
            if (seg_len[i] > BUF_MAX_LINE) ok = 0;
        if (seg_len[nsegs - 1] + taillen > BUF_MAX_LINE) ok = 0;
    }
    if (!ok) { free(seg_start); free(seg_len); return 0; }

    if (nsegs == 1) {
        int newlen = headlen + seg_len[0] + taillen;
        char *nl = realloc(b->lines[row], (size_t)newlen + 1);
        if (!nl) { free(seg_start); free(seg_len); return 0; }
        memmove(nl + headlen + seg_len[0], nl + headlen, (size_t)taillen);
        memcpy(nl + headlen, seg_start[0], (size_t)seg_len[0]);
        nl[newlen] = 0;
        b->lines[row] = nl;
        b->lens[row] = newlen;
        *end_row = row;
        *end_col = headlen + seg_len[0];
    } else {
        char *tail_orig; int tail_orig_len;
        int newlines_to_add;
        int wr;

        tail_orig_len = taillen;
        tail_orig = malloc((size_t)tail_orig_len + 1);
        if (!tail_orig) { free(seg_start); free(seg_len); return 0; }
        memcpy(tail_orig, b->lines[row] + col, (size_t)tail_orig_len);
        tail_orig[tail_orig_len] = 0;

        newlines_to_add = nsegs - 1;
        if (!buf_reserve(b, b->nlines + newlines_to_add)) {
            free(tail_orig); free(seg_start); free(seg_len);
            return 0;
        }

        /* shift existing lines after row down by newlines_to_add */
        for (i = b->nlines - 1; i > row; i--) {
            b->lines[i + newlines_to_add] = b->lines[i];
            b->lens[i + newlines_to_add] = b->lens[i];
        }
        b->nlines += newlines_to_add;

        /* line row: head + seg0 */
        {
            int newlen0 = headlen + seg_len[0];
            char *nl0 = realloc(b->lines[row], (size_t)newlen0 + 1);
            if (!nl0) { free(tail_orig); free(seg_start); free(seg_len); return 0; }
            memcpy(nl0 + headlen, seg_start[0], (size_t)seg_len[0]);
            nl0[newlen0] = 0;
            b->lines[row] = nl0;
            b->lens[row] = newlen0;
        }

        /* middle segments become new lines row+1..row+nsegs-2 */
        wr = row + 1;
        for (i = 1; i < nsegs - 1; i++) {
            char *ml = malloc((size_t)seg_len[i] + 1);
            if (!ml) { free(tail_orig); free(seg_start); free(seg_len); return 0; }
            memcpy(ml, seg_start[i], (size_t)seg_len[i]);
            ml[seg_len[i]] = 0;
            b->lines[wr] = ml;
            b->lens[wr] = seg_len[i];
            wr++;
        }

        /* last segment + original tail becomes line row+nsegs-1 */
        {
            int lastlen = seg_len[nsegs - 1] + tail_orig_len;
            char *ll = malloc((size_t)lastlen + 1);
            if (!ll) { free(tail_orig); free(seg_start); free(seg_len); return 0; }
            memcpy(ll, seg_start[nsegs - 1], (size_t)seg_len[nsegs - 1]);
            memcpy(ll + seg_len[nsegs - 1], tail_orig, (size_t)tail_orig_len);
            ll[lastlen] = 0;
            b->lines[wr] = ll;
            b->lens[wr] = lastlen;
        }

        *end_row = row + nsegs - 1;
        *end_col = seg_len[nsegs - 1];
        free(tail_orig);
    }

    free(seg_start);
    free(seg_len);
    b->dirty = 1;
    return 1;
}

int buf_find(const Buf *b, int row, int col, const char *needle, int fold,
             int *fr, int *fc) {
    (void)b; (void)row; (void)col; (void)needle; (void)fold; (void)fr; (void)fc;
    return 0;
}
