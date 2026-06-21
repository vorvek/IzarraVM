/* fc.c - FC: compare two text files byte for byte.
 *
 * Usage: FC file1 file2
 *   Reads both files into static buffers and compares them. Prints
 *   "FC: no differences encountered" and exits 0 when the files match
 *   exactly, otherwise prints "FC: files are different" and exits 1.
 *
 * Toka-DOS has no shell redirection yet, so FC names two files on the
 * command line rather than reading standard input.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 16384
static char buf1[MAXBUF];
static char buf2[MAXBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char fname1[80];
    static char fname2[80];
    int have1 = 0, have2 = 0;
    int handle, total1, total2, i;

    fname1[0] = 0;
    fname2[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (!have1) {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                fname1[k++] = *p++;
            }
            fname1[k] = 0;
            have1 = 1;
        } else if (!have2) {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                fname2[k++] = *p++;
            }
            fname2[k] = 0;
            have2 = 1;
        } else {
            /* Ignore any extra arguments. */
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have1 || !have2) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    handle = t_open(fname1, 0);
    if (handle < 0) {
        t_puts("File not found - ");
        t_putln(fname1);
        t_exit(1);
    }
    total1 = t_read(handle, buf1, MAXBUF);
    t_close(handle);
    if (total1 < 0) {
        total1 = 0;
    }

    handle = t_open(fname2, 0);
    if (handle < 0) {
        t_puts("File not found - ");
        t_putln(fname2);
        t_exit(1);
    }
    total2 = t_read(handle, buf2, MAXBUF);
    t_close(handle);
    if (total2 < 0) {
        total2 = 0;
    }

    if (total1 != total2) {
        t_putln("FC: files are different");
        t_exit(1);
    }

    for (i = 0; i < total1; i++) {
        if (buf1[i] != buf2[i]) {
            t_putln("FC: files are different");
            t_exit(1);
        }
    }

    t_putln("FC: no differences encountered");
    t_exit(0);
    return 0;
}