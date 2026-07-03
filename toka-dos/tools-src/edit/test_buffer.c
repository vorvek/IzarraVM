/* TokaEdit buffer core host test harness. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "buffer.h"

#ifdef BUF_TEST_ALLOC
/* Test-only allocation shim hooks, defined in buffer.c under the same
 * BUF_TEST_ALLOC guard. Not part of the public buffer.h API. */
extern void buf_test_alloc_reset(void);
extern void buf_test_alloc_fail_at(long n);
extern long buf_test_alloc_balance(void);
#endif

static Buf load(const char *bytes) {
    Buf b;
    assert(buf_init(&b));
    assert(buf_load(&b, bytes, (long)strlen(bytes)));
    return b;
}
static char *save(const Buf *b) {
    long n = buf_save_size(b);
    char *s = malloc((size_t)n + 1);
    buf_serialize(b, s);
    s[n] = 0;
    return s;
}

static void test_load_save_roundtrip(void) {
    Buf b = load("one\r\ntwo\r\n");
    char *s;
    assert(b.nlines == 2);
    assert(strcmp(b.lines[0], "one") == 0 && b.lens[0] == 3);
    s = save(&b);
    assert(strcmp(s, "one\r\ntwo\r\n") == 0);
    free(s); buf_free(&b);
}

static void test_lf_only_tabs_and_eof_char(void) {
    Buf b = load("a\tb\nx\n\x1a");
    char *s;
    assert(b.nlines == 2);
    assert(strcmp(b.lines[0], "a       b") == 0); /* tab -> col 8 */
    s = save(&b);
    assert(strcmp(s, "a       b\r\nx\r\n") == 0); /* CRLF out, no 0x1A */
    free(s); buf_free(&b);
}

static void test_empty_buffer_is_one_line(void) {
    Buf b = load("");
    char *s;
    assert(b.nlines == 1 && b.lens[0] == 0);
    s = save(&b);
    assert(strcmp(s, "\r\n") == 0); /* EDIT always terminates the last line */
    free(s); buf_free(&b);
}

static void test_insert_and_split(void) {
    Buf b = load("helo\r\n");
    assert(buf_insert_char(&b, 0, 3, 'l'));
    assert(strcmp(b.lines[0], "hello") == 0 && b.dirty);
    assert(buf_split_line(&b, 0, 2));          /* he | llo */
    assert(b.nlines == 2);
    assert(strcmp(b.lines[0], "he") == 0 && strcmp(b.lines[1], "llo") == 0);
    buf_free(&b);
}

static void test_delete_and_join(void) {
    Buf b = load("ab\r\ncd\r\n");
    assert(buf_delete_char(&b, 0, 0));         /* Del 'a' */
    assert(strcmp(b.lines[0], "b") == 0);
    assert(buf_delete_char(&b, 0, 1));         /* Del at EOL joins */
    assert(b.nlines == 1 && strcmp(b.lines[0], "bcd") == 0);
    buf_free(&b);
}

static void test_line_cap(void) {
    Buf b = load("");
    int i;
    for (i = 0; i < BUF_MAX_LINE; i++) assert(buf_insert_char(&b, 0, i, 'x'));
    assert(!buf_insert_char(&b, 0, BUF_MAX_LINE, 'x')); /* refused, not crash */
    assert(b.lens[0] == BUF_MAX_LINE);
    buf_free(&b);
}

static void test_range_get_delete_insert(void) {
    Buf b = load("alpha\r\nbeta\r\ngamma\r\n");
    Range r = {0, 2, 2, 3};                    /* "pha\r\nbeta\r\ngam" */
    char *cut;
    int er, ec;
    cut = buf_get_range(&b, &r);
    assert(cut && strcmp(cut, "pha\r\nbeta\r\ngam") == 0);
    assert(buf_delete_range(&b, &r));
    assert(b.nlines == 1 && strcmp(b.lines[0], "alma") == 0);
    assert(buf_insert_text(&b, 0, 2, cut, &er, &ec)); /* paste it back */
    assert(b.nlines == 3 && er == 2 && ec == 3);
    assert(strcmp(b.lines[0], "alpha") == 0);
    assert(strcmp(b.lines[1], "beta") == 0);
    assert(strcmp(b.lines[2], "gamma") == 0);
    free(cut); buf_free(&b);
}

static void test_single_line_range(void) {
    Buf b = load("abcdef\r\n");
    Range r = {0, 1, 0, 4};
    char *s = buf_get_range(&b, &r);
    assert(s && strcmp(s, "bcd") == 0);
    assert(buf_delete_range(&b, &r));
    assert(strcmp(b.lines[0], "aef") == 0);
    free(s); buf_free(&b);
}

static void test_too_many_lines_refused(void) {
    long count = 17000;
    char *big;
    long i;
    Buf b;
    big = malloc((size_t)count + 1);
    assert(big);
    for (i = 0; i < count; i++) big[i] = '\n';
    big[count] = 0;
    assert(buf_init(&b));
    assert(!buf_load(&b, big, count)); /* refused, not a crash */
    buf_free(&b); /* must not crash on the partially-grown buffer */
    free(big);
}

static void test_load_replaces_existing_content(void) {
    Buf b = load("first\r\nsecond\r\n");
    assert(buf_load(&b, "third\r\n", 7)); /* second load must win, not leak-append */
    assert(b.nlines == 1);
    assert(strcmp(b.lines[0], "third") == 0);
    buf_free(&b);
}

#ifdef BUF_TEST_ALLOC
static void test_insert_text_oom_atomic(void) {
    const char *original = "alpha\r\nbeta\r\ngamma\r\n";
    const char *paste = "X\nY\nZ"; /* multi-segment insert */
    long fail_n;

    for (fail_n = 1; fail_n <= 40; fail_n++) {
        Buf b = load(original);
        char *before;
        int er, ec;
        int rc;

        before = save(&b);
        buf_test_alloc_fail_at(fail_n);
        rc = buf_insert_text(&b, 1, 2, paste, &er, &ec);
        buf_test_alloc_reset();

        if (!rc) {
            char *after = save(&b);
            assert(strcmp(before, after) == 0); /* buffer untouched */
            free(after);
        }
        free(before);
        buf_free(&b);
        assert(buf_test_alloc_balance() == 0); /* no leak on this iteration */
    }
}
#endif

/* tmpfile() opens in TEXT mode on this host CRT, which mangles 0x1A and
 * CRLF bytes on readback; the stream I/O under test is byte-exact and
 * must be tested through a BINARY-mode temp file instead. Callers close
 * with close_tmp_binary(), which also removes the backing file. */
static char tmp_name[L_tmpnam];
static FILE *open_tmp_binary(void) {
    FILE *f;
    assert(tmpnam(tmp_name));
    f = fopen(tmp_name, "wb+");
    assert(f);
    return f;
}
static void close_tmp_binary(FILE *f) {
    fclose(f);
    remove(tmp_name);
}

static void assert_bufs_equal(const Buf *a, const Buf *b) {
    int i;
    assert(a->nlines == b->nlines);
    for (i = 0; i < a->nlines; i++) {
        assert(a->lens[i] == b->lens[i]);
        assert(memcmp(a->lines[i], b->lines[i], (size_t)a->lens[i]) == 0);
    }
}

static void test_stream_roundtrip(void) {
    Buf a, c;
    FILE *tf;
    char *big;
    long i, col;

    /* small multi-line case with tab-expanded content */
    a = load("one\r\ntwo\tthree\r\n\r\nlast line no newline");
    tf = open_tmp_binary();
    assert(buf_save_stream(&a, tf));
    rewind(tf);
    assert(buf_init(&c));
    assert(buf_load_stream(&c, tf));
    assert_bufs_equal(&a, &c);
    buf_free(&c);
    close_tmp_binary(tf);
    buf_free(&a);

    /* >70,000 bytes total: 300 lines x 250 chars, pins streaming + byte
     * counting past what a single 16-bit malloc could ever stage. */
    big = malloc((size_t)(300 * 251) + 1);
    assert(big);
    col = 0;
    for (i = 0; i < 300; i++) {
        int j;
        for (j = 0; j < 250; j++) big[col++] = (char)('A' + (j % 26));
        big[col++] = '\n';
    }
    big[col] = 0;
    assert(buf_init(&a));
    assert(buf_load(&a, big, col));
    free(big);

    tf = open_tmp_binary();
    assert(buf_save_stream(&a, tf));
    rewind(tf);
    assert(buf_init(&c));
    assert(buf_load_stream(&c, tf));
    assert_bufs_equal(&a, &c);
    buf_free(&c);
    close_tmp_binary(tf);
    buf_free(&a);
}

static void test_stream_stops_at_eof_char(void) {
    FILE *tf = open_tmp_binary();
    Buf b;
    fwrite("abc\r\n\x1a" "def\r\n", 1, 11, tf);
    rewind(tf);
    assert(buf_init(&b));
    assert(buf_load_stream(&b, tf));
    assert(b.nlines == 1);
    assert(strcmp(b.lines[0], "abc") == 0);
    buf_free(&b);
    close_tmp_binary(tf);
}

static void test_stream_refuses_too_large(void) {
    FILE *tf = open_tmp_binary();
    Buf b;
    char chunk[1024];
    long written = 0;
    memset(chunk, 'x', sizeof chunk);
    while (written < 410000L) {
        fwrite(chunk, 1, sizeof chunk, tf);
        written += (long)sizeof chunk;
    }
    rewind(tf);
    assert(buf_init(&b));
    assert(!buf_load_stream(&b, tf)); /* refused, not a crash */
    buf_free(&b);
    close_tmp_binary(tf);
}

static void test_range_too_large_refused(void) {
    /* > BUF_MAX_RANGE extracted in one block would truncate through the
     * DOS build's 16-bit size_t in malloc; buf_get_range must refuse. */
    Buf b;
    Range r;
    int i;
    char line[201];
    assert(buf_init(&b));
    memset(line, 'y', 200);
    line[200] = 0;
    for (i = 0; i < 350; i++) {           /* ~350 * 202 bytes > 60000 */
        int er, ec;
        assert(buf_insert_text(&b, b.nlines - 1, 0, line, &er, &ec));
        assert(buf_split_line(&b, b.nlines - 1, 200));
    }
    r.r1 = 0; r.c1 = 0; r.r2 = b.nlines - 1; r.c2 = 0;
    assert(buf_get_range(&b, &r) == NULL); /* refused, not truncated */
    r.r2 = 2; r.c2 = 5;                    /* small range still works */
    {
        char *s = buf_get_range(&b, &r);
        assert(s != NULL);
        free(s);
    }
    buf_free(&b);
}

static void test_find(void) {
    Buf b = load("The cat\r\nsat on the CAT\r\n");
    int fr, fc;
    assert(buf_find(&b, 0, 0, "cat", 1, &fr, &fc) && fr == 0 && fc == 4);
    assert(buf_find(&b, fr, fc, "cat", 1, &fr, &fc) && fr == 1 && fc == 11);
    assert(buf_find(&b, fr, fc, "cat", 1, &fr, &fc) && fr == 0 && fc == 4); /* wrap */
    assert(!buf_find(&b, 0, 0, "dog", 1, &fr, &fc));
    assert(!buf_find(&b, 0, 0, "CAT", 0, &fr, &fc) || fr == 1); /* case-sensitive */
    buf_free(&b);
}

int main(void) {
    test_load_save_roundtrip();
    test_lf_only_tabs_and_eof_char();
    test_empty_buffer_is_one_line();
    test_insert_and_split();
    test_delete_and_join();
    test_line_cap();
    test_range_get_delete_insert();
    test_single_line_range();
    test_too_many_lines_refused();
    test_load_replaces_existing_content();
#ifdef BUF_TEST_ALLOC
    test_insert_text_oom_atomic();
#endif
    test_stream_roundtrip();
    test_stream_stops_at_eof_char();
    test_stream_refuses_too_large();
    test_range_too_large_refused();
    test_find();
#ifdef BUF_TEST_ALLOC
    assert(buf_test_alloc_balance() == 0);
#endif
    puts("buffer core: OK");
    return 0;
}
