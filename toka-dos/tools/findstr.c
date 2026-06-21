/* findstr.c - FINDSTR: print lines of a file that contain a string.
 *
 * Usage: FINDSTR [/I] [/N] "string" filename
 *   /I  case-insensitive match (compare uppercased copies)
 *   /N  prefix each printed line with its 1-based line number
 *
 * Like FIND, this reads a named file (Toka-DOS has no stdin redirection yet)
 * and prints every line that contains the search string.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 16384
static char buf[MAXBUF];
static char upline[1024];
static char upneedle[128];

static char up(char c)
{
    if (c >= 'a' && c <= 'z') {
        return (char)(c - 'a' + 'A');
    }
    return c;
}

static void upcopy(char *dst, const char *src, int max)
{
    int k = 0;
    while (src[k] && k < max - 1) {
        dst[k] = up(src[k]);
        k++;
    }
    dst[k] = 0;
}

int main(void)
{
    char *p = t_cmdtail();
    char needle[128];
    char fname[80];
    int iflag = 0, nflag = 0;
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
            if (*p == 'I' || *p == 'i') {
                iflag = 1;
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
        t_putln("FINDSTR: no search string in quotes");
        t_exit(1);
    }
    if (!have_file) {
        t_putln("FINDSTR: no file named");
        t_exit(1);
    }

    if (iflag) {
        upcopy(upneedle, needle, (int)sizeof(upneedle));
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
                int hit;
                if (iflag) {
                    upcopy(upline, line, (int)sizeof(upline));
                    hit = strstr(upline, upneedle) != 0;
                } else {
                    hit = strstr(line, needle) != 0;
                }
                if (hit) {
                    matches++;
                    if (nflag) {
                        t_putu((unsigned)lineno);
                        t_puts(": ");
                    }
                    t_putln(line);
                }
            }
            if (last) {
                break;
            }
            line = &buf[i + 1];
        }
    }

    t_exit(matches > 0 ? 0 : 1);
    return 0;
}
