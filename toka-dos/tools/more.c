/* more.c - MORE: print a file one screenful (23 lines) at a time.
 *
 * Usage: MORE filename
 *
 * Reads the named file into a static buffer and prints it 23 lines at a
 * time. After every 23 lines it prints "-- More --" with no newline, waits
 * for a key, then continues. Toka-DOS has no shell redirection yet, so MORE
 * reads a named file rather than standard input.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 32768
static char buf[MAXBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char fname[80];
    int have_file = 0;
    int handle, total, i;
    int lines = 0;
    char *line;

    fname[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                fname[k++] = *p++;
            }
            fname[k] = 0;
            have_file = 1;
        }
    }

    if (!have_file) {
        t_putln("MORE: no file named");
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
            t_putln(line);
            lines++;
            if (last) {
                break;
            }
            if (lines == 23) {
                t_puts("-- More --");
                t_getkey();
                t_putln("");
                lines = 0;
            }
            line = &buf[i + 1];
        }
    }

    t_exit(0);
    return 0;
}
