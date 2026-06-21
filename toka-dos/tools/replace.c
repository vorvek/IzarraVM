/* replace.c - REPLACE: copy the source file over the destination file.
 *
 * Usage: REPLACE source dest
 *   Opens the source for reading. If it cannot be opened, prints
 *   "File not found" and exits 1. Otherwise creates (truncates) the
 *   destination and copies the source bytes into it in a loop, then
 *   reports "        1 file(s) replaced".
 *
 * Built small model with Open Watcom and linked as a flat .COM, so the
 * stack is tiny: the copy buffer is static.
 */
#include <string.h>
#include "toka.h"

#define COPYBUF 8192
static char buf[COPYBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char src[80];
    static char dst[80];
    int have_src = 0, have_dst = 0;
    int sh, dh;
    int n, written;
    int k;

    src[0] = 0;
    dst[0] = 0;

    /* Pull the source and destination names out of the command tail. */
    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (!have_src) {
            k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                src[k++] = *p++;
            }
            src[k] = 0;
            have_src = 1;
        } else if (!have_dst) {
            k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                dst[k++] = *p++;
            }
            dst[k] = 0;
            have_dst = 1;
        } else {
            /* Skip any extra arguments. */
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have_src || !have_dst) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    sh = t_open(src, 0);
    if (sh < 0) {
        t_putln("File not found");
        t_exit(1);
    }

    dh = t_create(dst);
    if (dh < 0) {
        t_close(sh);
        t_putln("File not found");
        t_exit(1);
    }

    while ((n = t_read(sh, buf, COPYBUF)) > 0) {
        written = t_write(dh, buf, n);
        if (written < n) {
            break;
        }
    }

    t_close(sh);
    t_close(dh);

    t_puts("        ");
    t_putln("1 file(s) replaced");
    t_exit(0);
    return 0;
}
