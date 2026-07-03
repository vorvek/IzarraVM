/* TokaEdit text buffer core. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#ifndef BUFFER_H
#define BUFFER_H

#include <stdio.h>

#define BUF_MAX_LINE 255
#define BUF_MAX_LOAD 409600L /* 400 KB */
/* Fits a 16-bit int with headroom. A 400 KB document can in theory exceed
 * this many lines, but 16000 separate line mallocs is already far beyond
 * what fits in DOS conventional memory, so the cap never bites first in
 * practice. */
#define BUF_MAX_LINES 16000

typedef struct {
    char **lines;   /* nlines malloc'd NUL-terminated strings */
    int   *lens;    /* strlen of each line, kept in step */
    int    nlines;  /* >= 1 always (an empty buffer is one empty line) */
    int    cap;     /* allocated slots in lines/lens */
    int    dirty;   /* modified since load/save */
} Buf;

/* lifecycle */
int  buf_init(Buf *b);                       /* 1 ok, 0 out of memory */
void buf_free(Buf *b);
/* load from raw file bytes: splits CRLF (tolerates LF), expands tabs to
 * 8-col stops, strips one trailing 0x1A. 1 ok, 0 oom/too-big/line-too-long.
 * b must already be buf_init'd; may be called again on a buffer that
 * already holds content (fresh or previously loaded) to replace it. On
 * refusal the buffer is left valid-but-partial (all of lines 0..nlines-1
 * are valid strings); the only additional requirement after a refusal is
 * that buf_free or another buf_load work correctly, and both do. */
int  buf_load(Buf *b, const char *data, long n);
/* serialized size (every line + CRLF) and serialization for save.
 * Memory-block helpers: used by the host test harness, NOT by the DOS
 * save path (a single malloc cannot exceed 64 KB in the 16-bit build). */
long buf_save_size(const Buf *b);
void buf_serialize(const Buf *b, char *out); /* writes exactly buf_save_size bytes */

/* stream I/O: no whole-file staging (a single allocation cannot exceed
 * 64 KB on the 16-bit DOS build). Same semantics as buf_load/serialize.
 * buf_load_stream: 1 ok, 0 oom/too-big/line-too-long/read-error.
 * buf_save_stream: 1 ok, 0 write error.
 * Difference from buf_load: the stream loader treats 0x1A as end-of-input
 * (stops reading at the FIRST 0x1A, rather than stripping one trailing
 * 0x1A from an in-memory block); for a text editor this is period-correct
 * DOS behavior. */
int buf_load_stream(Buf *b, FILE *f);
int buf_save_stream(const Buf *b, FILE *f);

/* editing (row 0-based, col 0..len; all return 1 ok, 0 refused/oom) */
int  buf_insert_char(Buf *b, int row, int col, char ch);
int  buf_delete_char(Buf *b, int row, int col);  /* Del at (row,col); joins lines at EOL */
int  buf_split_line(Buf *b, int row, int col);   /* Enter */

/* range = normalized selection, end exclusive on the column */
/* Caller must pass a normalized, in-bounds range: r1<=r2; each c within
 * its line's length; c1<=c2 when r1==r2. The core does not validate. */
typedef struct { int r1, c1, r2, c2; } Range;
/* extract range as CRLF-joined malloc'd string (caller frees); NULL on oom */
char *buf_get_range(const Buf *b, const Range *r);
int   buf_delete_range(Buf *b, const Range *r);
/* insert possibly-multi-line CRLF/LF text at (row,col); reports end position */
int   buf_insert_text(Buf *b, int row, int col, const char *text,
                      int *end_row, int *end_col);

/* search forward from just after (row,col); case-insensitive when fold=1.
 * 1 found (fr/fc set), 0 not found. Wraps to the top once. */
int  buf_find(const Buf *b, int row, int col, const char *needle, int fold,
              int *fr, int *fc);

#endif
