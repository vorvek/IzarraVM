/* expand.c - EXPAND: copy a file from source to dest.
 *
 * Usage: EXPAND source dest
 *
 * Toka-DOS files are stored uncompressed, so "expanding" a file is just a
 * straight byte-for-byte copy. EXPAND opens the source for reading, creates
 * the destination, copies through a static buffer, and reports one file
 * expanded.
 */
#include <string.h>
#include "toka.h"

#define COPYBUF 16384
static char buf[COPYBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char source[80];
    static char dest[80];
    int have_source = 0, have_dest = 0;
    int src, dst, n;

    source[0] = 0;
    dest[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (!have_source) {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                source[k++] = *p++;
            }
            source[k] = 0;
            have_source = 1;
        } else if (!have_dest) {
            int k = 0;
            while (*p && *p != ' ' && *p != '\t' && k < 79) {
                dest[k++] = *p++;
            }
            dest[k] = 0;
            have_dest = 1;
        } else {
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have_source || !have_dest) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    src = t_open(source, 0);
    if (src < 0) {
        t_putln("File not found");
        t_exit(1);
    }

    dst = t_create(dest);
    if (dst < 0) {
        t_close(src);
        t_putln("File not found");
        t_exit(1);
    }

    while ((n = t_read(src, buf, COPYBUF)) > 0) {
        t_write(dst, buf, n);
    }

    t_close(src);
    t_close(dst);

    t_puts(source);
    t_puts(" -> ");
    t_putln(dest);
    t_putln("        1 file(s) expanded");

    t_exit(0);
    return 0;
}
