/* comp.c - COMP: compare two files byte by byte.
 *
 * Usage: COMP file1 file2
 *   Reads each file into its own static buffer and compares them.
 *   If the lengths match and every byte is equal, prints "Files compare OK".
 *   Otherwise prints up to 10 mismatch lines of the form
 *     "Compare error at offset <decimal>"
 *   and exits 1. A length difference past the shorter file is reported as a
 *   mismatch at each trailing offset too.
 *
 * Toka-DOS has no shell redirection yet, so COMP takes two named files.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 16384

static char a[MAXBUF];
static char b[MAXBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char f1[80];
    static char f2[80];
    int have1 = 0, have2 = 0;
    int h1, h2;
    int n1, n2;
    int longer, i;
    int errors = 0;

    f1[0] = 0;
    f2[0] = 0;

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
                f1[k++] = *p++;
            }
            f1[k] = 0;
            have1 = 1;
        } else if (!have2) {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                f2[k++] = *p++;
            }
            f2[k] = 0;
            have2 = 1;
        } else {
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have1 || !have2) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    h1 = t_open(f1, 0);
    if (h1 < 0) {
        t_putln("File not found");
        t_exit(1);
    }
    h2 = t_open(f2, 0);
    if (h2 < 0) {
        t_close(h1);
        t_putln("File not found");
        t_exit(1);
    }

    n1 = t_read(h1, a, MAXBUF);
    n2 = t_read(h2, b, MAXBUF);
    t_close(h1);
    t_close(h2);
    if (n1 < 0) {
        n1 = 0;
    }
    if (n2 < 0) {
        n2 = 0;
    }

    longer = (n1 > n2) ? n1 : n2;
    for (i = 0; i < longer; i++) {
        int mismatch;
        if (i >= n1 || i >= n2) {
            mismatch = 1;
        } else {
            mismatch = (a[i] != b[i]);
        }
        if (mismatch) {
            t_puts("Compare error at offset ");
            t_putu((unsigned)i);
            t_putln("");
            errors++;
            if (errors >= 10) {
                break;
            }
        }
    }

    if (errors == 0) {
        t_putln("Files compare OK");
        t_exit(0);
    }

    t_exit(1);
    return 0;
}
