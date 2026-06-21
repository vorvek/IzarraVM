/* icommand.c - the Toka-DOS command interpreter (ICOMMAND.COM).
 *
 * Sets 80x25 text mode at boot (TOKABOOT already did, and we leave its startup
 * line on screen), then runs a prompt loop. Internal commands run in process;
 * anything else is resolved on the C: drive and launched through EXEC, exactly
 * like a real COMMAND.COM. The prompt shows the current directory.
 *
 * This phase covers the filesystem and info built-ins. The environment commands
 * (SET/PATH/PROMPT) and batch processing arrive next.
 */
#include <string.h>
#include "toka.h"

#define LINE_MAX 128

static const char *VERSION_LINE = "Toka-DOS v3.0";

static char up(char c)
{
    if (c >= 'a' && c <= 'z') {
        return (char)(c - 'a' + 'A');
    }
    return c;
}

static int eqi(const char *tok, const char *lit)
{
    while (*tok && *lit) {
        if (up(*tok) != up(*lit)) {
            return 0;
        }
        tok++;
        lit++;
    }
    return *tok == 0 && *lit == 0;
}

static char *skip_ws(char *p)
{
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    return p;
}

/* Copy the first whitespace-delimited word of `src` into `word` (uppercased),
 * and return a pointer to the rest of `src`. */
static char *split_word(char *src, char *word, int max)
{
    int i = 0;
    src = skip_ws(src);
    while (*src && *src != ' ' && *src != '\t' && i < max - 1) {
        word[i++] = up(*src++);
    }
    word[i] = 0;
    return skip_ws(src);
}

static int has_wildcard(const char *s)
{
    while (*s) {
        if (*s == '*' || *s == '?') {
            return 1;
        }
        s++;
    }
    return 0;
}

/* Print an unsigned long in decimal (file sizes exceed 16 bits). */
static void put_ulong(unsigned long value)
{
    char buf[12];
    int i = 0;
    if (value == 0) {
        t_putc('0');
        return;
    }
    while (value > 0 && i < 12) {
        buf[i++] = (char)('0' + (int)(value % 10));
        value /= 10;
    }
    while (i > 0) {
        t_putc(buf[--i]);
    }
}

static unsigned long dword_at(const unsigned char *p)
{
    return (unsigned long)p[0] | ((unsigned long)p[1] << 8) |
           ((unsigned long)p[2] << 16) | ((unsigned long)p[3] << 24);
}

/* The DTA fields a find result fills: name at 0x1E, attribute at 0x15, size
 * dword at 0x1A. */
#define DTA_ATTR 0x15
#define DTA_SIZE 0x1A
#define DTA_NAME 0x1E

static void print_prompt(void)
{
    char cwd[64];
    t_getcwd(cwd);
    t_puts("C:\\");
    t_puts(cwd); /* no leading slash; empty at the root */
    t_puts(">");
}

static void cmd_dir(const char *arg)
{
    static unsigned char dta[43];
    char spec[80];
    char cwd[64];
    unsigned files = 0;

    strcpy(spec, arg[0] == 0 ? "*.*" : arg);
    t_getcwd(cwd);
    t_putln("");
    t_putln(" Volume in drive C is TOKA-DOS");
    t_puts(" Directory of C:\\");
    t_putln(cwd);
    t_putln("");

    if (t_findfirst(spec, 0x10, dta) != 0) {
        t_putln("File not found");
        return;
    }
    do {
        const char *name = (const char *)(dta + DTA_NAME);
        unsigned char attr = dta[DTA_ATTR];
        t_puts(name);
        if (attr & 0x10) {
            t_puts("    <DIR>");
        } else {
            t_puts("    ");
            put_ulong(dword_at(dta + DTA_SIZE));
        }
        t_putln("");
        files++;
    } while (t_findnext(dta) == 0);

    t_puts("    ");
    t_putu(files);
    t_putln(" file(s)");
}

static void cmd_type(const char *arg)
{
    char buf[512];
    int handle, n, i;
    if (arg[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    handle = t_open(arg, 0);
    if (handle < 0) {
        t_putln("File not found");
        return;
    }
    while ((n = t_read(handle, buf, sizeof buf)) > 0) {
        for (i = 0; i < n; i++) {
            t_putc(buf[i]);
        }
    }
    t_close(handle);
}

static void cmd_copy(char *arg)
{
    char src[80], dst[80], buf[512];
    int hs, hd, n;
    char *rest = split_word(arg, src, sizeof src);
    split_word(rest, dst, sizeof dst);
    if (src[0] == 0 || dst[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    hs = t_open(src, 0);
    if (hs < 0) {
        t_putln("File not found");
        return;
    }
    hd = t_create(dst);
    if (hd < 0) {
        t_close(hs);
        t_putln("Cannot create file");
        return;
    }
    while ((n = t_read(hs, buf, sizeof buf)) > 0) {
        t_write(hd, buf, n);
    }
    t_close(hs);
    t_close(hd);
    t_putln("        1 file(s) copied");
}

/* Copy the directory part of `spec` (through the last backslash) into `out`. */
static void dir_prefix(const char *spec, char *out)
{
    int last = -1, i;
    for (i = 0; spec[i]; i++) {
        if (spec[i] == '\\' || spec[i] == '/') {
            last = i;
        }
    }
    for (i = 0; i <= last; i++) {
        out[i] = spec[i];
    }
    out[last + 1] = 0;
}

static void cmd_del(const char *arg)
{
    static unsigned char dta[43];
    char prefix[80], path[96];
    if (arg[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    if (!has_wildcard(arg)) {
        if (t_delete(arg) != 0) {
            t_putln("File not found");
        }
        return;
    }
    /* The kernel snapshots the match list at find-first, so deleting while we
     * iterate find-next is safe. */
    dir_prefix(arg, prefix);
    if (t_findfirst(arg, 0, dta) != 0) {
        t_putln("File not found");
        return;
    }
    do {
        if (dta[DTA_ATTR] & 0x10) {
            continue; /* skip directories */
        }
        strcpy(path, prefix);
        strcat(path, (const char *)(dta + DTA_NAME));
        t_delete(path);
    } while (t_findnext(dta) == 0);
}

static void cmd_ren(char *arg)
{
    char oldn[80], newn[80];
    char *rest = split_word(arg, oldn, sizeof oldn);
    split_word(rest, newn, sizeof newn);
    if (oldn[0] == 0 || newn[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    if (t_rename(oldn, newn) != 0) {
        t_putln("Cannot rename");
    }
}

static void cmd_cd(const char *arg)
{
    char cwd[64];
    if (arg[0] == 0) {
        t_getcwd(cwd);
        t_puts("C:\\");
        t_putln(cwd);
        return;
    }
    if (t_chdir(arg) != 0) {
        t_putln("Invalid directory");
    }
}

static void cmd_md(const char *arg)
{
    if (arg[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    if (t_mkdir(arg) != 0) {
        t_putln("Unable to create directory");
    }
}

static void cmd_rd(const char *arg)
{
    if (arg[0] == 0) {
        t_putln("Required parameter missing");
        return;
    }
    if (t_rmdir(arg) != 0) {
        t_putln("Invalid path, not directory, or directory not empty");
    }
}

static void put2(int value)
{
    if (value < 10) {
        t_putc('0');
    }
    t_putu((unsigned)value);
}

static void cmd_date(void)
{
    static const char *days[7] = {"Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"};
    int year, month, day, dow;
    t_getdate(&year, &month, &day, &dow);
    t_puts("Current date is ");
    t_puts(days[dow % 7]);
    t_puts(" ");
    put2(month);
    t_puts("-");
    put2(day);
    t_puts("-");
    t_putu((unsigned)year);
    t_putln("");
}

static void cmd_time(void)
{
    int hour, minute, second;
    t_gettime(&hour, &minute, &second);
    t_puts("Current time is ");
    put2(hour);
    t_puts(":");
    put2(minute);
    t_puts(":");
    put2(second);
    t_putln("");
}

/* Run `name` as C:\NAME.COM then C:\NAME.EXE, passing `tail`. 1 if launched. */
static int run_external(const char *name, const char *tail)
{
    char path[24];
    strcpy(path, "C:\\");
    strncat(path, name, 12);
    strcat(path, ".COM");
    if (t_exec(path, tail) != 2) {
        return 1;
    }
    strcpy(path, "C:\\");
    strncat(path, name, 12);
    strcat(path, ".EXE");
    if (t_exec(path, tail) != 2) {
        return 1;
    }
    return 0;
}

static void dispatch(char *word, char *rest)
{
    if (eqi(word, "EXIT")) {
        t_exit(0);
    } else if (eqi(word, "VER")) {
        t_putln(VERSION_LINE);
    } else if (eqi(word, "CLS")) {
        t_cls();
    } else if (eqi(word, "ECHO")) {
        t_putln(rest);
    } else if (eqi(word, "REM")) {
        /* a comment: do nothing */
    } else if (eqi(word, "DIR")) {
        cmd_dir(rest);
    } else if (eqi(word, "TYPE")) {
        cmd_type(rest);
    } else if (eqi(word, "COPY")) {
        cmd_copy(rest);
    } else if (eqi(word, "DEL") || eqi(word, "ERASE")) {
        cmd_del(rest);
    } else if (eqi(word, "REN") || eqi(word, "RENAME")) {
        cmd_ren(rest);
    } else if (eqi(word, "CD") || eqi(word, "CHDIR")) {
        cmd_cd(rest);
    } else if (eqi(word, "MD") || eqi(word, "MKDIR")) {
        cmd_md(rest);
    } else if (eqi(word, "RD") || eqi(word, "RMDIR")) {
        cmd_rd(rest);
    } else if (eqi(word, "DATE")) {
        cmd_date();
    } else if (eqi(word, "TIME")) {
        cmd_time();
    } else if (eqi(word, "VOL")) {
        t_putln(" Volume in drive C is TOKA-DOS");
    } else if (eqi(word, "VERIFY")) {
        t_putln("VERIFY is off");
    } else {
        if (!run_external(word, rest)) {
            t_putln("Bad command or file name");
        }
    }
}

int main(void)
{
    char line[LINE_MAX];
    char word[16];
    char *rest;

    /* TOKABOOT already set 80x25 text mode and printed the startup line; print
     * below it rather than clearing. */
    t_putln("");

    for (;;) {
        print_prompt();
        t_getline(line, LINE_MAX);
        rest = split_word(line, word, sizeof word);
        if (word[0] == 0) {
            continue;
        }
        dispatch(word, rest);
    }
    /* not reached */
}
