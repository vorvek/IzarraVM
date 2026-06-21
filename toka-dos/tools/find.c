/* find.c - FIND: print lines of a file that contain a string.
 *
 * Usage: FIND [/V] [/C] [/N] "string" filename
 *   /V  print lines that do NOT contain the string
 *   /C  print only the count of matching lines
 *   /N  prefix each printed line with its line number
 *
 * Toka-DOS has no shell redirection yet, so FIND reads a named file rather than
 * standard input.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 16384
static char buf[MAXBUF];

int main(void)
{
    char *p = t_cmdtail();
    char needle[128];
    char fname[80];
    int vflag = 0, cflag = 0, nflag = 0;
    int have_needle = 0, have_file = 0;
    int handle, total, i, lineno = 0, matches = 0;
    char *line;

    needle[0] = 0;
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
            if (*p == 'V' || *p == 'v') {
                vflag = 1;
            } else if (*p == 'C' || *p == 'c') {
                cflag = 1;
            } else if (*p == 'N' || *p == 'n') {
                nflag = 1;
            }
            if (*p) {
                p++;
            }
        } else if (*p == '"') {
            int k = 0;
            p++;
            while (*p && *p != '"' && k < 127) {
                needle[k++] = *p++;
            }
            needle[k] = 0;
            have_needle = 1;
            if (*p == '"') {
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

    if (!have_needle) {
        t_putln("FIND: no search string in quotes");
        t_exit(1);
    }
    if (!have_file) {
        t_putln("FIND: no file named");
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
            lineno++;
            {
                int hit = strstr(line, needle) != 0;
                if (vflag) {
                    hit = !hit;
                }
                if (hit) {
                    matches++;
                    if (!cflag) {
                        if (nflag) {
                            t_putu((unsigned)lineno);
                            t_puts(": ");
                        }
                        t_putln(line);
                    }
                }
            }
            if (last) {
                break;
            }
            line = &buf[i + 1];
        }
    }

    if (cflag) {
        t_puts("--------- ");
        t_puts(fname);
        t_puts(": ");
        t_putu((unsigned)matches);
        t_putln("");
    }
    t_exit(matches > 0 ? 0 : 1);
    return 0;
}
