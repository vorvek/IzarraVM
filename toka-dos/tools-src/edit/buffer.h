/* TokaEdit text buffer core. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#ifndef BUFFER_H
#define BUFFER_H

#define BUF_MAX_LINE 255
#define BUF_MAX_LOAD 409600L /* 400 KB */

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
 * 8-col stops, strips one trailing 0x1A. 1 ok, 0 oom/too-big/line-too-long. */
int  buf_load(Buf *b, const char *data, long n);
/* serialized size (every line + CRLF) and serialization for save */
long buf_save_size(const Buf *b);
void buf_serialize(const Buf *b, char *out); /* writes exactly buf_save_size bytes */

/* editing (row 0-based, col 0..len; all return 1 ok, 0 refused/oom) */
int  buf_insert_char(Buf *b, int row, int col, char ch);
int  buf_delete_char(Buf *b, int row, int col);  /* Del at (row,col); joins lines at EOL */
int  buf_split_line(Buf *b, int row, int col);   /* Enter */

/* range = normalized selection, end exclusive on the column */
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
