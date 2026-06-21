/* xcopy.c - XCOPY: copy files matching a filespec into a target directory.
 *
 * Usage: XCOPY source destdir [/S]
 *   source   a filespec in the current directory, may contain wildcards
 *   destdir  the target directory, created if it does not exist
 *   /S       best-effort recursion into subdirectories
 *
 * For each matching file (not a directory), the file is copied to
 * destdir\name and its name is printed. At the end the number of files
 * copied is printed.
 */
#include <string.h>
#include "toka.h"

#define COPYBUF 8192
static char copybuf[COPYBUF];

/* Copy a single file from srcpath to dstpath. Returns 0 on success. */
static int copy_one(const char *srcpath, const char *dstpath)
{
    int in, out, n, w;

    in = t_open(srcpath, 0);
    if (in < 0) {
        return 1;
    }
    out = t_create(dstpath);
    if (out < 0) {
        t_close(in);
        return 1;
    }
    for (;;) {
        n = t_read(in, copybuf, COPYBUF);
        if (n <= 0) {
            break;
        }
        w = t_write(out, copybuf, n);
        if (w != n) {
            t_close(in);
            t_close(out);
            return 1;
        }
    }
    t_close(in);
    t_close(out);
    return (n < 0) ? 1 : 0;
}

/* Join dir + '\\' + name into out. */
static void join_path(char *out, const char *dir, const char *name)
{
    int len;

    strcpy(out, dir);
    len = (int)strlen(out);
    if (len > 0 && out[len - 1] != '\\' && out[len - 1] != '/') {
        out[len++] = '\\';
        out[len] = 0;
    }
    strcat(out, name);
}

/* Split a filespec into its directory part (with trailing separator kept in
 * dir if present, else empty) and the pattern part. */
static void split_spec(const char *spec, char *dir, char *pat)
{
    const char *slash;
    const char *s;
    int n;

    slash = 0;
    for (s = spec; *s; s++) {
        if (*s == '\\' || *s == '/') {
            slash = s;
        }
    }
    if (slash) {
        n = (int)(slash - spec) + 1;
        memcpy(dir, spec, (unsigned)n);
        dir[n] = 0;
        strcpy(pat, slash + 1);
    } else {
        dir[0] = 0;
        strcpy(pat, spec);
    }
}

/* Recursively copy. srcdir is the source directory prefix (may be empty or end
 * with a separator), pat is the filename pattern, dst is the destination
 * directory. Returns the number of files copied. Each recursion level owns its
 * own DTA buffers. */
static unsigned copy_tree(const char *srcdir, const char *pat,
                          const char *dst, int recurse)
{
    char dta[43];
    char srcpath[128];
    char dstpath[128];
    char *name;
    int attr;
    unsigned count;

    count = 0;

    /* Copy the matching files in this directory. */
    {
        char spec[128];
        join_path(spec, srcdir[0] ? srcdir : ".", pat);
        if (t_findfirst(spec, 0x10, dta) == 0) {
            do {
                name = (char *)dta + 0x1E;
                attr = *((unsigned char *)dta + 0x15);
                if (attr & 0x10) {
                    continue;
                }
                join_path(srcpath, srcdir[0] ? srcdir : ".", name);
                join_path(dstpath, dst, name);
                if (copy_one(srcpath, dstpath) == 0) {
                    t_putln(name);
                    count++;
                }
            } while (t_findnext(dta) == 0);
        }
    }

    if (!recurse) {
        return count;
    }

    /* Walk subdirectories. Separate DTA from the file scan above. */
    {
        char dta2[43];
        char subspec[128];
        char subsrc[128];
        char subdst[128];

        join_path(subspec, srcdir[0] ? srcdir : ".", "*.*");
        if (t_findfirst(subspec, 0x10, dta2) == 0) {
            do {
                name = (char *)dta2 + 0x1E;
                attr = *((unsigned char *)dta2 + 0x15);
                if (!(attr & 0x10)) {
                    continue;
                }
                if (name[0] == '.' &&
                    (name[1] == 0 || (name[1] == '.' && name[2] == 0))) {
                    continue;
                }
                join_path(subsrc, srcdir[0] ? srcdir : ".", name);
                join_path(subdst, dst, name);
                t_mkdir(subdst);
                count += copy_tree(subsrc, pat, subdst, recurse);
            } while (t_findnext(dta2) == 0);
        }
    }

    return count;
}

int main(void)
{
    char *p = t_cmdtail();
    char source[128];
    char dest[128];
    char srcdir[128];
    char pat[80];
    int have_source = 0, have_dest = 0, sflag = 0;
    unsigned count;

    source[0] = 0;
    dest[0] = 0;

    while (*p) {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == 0) {
            break;
        }
        if (*p == '/') {
            p++;
            if (*p == 'S' || *p == 's') {
                sflag = 1;
            }
            if (*p) {
                p++;
            }
        } else {
            int k = 0;
            if (!have_source) {
                while (*p && *p != ' ' && *p != '\t' && k < 127) {
                    source[k++] = *p++;
                }
                source[k] = 0;
                have_source = 1;
            } else if (!have_dest) {
                while (*p && *p != ' ' && *p != '\t' && k < 127) {
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
    }

    if (!have_source) {
        t_putln("XCOPY: no source given");
        t_exit(1);
    }
    if (!have_dest) {
        t_putln("XCOPY: no destination directory given");
        t_exit(1);
    }

    /* Create the destination directory; ignore an already-exists error. */
    t_mkdir(dest);

    split_spec(source, srcdir, pat);

    count = copy_tree(srcdir, pat, dest, sflag);

    t_puts("        ");
    t_putu(count);
    t_putln(" file(s) copied");

    t_exit(0);
    return 0;
}
