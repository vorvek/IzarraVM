/* exe2bin.c - EXE2BIN: convert a DOS .EXE to a flat binary image.
 *
 * Usage: EXE2BIN source dest
 *
 * Reads the source file. If it begins with the MZ signature (4D 5A), the load
 * module starts past the header: the header size in 16-byte paragraphs is the
 * little-endian word at offset 8, so the module begins at that value times 16.
 * Everything from there to end of file is written to dest. A file without the
 * MZ signature is copied whole.
 */
#include <string.h>
#include "toka.h"

#define MAXBUF 32768
static char buf[MAXBUF];

int main(void)
{
    char *p = t_cmdtail();
    static char src[80];
    static char dst[80];
    int have_src = 0, have_dst = 0;
    int in, out, total, k;
    int headerbytes, modlen;

    src[0] = 0;
    dst[0] = 0;

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
            while (*p && *p != ' ' && *p != '\t') {
                p++;
            }
        }
    }

    if (!have_src || !have_dst) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    in = t_open(src, 0);
    if (in < 0) {
        t_putln("File not found");
        t_exit(1);
    }

    total = t_read(in, buf, MAXBUF);
    t_close(in);
    if (total < 0) {
        total = 0;
    }

    headerbytes = 0;
    if (total >= 10 && buf[0] == 0x4D && buf[1] == 0x5A) {
        /* Header size in 16-byte paragraphs is the word at offset 8. */
        headerbytes = ((unsigned char)buf[8] |
                       ((unsigned char)buf[9] << 8)) * 16;
        if (headerbytes > total) {
            headerbytes = total;
        }
    }

    modlen = total - headerbytes;

    out = t_create(dst);
    if (out < 0) {
        t_putln("File not found");
        t_exit(1);
    }
    if (modlen > 0) {
        t_write(out, buf + headerbytes, modlen);
    }
    t_close(out);

    t_puts("        ");
    t_putu((unsigned)modlen);
    t_putln(" bytes written");

    t_exit(0);
    return 0;
}
