/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

/* TokaEdit text buffer core implementation. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <ctype.h>
#include <stdlib.h>
#include <string.h>
#include "buffer.h"

#ifdef BUF_TEST_ALLOC
/* Test-only allocation shim: lets the host test harness force the Nth
 * allocation to fail, and tracks an outstanding-allocation balance so
 * leaks show up as a nonzero balance at the end of the test run. */
static long buf_alloc_count = 0;
static long buf_alloc_fail_at = -1; /* -1 = never fail */
static long buf_alloc_balance = 0;

void buf_test_alloc_reset(void) {
    buf_alloc_count = 0;
    buf_alloc_fail_at = -1;
}

void buf_test_alloc_fail_at(long n) {
    buf_alloc_count = 0;
    buf_alloc_fail_at = n;
}

long buf_test_alloc_balance(void) {
    return buf_alloc_balance;
}

static int buf_alloc_should_fail(void) {
    buf_alloc_count++;
    return buf_alloc_fail_at >= 0 && buf_alloc_count == buf_alloc_fail_at;
}

static void *buf_malloc(size_t n) {
    void *p;
    if (buf_alloc_should_fail()) return NULL;
    p = malloc(n);
    if (p) buf_alloc_balance++;
    return p;
}

static void *buf_realloc(void *old, size_t n) {
    void *p;
    if (buf_alloc_should_fail()) return NULL;
    p = realloc(old, n);
    if (p && !old) buf_alloc_balance++;
    return p;
}

static void buf_free_mem(void *p) {
    if (p) buf_alloc_balance--;
    free(p);
}
#else
#define buf_malloc malloc
#define buf_realloc realloc
#define buf_free_mem free
#endif

static int chareq(char a, char b, int fold) {
    if (fold) return tolower((unsigned char)a) == tolower((unsigned char)b);
    return a == b;
}

static int line_match_at(const char *line, int linelen, int col,
                          const char *needle, int needlelen, int fold) {
    int i;
    if (col + needlelen > linelen) return 0;
    for (i = 0; i < needlelen; i++)
        if (!chareq(line[col + i], needle[i], fold)) return 0;
    return 1;
}

static int scan_line_from(const Buf *b, int row, int startcol,
                           const char *needle, int needlelen, int fold,
                           int *fc) {
    int col;
    for (col = startcol; col <= b->lens[row] - needlelen; col++) {
        if (line_match_at(b->lines[row], b->lens[row], col, needle, needlelen, fold)) {
            *fc = col;
            return 1;
        }
    }
    return 0;
}

int buf_init(Buf *b) {
    b->cap = 64;
    b->nlines = 1;
    b->dirty = 0;
    b->lines = buf_malloc((size_t)b->cap * sizeof(char *));
    b->lens  = buf_malloc((size_t)b->cap * sizeof(int));
    if (!b->lines || !b->lens) return 0;
    b->lines[0] = buf_malloc(1);
    if (!b->lines[0]) return 0;
    b->lines[0][0] = 0;
    b->lens[0] = 0;
    return 1;
}

void buf_free(Buf *b) {
    int i;
    for (i = 0; i < b->nlines; i++) buf_free_mem(b->lines[i]);
    buf_free_mem(b->lines);
    buf_free_mem(b->lens);
    b->lines = NULL;
    b->lens = NULL;
    b->nlines = 0;
    b->cap = 0;
}

static int buf_reserve(Buf *b, int n) {
    char **nl; int *nn; int c = b->cap;
    if (n > BUF_MAX_LINES) return 0;
    if (n <= c) return 1;
    while (c < n) c *= 2;
    if (c > BUF_MAX_LINES) c = BUF_MAX_LINES;
    nl = buf_realloc(b->lines, (size_t)c * sizeof(char *));
    if (!nl) return 0;
    b->lines = nl;
    nn = buf_realloc(b->lens, (size_t)c * sizeof(int));
    if (!nn) return 0;
    b->lens = nn;
    b->cap = c;
    return 1;
}

static int append_line(Buf *b, const char *s, int len) {
    char *dup;
    if (!buf_reserve(b, b->nlines + 1)) return 0;
    dup = buf_malloc((size_t)len + 1);
    if (!dup) return 0;
    memcpy(dup, s, (size_t)len);
    dup[len] = 0;
    b->lines[b->nlines] = dup;
    b->lens[b->nlines] = len;
    b->nlines++;
    return 1;
}

/* Shared per-character state machine driving both buf_load (from a memory
 * block) and buf_load_stream (from a FILE*): line accumulator + tab
 * expansion + CR skip + LF commit. */
typedef struct {
    Buf *b;
    char cur[BUF_MAX_LINE + 1];
    int  col;
} Loader;

static void loader_init(Loader *ld, Buf *b) {
    ld->b = b;
    ld->col = 0;
}

/* 1 ok, 0 refused (line too long/oom) */
static int loader_feed(Loader *ld, char ch) {
    if (ch == '\r') return 1;                   /* CRLF: LF ends the line */
    if (ch == '\n') {
        if (!append_line(ld->b, ld->cur, ld->col)) return 0;
        ld->col = 0;
        return 1;
    }
    if (ch == '\t') {                            /* expand to the next 8-col stop */
        int stop = (ld->col / 8 + 1) * 8;
        if (stop > BUF_MAX_LINE) return 0;
        while (ld->col < stop) ld->cur[ld->col++] = ' ';
        return 1;
    }
    if (ld->col >= BUF_MAX_LINE) return 0;
    ld->cur[ld->col++] = ch;
    return 1;
}

/* A trailing partial line with no final newline still becomes a line;
 * input ending in \n contributes nothing further (already appended). */
static int loader_finish(Loader *ld) {
    if (ld->col > 0 || ld->b->nlines == 0) {
        if (!append_line(ld->b, ld->cur, ld->col)) return 0;
    }
    return 1;
}

static void loader_discard_existing(Buf *b) {
    int oldn = b->nlines, k;                     /* free every existing line, */
    for (k = 0; k < oldn; k++) buf_free_mem(b->lines[k]); /* not just [0] */
    b->nlines = 0;                                /* rebuild from scratch */
}

/* May be called on any initialized Buf, fresh or already loaded; it
 * discards the current content and replaces it. */
int buf_load(Buf *b, const char *data, long n) {
    Loader ld;
    long i;
    if (n > BUF_MAX_LOAD) return 0;
    if (n > 0 && data[n - 1] == 0x1A) n--;     /* one trailing DOS EOF char */
    loader_discard_existing(b);
    loader_init(&ld, b);
    for (i = 0; i < n; i++)
        if (!loader_feed(&ld, data[i])) return 0;
    if (!loader_finish(&ld)) return 0;
    b->dirty = 0;
    return 1;
}

/* Streams from f in small chunks: no whole-file staging allocation (a
 * single malloc cannot exceed 64 KB on the 16-bit DOS build). A 0x1A byte
 * stops reading immediately (treated as end-of-input), matching the
 * period-correct DOS text-file convention; see buffer.h. */
int buf_load_stream(Buf *b, FILE *f) {
    Loader ld;
    char chunk[512];
    long total = 0;
    size_t got, k;
    int stopped = 0;

    loader_discard_existing(b);
    loader_init(&ld, b);
    while (!stopped && (got = fread(chunk, 1, sizeof chunk, f)) > 0) {
        for (k = 0; k < got; k++) {
            char ch = chunk[k];
            if (ch == 0x1A) { stopped = 1; break; }
            if (++total > BUF_MAX_LOAD) return 0;
            if (!loader_feed(&ld, ch)) return 0;
        }
    }
    if (ferror(f)) return 0;
    if (!loader_finish(&ld)) return 0;
    b->dirty = 0;
    return 1;
}

int buf_save_stream(const Buf *b, FILE *f) {
    int i;
    for (i = 0; i < b->nlines; i++) {
        if (b->lens[i] > 0 &&
            fwrite(b->lines[i], 1, (size_t)b->lens[i], f) != (size_t)b->lens[i])
            return 0;
        if (fwrite("\r\n", 1, 2, f) != 2) return 0;
    }
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
    nl = buf_realloc(b->lines[row], (size_t)len + 2);
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
        char *nl = buf_realloc(b->lines[row], (size_t)nlen + 1);
        if (!nl) return 0;
        b->lines[row] = nl;
        memcpy(nl + b->lens[row], b->lines[row + 1], (size_t)b->lens[row + 1] + 1);
        b->lens[row] = nlen;
    }
    buf_free_mem(b->lines[row + 1]);
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
    tail = buf_malloc((size_t)taillen + 1);
    if (!tail) return 0;
    memcpy(tail, b->lines[row] + col, (size_t)taillen);
    tail[taillen] = 0;
    if (!buf_reserve(b, b->nlines + 1)) { buf_free_mem(tail); return 0; }
    head = buf_realloc(b->lines[row], (size_t)col + 1);
    if (!head) { buf_free_mem(tail); return 0; }
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

/* Caller-owned output, freed by the caller with plain free() per
 * buffer.h; deliberately not routed through the buf_malloc test shim. */
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
    /* One clipboard block must stay under the 16-bit DOS size_t: a bigger
     * range would truncate in malloc below and overflow the heap. Refuse. */
    if (total > BUF_MAX_RANGE) return NULL;
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
        nl = buf_malloc((size_t)nlen + 1);
        if (!nl) return 0;
        memcpy(nl, b->lines[r->r1], (size_t)headlen);
        memcpy(nl + headlen, b->lines[r->r2] + r->c2, (size_t)tailfromlen);
        nl[nlen] = 0;
        buf_free_mem(b->lines[r->r1]);
        b->lines[r->r1] = nl;
        b->lens[r->r1] = nlen;
        for (i = r->r1 + 1; i <= r->r2; i++) buf_free_mem(b->lines[i]);
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

/* Allocate-all-then-commit: every new line string this call needs is
 * allocated into a local temporary first. Only once ALL allocations (and
 * the capacity reserve) have succeeded do we touch b->lines/b->lens/
 * b->nlines. On any failure every temporary is freed and the buffer is
 * returned completely untouched. */
int buf_insert_text(Buf *b, int row, int col, const char *text,
                     int *end_row, int *end_col) {
    long nsegs_l = 1;
    int nsegs;
    const char *p;
    int i;
    const char **seg_start; int *seg_len;
    int headlen, taillen, ok;
    char **new_lines; int *new_lens; /* temporaries: one per new line owned by this call */

    /* count segments (split on \n, skip \r), guarding against overflowing
     * the buffer's total line count before nsegs itself is even used as
     * an int (16-bit int on the DOS build). */
    for (p = text; *p; p++) if (*p == '\n') nsegs_l++;
    if ((long)b->nlines - 1 + nsegs_l > BUF_MAX_LINES) return 0;
    nsegs = (int)nsegs_l;

    seg_start = buf_malloc((size_t)nsegs * sizeof(char *));
    seg_len = buf_malloc((size_t)nsegs * sizeof(int));
    if (!seg_start || !seg_len) {
        buf_free_mem(seg_start); buf_free_mem(seg_len); return 0;
    }

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
    if (!ok) { buf_free_mem(seg_start); buf_free_mem(seg_len); return 0; }

    if (nsegs == 1) {
        int newlen = headlen + seg_len[0] + taillen;
        char *nl = buf_malloc((size_t)newlen + 1);
        if (!nl) { buf_free_mem(seg_start); buf_free_mem(seg_len); return 0; }
        memcpy(nl, b->lines[row], (size_t)headlen);
        memcpy(nl + headlen, seg_start[0], (size_t)seg_len[0]);
        memcpy(nl + headlen + seg_len[0], b->lines[row] + col, (size_t)taillen);
        nl[newlen] = 0;
        /* commit: single-line case cannot fail from here on */
        buf_free_mem(b->lines[row]);
        b->lines[row] = nl;
        b->lens[row] = newlen;
        *end_row = row;
        *end_col = headlen + seg_len[0];
        buf_free_mem(seg_start);
        buf_free_mem(seg_len);
        b->dirty = 1;
        return 1;
    }

    /* multi-segment: build every new line into temporaries first. There
     * are nsegs lines total covering row..row+nsegs-1: line 0 (head+seg0),
     * middle lines 1..nsegs-2 (seg_i verbatim), last line (seg_last+tail). */
    new_lines = buf_malloc((size_t)nsegs * sizeof(char *));
    new_lens  = buf_malloc((size_t)nsegs * sizeof(int));
    if (!new_lines || !new_lens) {
        buf_free_mem(new_lines); buf_free_mem(new_lens);
        buf_free_mem(seg_start); buf_free_mem(seg_len);
        return 0;
    }
    for (i = 0; i < nsegs; i++) new_lines[i] = NULL; /* so cleanup can free safely */

    ok = 1;
    {
        int newlen0 = headlen + seg_len[0];
        char *nl0 = buf_malloc((size_t)newlen0 + 1);
        if (!nl0) { ok = 0; }
        else {
            memcpy(nl0, b->lines[row], (size_t)headlen);
            memcpy(nl0 + headlen, seg_start[0], (size_t)seg_len[0]);
            nl0[newlen0] = 0;
            new_lines[0] = nl0;
            new_lens[0] = newlen0;
        }
    }
    for (i = 1; ok && i < nsegs - 1; i++) {
        char *ml = buf_malloc((size_t)seg_len[i] + 1);
        if (!ml) { ok = 0; break; }
        memcpy(ml, seg_start[i], (size_t)seg_len[i]);
        ml[seg_len[i]] = 0;
        new_lines[i] = ml;
        new_lens[i] = seg_len[i];
    }
    if (ok) {
        int lastlen = seg_len[nsegs - 1] + taillen;
        char *ll = buf_malloc((size_t)lastlen + 1);
        if (!ll) { ok = 0; }
        else {
            memcpy(ll, seg_start[nsegs - 1], (size_t)seg_len[nsegs - 1]);
            memcpy(ll + seg_len[nsegs - 1], b->lines[row] + col, (size_t)taillen);
            ll[lastlen] = 0;
            new_lines[nsegs - 1] = ll;
            new_lens[nsegs - 1] = lastlen;
        }
    }
    /* reserve capacity for the net new lines (nsegs - 1 more than before) */
    if (ok && !buf_reserve(b, b->nlines + (nsegs - 1))) ok = 0;

    if (!ok) {
        for (i = 0; i < nsegs; i++) buf_free_mem(new_lines[i]);
        buf_free_mem(new_lines); buf_free_mem(new_lens);
        buf_free_mem(seg_start); buf_free_mem(seg_len);
        return 0;
    }

    /* commit: everything is allocated and capacity is reserved, so the
     * rest cannot fail. */
    {
        int newlines_to_add = nsegs - 1;
        int wr;
        for (i = b->nlines - 1; i > row; i--) {
            b->lines[i + newlines_to_add] = b->lines[i];
            b->lens[i + newlines_to_add] = b->lens[i];
        }
        buf_free_mem(b->lines[row]); /* replaced by new_lines[0] */
        b->nlines += newlines_to_add;
        wr = row;
        for (i = 0; i < nsegs; i++) {
            b->lines[wr] = new_lines[i];
            b->lens[wr] = new_lens[i];
            wr++;
        }
        *end_row = row + nsegs - 1;
        *end_col = seg_len[nsegs - 1];
    }

    buf_free_mem(new_lines);
    buf_free_mem(new_lens);
    buf_free_mem(seg_start);
    buf_free_mem(seg_len);
    b->dirty = 1;
    return 1;
}

int buf_find(const Buf *b, int row, int col, const char *needle, int fold,
             int *fr, int *fc) {
    int needlelen = (int)strlen(needle);
    int r;

    if (needlelen == 0) return 0;

    /* rest of the start row, from col+1 */
    if (scan_line_from(b, row, col + 1, needle, needlelen, fold, fc)) {
        *fr = row;
        return 1;
    }
    /* following lines to the end */
    for (r = row + 1; r < b->nlines; r++) {
        if (scan_line_from(b, r, 0, needle, needlelen, fold, fc)) {
            *fr = r;
            return 1;
        }
    }
    /* wrap: row 0 through the start row inclusive */
    for (r = 0; r <= row; r++) {
        if (scan_line_from(b, r, 0, needle, needlelen, fold, fc)) {
            *fr = r;
            return 1;
        }
    }
    return 0;
}
