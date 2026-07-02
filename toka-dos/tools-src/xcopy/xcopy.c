/* XCOPY.C -- Toka-DOS project original implementation of the classic DOS
 * XCOPY command.
 *
 * Copyright (C) 2026 the Toka-DOS project.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, see <https://www.gnu.org/licenses/>.
 *
 * NOT a port: written from documented XCOPY command-line behavior only.
 * No FreeDOS or MS-DOS XCOPY source was consulted or copied -- see
 * toka-dos/msdos4/VENDOR.md for why porting the real MS-DOS 4.0 XCOPY was
 * investigated and rejected.
 *
 * Usage:
 *   XCOPY source [destination] [/S] [/E] [/P] [/V] [/W] [/Y] [/-Y]
 *
 *     source       file, directory, or wildcard to copy from.
 *     destination  file or directory to copy to; defaults to the current
 *                  directory. A trailing '\' or an existing directory means
 *                  "copy into this directory".
 *     /S           copy directories and subdirectories, except empty ones.
 *     /E           copy subdirectories even if empty (implies /S).
 *     /P           prompt "(Y/N)?" before creating each destination file.
 *     /V           accepted; sets the DOS verify-after-write flag (INT 21h
 *                  AH=2Eh) for the duration of the run.
 *     /W           display "Press any key to begin copying..." and wait for
 *                  a keystroke before doing anything else.
 *     /Y           overwrite existing destination files without prompting.
 *     /-Y          prompt before overwriting existing destination files
 *                  (this is also the default).
 *
 * Exit codes (the classic XCOPY set): 0 ok, 1 no files found to copy,
 * 4 initialization error (bad usage / out of memory / bad path), 5 disk
 * write error (user aborted an overwrite, or a write actually failed).
 *
 * Simplification, called out because real XCOPY is fuzzier here: real XCOPY
 * asks "(F = file, D = directory)?" when it cannot tell whether a
 * not-yet-existing destination should be a file or a directory. This
 * implementation skips that prompt: if the destination does not exist and
 * either /S or /E was given, or the source expands to more than one file,
 * the destination is treated as a directory (and created). Otherwise a
 * non-existent destination is treated as a file name for a single-file
 * copy. This covers the common cases (XCOPY A.TXT B.TXT, XCOPY SRC DEST /S)
 * without an extra interactive prompt.
 */

#include <dos.h>
#include <fcntl.h>
#include <conio.h>
#include <io.h>
#include <direct.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define TRUE  1
#define FALSE 0

#define EXIT_OK       0
#define EXIT_NOFILES  1
#define EXIT_INIT     4
#define EXIT_DISKERR  5

#define MAXPATH 128

/* ---- options ------------------------------------------------------- */

static int opt_s = FALSE;   /* /S: recurse, skip empty subdirs           */
static int opt_e = FALSE;   /* /E: recurse, include empty subdirs        */
static int opt_p = FALSE;   /* /P: confirm each source file              */
static int opt_v = FALSE;   /* /V: set DOS verify flag                   */
static int opt_w = FALSE;   /* /W: wait for a keypress before starting   */
static int opt_y = 0;       /* overwrite prompting: 0=default(prompt),
                                1=/Y (never prompt), -1=/-Y (always prompt) */

static unsigned long files_copied = 0;

/* Destination is a directory (existing, or created because we decided the
 * copy is directory-shaped -- see the /P style note above the file). */
static int dest_is_dir = FALSE;
static char dest_root[MAXPATH];

/* ---- small helpers --------------------------------------------------- */

static void out(const char *s)
{
    fputs(s, stdout);
}

/* Decimal-format an unsigned long by hand and print it, avoiding a
 * printf("%lu", ...) varargs call for a long argument (small-model DOS
 * printf's long handling is not worth relying on for one call site). */
static void out_ulong(unsigned long n)
{
    char buf[12]; /* max "4294967295" + NUL */
    char *p = buf + sizeof(buf) - 1;
    *p = '\0';
    do {
        *--p = (char)('0' + (n % 10));
        n /= 10;
    } while (n != 0);
    out(p);
}

/* Case-insensitive ASCII compare, DOS has no locale to speak of. */
static int samechar(char a, char b)
{
    if (a >= 'a' && a <= 'z') a = (char)(a - 'a' + 'A');
    if (b >= 'a' && b <= 'z') b = (char)(b - 'a' + 'A');
    return a == b;
}

static int sameswitch(const char *arg, const char *name)
{
    while (*name) {
        if (!*arg || !samechar(*arg, *name)) return FALSE;
        arg++; name++;
    }
    return *arg == '\0';
}

static int is_dir_attr(unsigned attr)
{
    return (attr & _A_SUBDIR) != 0;
}

static int path_exists(const char *path, unsigned *attr_out)
{
    unsigned attr;
    if (_dos_getfileattr(path, &attr) != 0) return FALSE;
    if (attr_out) *attr_out = attr;
    return TRUE;
}

/* Join dir + "\" + name into out (bounds-checked, MAXPATH). */
static void join_path(char *out, const char *dir, const char *name)
{
    size_t n = strlen(dir);
    if (n >= MAXPATH - 2) n = MAXPATH - 2;
    memcpy(out, dir, n);
    if (n > 0 && out[n - 1] != '\\' && out[n - 1] != ':') {
        out[n++] = '\\';
    }
    {
        size_t rem = MAXPATH - 1 - n;
        size_t m = strlen(name);
        if (m > rem) m = rem;
        memcpy(out + n, name, m);
        out[n + m] = '\0';
    }
}

/* Split "a\b\c" into dir="a\b" (in place, NUL where the last backslash
 * was) and return a pointer to "c". If there is no backslash/colon, dir is
 * left as "." and the whole string is returned as the leaf. */
static char *split_dir_leaf(char *path, char *dir_buf)
{
    char *slash = NULL;
    char *p;
    for (p = path; *p; p++) {
        if (*p == '\\' || *p == '/' || *p == ':') slash = p;
    }
    if (slash == NULL) {
        dir_buf[0] = '.';
        dir_buf[1] = '\0';
        return path;
    }
    {
        size_t n = (size_t)(slash - path) + (*slash == ':' ? 1 : 0);
        if (n == 0) n = 1; /* "\FOO" -> dir "\" */
        memcpy(dir_buf, path, n);
        dir_buf[n] = '\0';
    }
    return slash + 1;
}

/* mkdir() only makes one level; walk the path and create each missing
 * component. Ignores "already exists" failures. */
static int make_dirs(const char *path)
{
    char buf[MAXPATH];
    char *p;
    size_t n = strlen(path);
    if (n >= MAXPATH) n = MAXPATH - 1;
    memcpy(buf, path, n);
    buf[n] = '\0';

    for (p = buf; *p; p++) {
        if (*p == '\\' || *p == '/') {
            if (p != buf && *(p - 1) != ':') {
                char save = *p;
                *p = '\0';
                if (!path_exists(buf, NULL)) mkdir(buf);
                *p = save;
            }
        }
    }
    if (!path_exists(buf, NULL)) {
        if (mkdir(buf) != 0) return FALSE;
    }
    return TRUE;
}

/* Read a single Y/N answer from the keyboard, echoing it, and return TRUE
 * for yes. Anything other than a leading 'n'/'N' counts as yes, matching
 * classic XCOPY's "any key but N" prompt behavior. */
static int ask_yes(const char *prompt)
{
    int ch;
    out(prompt);
    ch = getche();
    out("\r\n");
    if (ch == 'n' || ch == 'N') return FALSE;
    return TRUE;
}

/* ---- copying a single file ------------------------------------------- */

static int copy_one_file(const char *src_path, const char *dst_path)
{
    int sh, dh;
    unsigned attr;
    unsigned date, time;
    char buf[8192];
    unsigned got, wrote;
    unsigned rc;

    if (opt_p) {
        char msg[MAXPATH + 16];
        sprintf(msg, "%s (Y/N)? ", src_path);
        if (!ask_yes(msg)) return TRUE; /* skipped, not an error */
    }

    if (path_exists(dst_path, NULL) && opt_y != 1) {
        if (opt_y == -1 || opt_y == 0) {
            char msg[MAXPATH + 32];
            sprintf(msg, "Overwrite %s (Y/N)? ", dst_path);
            if (!ask_yes(msg)) return TRUE; /* skipped, not an error */
        }
    }

    rc = _dos_open(src_path, O_RDONLY | O_BINARY, &sh);
    if (rc != 0) {
        fprintf(stderr, "Cannot open %s\n", src_path);
        return FALSE;
    }

    _dos_getfileattr(src_path, &attr);

    rc = _dos_creat(dst_path, attr & (_A_RDONLY | _A_HIDDEN | _A_SYSTEM | _A_ARCH), &dh);
    if (rc != 0) {
        fprintf(stderr, "Cannot create %s\n", dst_path);
        _dos_close(sh);
        return FALSE;
    }

    for (;;) {
        rc = _dos_read(sh, buf, sizeof(buf), &got);
        if (rc != 0 || got == 0) break;
        rc = _dos_write(dh, buf, got, &wrote);
        if (rc != 0 || wrote != got) {
            fprintf(stderr, "Disk write error writing %s\n", dst_path);
            _dos_close(sh);
            _dos_close(dh);
            return FALSE;
        }
    }

    /* Preserve the source's last-write date/time on the new file before
     * closing it (DOS wants the handle open for _dos_setftime). */
    if (_dos_getftime(sh, &date, &time) == 0) {
        _dos_setftime(dh, date, time);
    }

    _dos_close(sh);
    _dos_close(dh);

    /* Preserve attributes last, after the handle is closed (a read-only
     * attribute would otherwise block the write we just did). */
    _dos_setfileattr(dst_path, attr);

    printf("%s\n", dst_path);
    files_copied++;
    return TRUE;
}

/* ---- directory walk ---------------------------------------------------
 * Copies every file matching `mask` (e.g. "*.*") directly under `src_dir`
 * into `dst_dir` (created on demand), then, if recursing, walks each
 * subdirectory of src_dir the same way, appending its name to both sides
 * of the path. Empty subdirectories are only materialized on the
 * destination when opt_e is set. */

static int walk_dir(const char *src_dir, const char *dst_dir, const char *mask)
{
    struct find_t fi;
    char pattern[MAXPATH];
    char src_path[MAXPATH];
    char dst_path[MAXPATH];
    int made_dest = FALSE;
    unsigned rc;

    join_path(pattern, src_dir, mask);

    rc = _dos_findfirst(pattern, _A_RDONLY | _A_HIDDEN | _A_SYSTEM | _A_ARCH, &fi);
    while (rc == 0) {
        if (!(fi.attrib & _A_SUBDIR)) {
            if (!made_dest) {
                if (!make_dirs(dst_dir)) {
                    fprintf(stderr, "Unable to create directory %s\n", dst_dir);
                    return FALSE;
                }
                made_dest = TRUE;
            }
            join_path(src_path, src_dir, fi.name);
            join_path(dst_path, dst_dir, fi.name);
            if (!copy_one_file(src_path, dst_path)) return FALSE;
        }
        rc = _dos_findnext(&fi);
    }

    if (opt_e && !made_dest) {
        make_dirs(dst_dir);
        made_dest = TRUE;
    }

    if (opt_s || opt_e) {
        /* Scan with a directory-only mask over the whole dir, not the file
         * mask, so subdirectories are found regardless of `mask`. */
        join_path(pattern, src_dir, "*.*");
        rc = _dos_findfirst(pattern, _A_SUBDIR, &fi);
        while (rc == 0) {
            if ((fi.attrib & _A_SUBDIR) && strcmp(fi.name, ".") != 0 &&
                strcmp(fi.name, "..") != 0) {
                char sub_src[MAXPATH];
                char sub_dst[MAXPATH];
                join_path(sub_src, src_dir, fi.name);
                join_path(sub_dst, dst_dir, fi.name);
                if (!walk_dir(sub_src, sub_dst, mask)) return FALSE;
            }
            rc = _dos_findnext(&fi);
        }
    }

    return TRUE;
}

/* ---- argument parsing --------------------------------------------------
 * Classic XCOPY syntax has no long options and no '=' forms; every token
 * is either a switch starting with '/' or a positional (source then
 * destination) path/wildcard. */

static void usage_error(const char *why)
{
    fprintf(stderr, "Invalid syntax: %s\n", why);
    fprintf(stderr,
        "XCOPY source [destination] [/S] [/E] [/P] [/V] [/W] [/Y] [/-Y]\n");
    exit(EXIT_INIT);
}

int main(int argc, char **argv)
{
    const char *source = NULL;
    const char *dest = NULL;
    int i;
    char src_dir[MAXPATH];
    unsigned src_attr;
    int src_is_dir;
    int single_file_source;

    for (i = 1; i < argc; i++) {
        const char *a = argv[i];
        if (a[0] == '/') {
            const char *sw = a + 1;
            if (sameswitch(sw, "S")) opt_s = TRUE;
            else if (sameswitch(sw, "E")) opt_e = TRUE;
            else if (sameswitch(sw, "P")) opt_p = TRUE;
            else if (sameswitch(sw, "V")) opt_v = TRUE;
            else if (sameswitch(sw, "W")) opt_w = TRUE;
            else if (sameswitch(sw, "Y")) opt_y = 1;
            else if (sameswitch(sw, "-Y")) opt_y = -1;
            else usage_error(a);
        } else if (source == NULL) {
            source = a;
        } else if (dest == NULL) {
            dest = a;
        } else {
            usage_error(a);
        }
    }

    if (opt_e) opt_s = TRUE;

    if (source == NULL) usage_error("no source specified");

    if (opt_w) {
        out("Press any key to begin copying...");
        getch();
        out("\r\n");
    }

    if (opt_v) {
        union REGS regs;
        regs.h.ah = 0x2E;
        regs.h.al = 1;   /* turn verify on */
        regs.h.dl = 0;
        intdos(&regs, &regs);
    }

    /* Resolve the source. A bare directory name means "everything in it"
     * (equivalent to SOURCE\*.*); anything else is taken as a wildcard or
     * single-file pattern as typed. */
    {
        char work[MAXPATH];
        size_t n = strlen(source);
        if (n >= MAXPATH) n = MAXPATH - 1;
        memcpy(work, source, n);
        work[n] = '\0';

        src_is_dir = path_exists(work, &src_attr) && is_dir_attr(src_attr);
        strcpy(src_dir, work);
    }

    /* Figure out whether the source expands to a single file (drives
     * whether a not-yet-existing destination is treated as a file name or
     * a directory -- see the simplification note at the top of the file). */
    single_file_source = TRUE;
    if (src_is_dir || opt_s || opt_e) {
        single_file_source = FALSE;
    } else {
        struct find_t fi;
        char pat[MAXPATH];
        int count = 0;
        strcpy(pat, src_dir);
        if (_dos_findfirst(pat, _A_RDONLY | _A_HIDDEN | _A_SYSTEM | _A_ARCH, &fi) == 0) {
            count = 1;
            if (_dos_findnext(&fi) == 0) count = 2;
        }
        if (count != 1) single_file_source = FALSE;
    }

    /* Decide the destination. */
    if (dest == NULL) {
        dest_is_dir = TRUE;
        strcpy(dest_root, ".");
    } else {
        size_t dn = strlen(dest);
        char work[MAXPATH];
        size_t n = dn;
        if (n >= MAXPATH) n = MAXPATH - 1;
        memcpy(work, dest, n);
        work[n] = '\0';

        if (dn > 0 && (dest[dn - 1] == '\\' || dest[dn - 1] == '/')) {
            dest_is_dir = TRUE;
            /* Strip the trailing slash for our own path building; join_path
             * re-adds it as needed. */
            if (n > 1) work[n - 1] = '\0';
            strcpy(dest_root, work);
        } else {
            unsigned dattr;
            if (path_exists(work, &dattr) && is_dir_attr(dattr)) {
                dest_is_dir = TRUE;
                strcpy(dest_root, work);
            } else if (!single_file_source) {
                dest_is_dir = TRUE;
                strcpy(dest_root, work);
            } else {
                dest_is_dir = FALSE;
                strcpy(dest_root, work);
            }
        }
    }

    if (src_is_dir) {
        /* Directory source: copy its immediate contents (and, if /S or /E,
         * its subdirectories) into dest_root. */
        if (!walk_dir(src_dir, dest_root, "*.*")) {
            return EXIT_DISKERR;
        }
    } else if (single_file_source && !dest_is_dir) {
        /* Single file -> single file (or new file name). */
        struct find_t fi;
        if (_dos_findfirst(src_dir, _A_RDONLY | _A_HIDDEN | _A_SYSTEM | _A_ARCH, &fi) != 0) {
            fprintf(stderr, "File not found - %s\n", src_dir);
            return EXIT_NOFILES;
        }
        {
            char sdir_buf[MAXPATH];
            char full_src[MAXPATH];
            char work[MAXPATH];
            size_t n = strlen(src_dir);
            if (n >= MAXPATH) n = MAXPATH - 1;
            memcpy(work, src_dir, n);
            work[n] = '\0';
            split_dir_leaf(work, sdir_buf);
            join_path(full_src, sdir_buf, fi.name);
            if (!copy_one_file(full_src, dest_root)) return EXIT_DISKERR;
        }
    } else {
        /* Wildcard or multi-file source into a directory. */
        char sdir_buf[MAXPATH];
        char mask_buf[MAXPATH];
        char work[MAXPATH];
        char *leaf;
        size_t n = strlen(src_dir);
        if (n >= MAXPATH) n = MAXPATH - 1;
        memcpy(work, src_dir, n);
        work[n] = '\0';
        leaf = split_dir_leaf(work, sdir_buf);
        strcpy(mask_buf, leaf);
        if (mask_buf[0] == '\0') strcpy(mask_buf, "*.*");

        if (!walk_dir(sdir_buf, dest_root, mask_buf)) {
            return EXIT_DISKERR;
        }
    }

    if (files_copied == 0) {
        out("0 File(s) copied\n");
        return EXIT_NOFILES;
    }

    out_ulong(files_copied);
    out(" File(s) copied\n");
    return EXIT_OK;
}
