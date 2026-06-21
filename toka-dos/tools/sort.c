/* sort.c - SORT: read a file, sort its lines, print them.
 *
 * Usage: SORT [/R] filename
 *   /R  sort in reverse (descending) order
 *
 * Toka-DOS has no shell redirection yet, so SORT reads a named file rather
 * than standard input. The file is read into a static 16384-byte buffer,
 * split into lines (newline bytes become NUL, a trailing carriage return is
 * stripped), and up to 512 line pointers are collected, sorted with strcmp,
 * and printed one per line.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 16384
#define MAXLINES 512

static char buf[MAXBUF];
static char *lines[MAXLINES];

int main(void)
{
    char *p = t_cmdtail();
    char fname[80];
    int rflag = 0, have_file = 0;
    int handle, total, i, j, nlines = 0;
    char *line;
    char *tmp;
    int cmp;

    fname[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (*p == '/') {
            p++;
            if (*p == 'R' || *p == 'r') {
                rflag = 1;
            }
            if (*p) {
                p++;
            }
        } else {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                fname[k++] = *p++;
            }
            fname[k] = 0;
            have_file = 1;
        }
    }

    if (!have_file) {
        t_putln("SORT: no file named");
        t_exit(1);
    }

    handle = t_open(fname, 0);
    if (handle < 0) {
        t_puts("File not found - ");
        t_putln(fname);
        t_exit(1);
    }
    total = t_read(handle, buf, MAXBUF - 1);
    t_close(handle);
    if (total < 0) {
        total = 0;
    }
    buf[total] = 0;

    line = buf;
    for (i = 0; i <= total; i++) {
        if (buf[i] == '\n' || buf[i] == 0) {
            int last = (buf[i] == 0);
            if (i > 0 && buf[i - 1] == '\r') {
                buf[i - 1] = 0;
            }
            buf[i] = 0;
            if (nlines < MAXLINES) {
                lines[nlines++] = line;
            }
            if (last) {
                break;
            }
            line = &buf[i + 1];
        }
    }

    /* Drop a trailing empty line produced by a final newline. */
    if (nlines > 0 && lines[nlines - 1][0] == 0) {
        nlines--;
    }

    for (i = 0; i < nlines - 1; i++) {
        for (j = 0; j < nlines - 1 - i; j++) {
            cmp = strcmp(lines[j], lines[j + 1]);
            if (rflag) {
                cmp = -cmp;
            }
            if (cmp > 0) {
                tmp = lines[j];
                lines[j] = lines[j + 1];
                lines[j + 1] = tmp;
            }
        }
    }

    for (i = 0; i < nlines; i++) {
        t_putln(lines[i]);
    }

    t_exit(0);
    return 0;
}
