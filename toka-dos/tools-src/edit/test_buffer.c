/* TokaEdit buffer core host test harness. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "buffer.h"

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

int main(void) {
    test_load_save_roundtrip();
    test_lf_only_tabs_and_eof_char();
    test_empty_buffer_is_one_line();
    puts("buffer core: OK");
    return 0;
}
