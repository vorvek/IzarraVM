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

/* The master environment: a DOS-format block of KEY=VALUE strings, each NUL
 * terminated, ended by an empty string (a double NUL). ICOMMAND owns it and
 * seeds it. PATH is deliberately absent at boot, the way real COMMAND.COM leaves
 * it: AUTOEXEC.BAT sets it (the default config carries PATH=C:\DOS), and until
 * then external commands resolve from the current directory. SET/PATH/PROMPT
 * edit this block and the prompt reads it. (Children still inherit the boot
 * environment, so a SET after boot is not yet propagated into a launched
 * program.) */
static char master_env[2048] = "PROMPT=$p$g\0COMSPEC=C:\\ICOMMAND.COM\0";

static int key_matches(const char *entry, const char *name, int namelen)
{
    int i;
    for (i = 0; i < namelen; i++) {
        if (up(entry[i]) != up(name[i])) {
            return 0;
        }
    }
    return entry[namelen] == '=';
}

/* Return a pointer to the value of NAME in the environment, or NULL. */
static char *env_get(const char *name)
{
    char *p = master_env;
    int namelen = (int)strlen(name);
    while (*p) {
        if (key_matches(p, name, namelen)) {
            return p + namelen + 1;
        }
        p += strlen(p) + 1;
    }
    return 0;
}

/* Set NAME=VALUE in the environment. An empty value removes NAME. The key is
 * stored uppercased, the DOS convention. */
static void env_set(const char *name, const char *value)
{
    char tmp[2048];
    int t = 0, i;
    int namelen = (int)strlen(name);
    char *p = master_env;
    while (*p) {
        int entrylen = (int)strlen(p);
        if (!key_matches(p, name, namelen)) {
            memcpy(tmp + t, p, entrylen + 1);
            t += entrylen + 1;
        }
        p += entrylen + 1;
    }
    if (value[0]) {
        for (i = 0; i < namelen; i++) {
            tmp[t++] = up(name[i]);
        }
        tmp[t++] = '=';
        strcpy(tmp + t, value);
        t += (int)strlen(value) + 1;
    }
    tmp[t++] = 0; /* the terminating empty string */
    memcpy(master_env, tmp, t);
}

static void env_list(void)
{
    char *p = master_env;
    while (*p) {
        t_putln(p);
        p += strlen(p) + 1;
    }
}

/* Draw the prompt from the PROMPT variable, expanding the $ codes. Falls back to
 * $p$g (current path then '>') when PROMPT is unset. */
static void render_prompt(void)
{
    char cwd[64];
    char *p = env_get("PROMPT");
    if (!p || !*p) {
        t_puts("C:\\");
        t_getcwd(cwd);
        t_puts(cwd);
        t_puts(">");
        return;
    }
    while (*p) {
        if (*p == '$' && p[1]) {
            p++;
            switch (up(*p)) {
            case 'P':
                t_puts("C:\\");
                t_getcwd(cwd);
                t_puts(cwd);
                break;
            case 'G':
                t_putc('>');
                break;
            case 'L':
                t_putc('<');
                break;
            case 'B':
                t_putc('|');
                break;
            case 'N':
                t_putc('C');
                break;
            case 'Q':
                t_putc('=');
                break;
            case '$':
                t_putc('$');
                break;
            case '_':
                t_putc('\r');
                t_putc('\n');
                break;
            default:
                t_putc('$');
                t_putc(*p);
                break;
            }
            p++;
        } else {
            t_putc(*p++);
        }
    }
}

static void cmd_set(char *rest)
{
    char *eq;
    if (rest[0] == 0) {
        env_list();
        return;
    }
    eq = strchr(rest, '=');
    if (!eq) {
        char *value = env_get(rest);
        if (value) {
            t_puts(rest);
            t_puts("=");
            t_putln(value);
        } else {
            t_putln("Environment variable not defined");
        }
        return;
    }
    *eq = 0;
    env_set(rest, eq + 1);
}

static void cmd_path(char *rest)
{
    char *value;
    if (rest[0] == 0) {
        value = env_get("PATH");
        if (value && *value) {
            t_puts("PATH=");
            t_putln(value);
        } else {
            t_putln("No Path"); /* the DOS message when no path is set */
        }
        return;
    }
    env_set("PATH", rest);
}

static void cmd_prompt(char *rest)
{
    env_set("PROMPT", rest[0] ? rest : "$p$g");
}

/* Case-insensitive test that `s` begins with `prefix`. */
static int starts_ci(const char *s, const char *prefix)
{
    while (*prefix) {
        if (up(*s) != up(*prefix)) {
            return 0;
        }
        s++;
        prefix++;
    }
    return 1;
}

/* PATH=x and PROMPT=x are the no-space assignment forms of these internal
 * commands. Real COMMAND.COM treats PATH=x like PATH x, so an AUTOEXEC.BAT line
 * such as "PATH=C:\DOS" works, and a bare "PATH=" clears the search path. The
 * value is read from the whole line so it is not clipped by the 16-byte
 * command-word buffer that split_word fills. PATH goes straight through env_set
 * (an empty value removes the variable) rather than cmd_path, whose no-argument
 * path prints instead of clearing. Recognized at the prompt and as a top-level
 * batch line; the nested IF/FOR/CALL forms still take the spaced or SET spelling.
 * Returns 1 when it handled the line. */
static int run_assignment(char *line)
{
    char *p = skip_ws(line);
    if (starts_ci(p, "PATH=")) {
        env_set("PATH", p + 5);
        return 1;
    }
    if (starts_ci(p, "PROMPT=")) {
        cmd_prompt(p + 7);
        return 1;
    }
    return 0;
}

/* TRUENAME: print the fully qualified path of the argument. */
static void cmd_truename(const char *arg)
{
    char cwd[64];
    if (arg[0] == 0) {
        t_getcwd(cwd);
        t_puts("C:\\");
        t_putln(cwd);
        return;
    }
    if (arg[1] == ':') {
        /* already drive-qualified */
        t_putln(arg);
        return;
    }
    if (arg[0] == '\\' || arg[0] == '/') {
        t_puts("C:");
        t_putln(arg);
        return;
    }
    t_getcwd(cwd);
    t_puts("C:\\");
    if (cwd[0]) {
        t_puts(cwd);
        t_putc('\\');
    }
    t_putln(arg);
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

static void dispatch(char *word, char *rest);

/* ERRORLEVEL: the exit code of the last external program. */
static int errorlevel = 0;

/* --- Batch (.BAT) interpreter -----------------------------------------------
 * A batch file is loaded whole, split into lines, and run with a line index so
 * GOTO can jump. One level is supported (a batch that runs another .BAT does not
 * resume afterward; the depth guard keeps the state from being clobbered). */
#define BAT_MAX 8192
#define BAT_LINES 256

static char bat_text[BAT_MAX];
static char *bat_line[BAT_LINES];
static int bat_nlines;
static int bat_ip;
static int bat_echo;
static int bat_depth;
static char bat_arg[10][64]; /* %0..%9 */

static int small_atoi(const char *s)
{
    int value = 0;
    while (*s >= '0' && *s <= '9') {
        value = value * 10 + (*s - '0');
        s++;
    }
    return value;
}

/* Find "==" in `s`; return its index or -1. */
static int find_eqeq(const char *s)
{
    int i;
    for (i = 0; s[i]; i++) {
        if (s[i] == '=' && s[i + 1] == '=') {
            return i;
        }
    }
    return -1;
}

/* Expand %0..%9 and %VAR% (and %% to %) from `src` into `dst`. */
static void bat_expand(const char *src, char *dst)
{
    int d = 0;
    while (*src && d < 250) {
        if (*src == '%') {
            src++;
            if (*src == '%') {
                dst[d++] = '%';
                src++;
            } else if (*src >= '0' && *src <= '9') {
                const char *a = bat_arg[*src - '0'];
                src++;
                while (*a && d < 250) {
                    dst[d++] = *a++;
                }
            } else {
                char name[32];
                int k = 0;
                char *value;
                while (*src && *src != '%' && k < 31) {
                    name[k++] = *src++;
                }
                name[k] = 0;
                if (*src == '%') {
                    src++;
                }
                value = env_get(name);
                if (value) {
                    while (*value && d < 250) {
                        dst[d++] = *value++;
                    }
                }
            }
        } else {
            dst[d++] = *src++;
        }
    }
    dst[d] = 0;
}

static void bat_goto(const char *label)
{
    int i;
    if (*label == ':') {
        label++;
    }
    for (i = 0; i < bat_nlines; i++) {
        char *p = skip_ws(bat_line[i]);
        if (*p == ':') {
            char name[32];
            int k = 0;
            p++;
            while (p[k] && p[k] != ' ' && p[k] != '\t' && k < 31) {
                name[k] = p[k];
                k++;
            }
            name[k] = 0;
            if (eqi(name, label)) {
                bat_ip = i;
                return;
            }
        }
    }
    t_putln("Label not found");
    bat_ip = bat_nlines;
}

static void bat_echo_cmd(char *rest)
{
    if (eqi(rest, "ON")) {
        bat_echo = 1;
    } else if (eqi(rest, "OFF")) {
        bat_echo = 0;
    } else if (rest[0] == 0) {
        t_putln(bat_echo ? "ECHO is on" : "ECHO is off");
    } else if (rest[0] == '.') {
        t_putln(rest + 1);
    } else {
        t_putln(rest);
    }
}

static void bat_if(char *rest)
{
    int negate = 0, cond = 0, eq;
    char tok[40];
    char *p = split_word(rest, tok, sizeof tok);
    if (eqi(tok, "NOT")) {
        negate = 1;
        p = split_word(p, tok, sizeof tok);
    }
    if (eqi(tok, "ERRORLEVEL")) {
        char num[8];
        p = split_word(p, num, sizeof num);
        cond = errorlevel >= small_atoi(num);
    } else if (eqi(tok, "EXIST")) {
        char path[80];
        p = split_word(p, path, sizeof path);
        cond = t_exists(path);
    } else if ((eq = find_eqeq(tok)) >= 0) {
        /* s1==s2 with no spaces around ==. */
        char left[20];
        int i;
        for (i = 0; i < eq && i < 19; i++) {
            left[i] = tok[i];
        }
        left[i] = 0;
        cond = (strcmp(left, tok + eq + 2) == 0);
    }
    if (negate) {
        cond = !cond;
    }
    if (cond) {
        char word[16];
        char *r2 = split_word(p, word, sizeof word);
        dispatch(word, r2);
    }
}

static void bat_for(char *rest)
{
    char var, items[256], tok[8];
    char *p = skip_ws(rest);
    int n = 0;
    char *it, *cmd;
    if (p[0] == '%' && p[1] == '%') {
        var = p[2];
        p += 3;
    } else {
        return;
    }
    p = split_word(skip_ws(p), tok, sizeof tok); /* IN */
    p = skip_ws(p);
    if (*p != '(') {
        return;
    }
    p++;
    while (*p && *p != ')' && n < 255) {
        items[n++] = *p++;
    }
    items[n] = 0;
    if (*p == ')') {
        p++;
    }
    p = split_word(skip_ws(p), tok, sizeof tok); /* DO */
    cmd = skip_ws(p);
    it = items;
    for (;;) {
        char item[64], built[256], word[16];
        int k = 0, b = 0;
        char *c = cmd, *r2;
        it = skip_ws(it);
        while (*it && *it != ' ' && *it != '\t' && k < 63) {
            item[k++] = *it++;
        }
        item[k] = 0;
        if (k == 0) {
            break;
        }
        while (*c && b < 250) {
            if (c[0] == '%' && c[1] == '%' && c[2] == var) {
                int m = 0;
                while (item[m] && b < 250) {
                    built[b++] = item[m++];
                }
                c += 3;
            } else {
                built[b++] = *c++;
            }
        }
        built[b] = 0;
        r2 = split_word(built, word, sizeof word);
        dispatch(word, r2);
    }
}

static void bat_exec_line(char *raw)
{
    char expanded[256], word[16];
    char *p = skip_ws(raw);
    char *rest;
    int at = 0;
    if (*p == ':') {
        return; /* a label */
    }
    if (*p == '@') {
        at = 1;
        p = skip_ws(p + 1);
    }
    if (*p == 0) {
        return;
    }
    bat_expand(p, expanded);
    if (bat_echo && !at) {
        render_prompt();
        t_putln(expanded);
    }
    rest = split_word(expanded, word, sizeof word);
    if (eqi(word, "ECHO")) {
        bat_echo_cmd(rest);
    } else if (eqi(word, "REM")) {
        /* comment */
    } else if (eqi(word, "GOTO")) {
        bat_goto(rest);
    } else if (eqi(word, "IF")) {
        bat_if(rest);
    } else if (eqi(word, "FOR")) {
        bat_for(rest);
    } else if (eqi(word, "SHIFT")) {
        int i;
        for (i = 0; i < 9; i++) {
            strcpy(bat_arg[i], bat_arg[i + 1]);
        }
        bat_arg[9][0] = 0;
    } else if (eqi(word, "PAUSE")) {
        t_putln("Press any key to continue . . .");
        t_getkey();
    } else if (eqi(word, "CALL")) {
        char *r2 = split_word(rest, word, sizeof word);
        dispatch(word, r2);
    } else if (!run_assignment(expanded)) {
        dispatch(word, rest);
    }
}

/* Run a .BAT file at `path` with command-tail `tail` as %1..%9. */
static void run_batch(const char *path, const char *tail)
{
    int handle, total, i;
    char *t;

    if (bat_depth > 0) {
        /* One batch level: a nested .BAT runs but does not resume the caller. */
    }
    handle = t_open(path, 0);
    if (handle < 0) {
        return;
    }
    total = t_read(handle, bat_text, BAT_MAX - 1);
    t_close(handle);
    if (total < 0) {
        return;
    }
    bat_text[total] = 0;

    /* Split into NUL-terminated lines, stripping CR. */
    bat_nlines = 0;
    bat_line[bat_nlines++] = bat_text;
    for (i = 0; i < total; i++) {
        if (bat_text[i] == '\r') {
            bat_text[i] = 0;
        } else if (bat_text[i] == '\n') {
            bat_text[i] = 0;
            if (bat_nlines < BAT_LINES) {
                bat_line[bat_nlines++] = &bat_text[i + 1];
            }
        }
    }

    /* %0 = the batch name; %1.. = the tail words. */
    for (i = 0; i < 10; i++) {
        bat_arg[i][0] = 0;
    }
    strncpy(bat_arg[0], path, 63);
    t = (char *)tail;
    for (i = 1; i < 10; i++) {
        char w[64];
        t = split_word(t, w, sizeof w);
        strcpy(bat_arg[i], w);
        if (w[0] == 0) {
            break;
        }
    }

    bat_echo = 1;
    bat_depth++;
    for (bat_ip = 0; bat_ip < bat_nlines; bat_ip++) {
        bat_exec_line(bat_line[bat_ip]);
    }
    bat_depth--;
}

/* Join `dir` + `name` (up to 12 chars) + `ext` into `out` with exactly one
 * backslash between the directory and the file. `dir` may or may not end in a
 * backslash. `out` must hold at least 100 bytes (a 79-char dir plus 8.3 name). */
static void join_path(char *out, const char *dir, const char *name, const char *ext)
{
    int n = (int)strlen(dir);
    strcpy(out, dir);
    if (n == 0 || out[n - 1] != '\\') {
        out[n++] = '\\';
        out[n] = 0;
    }
    strncat(out, name, 12);
    strcat(out, ext);
}

/* Try to run `dir`\`name` as .COM, then .EXE, then .BAT (the DOS extension
 * order). Returns 1 if a match exists in `dir` (and is run, or its launch error
 * is reported through errorlevel); 0 if no such file exists there. */
static int try_dir(const char *dir, const char *name, const char *tail)
{
    char path[100];
    join_path(path, dir, name, ".COM");
    if (t_exec(path, tail) != 2) {
        errorlevel = t_lastexit();
        return 1;
    }
    join_path(path, dir, name, ".EXE");
    if (t_exec(path, tail) != 2) {
        errorlevel = t_lastexit();
        return 1;
    }
    join_path(path, dir, name, ".BAT");
    if (t_exists(path)) {
        run_batch(path, tail);
        return 1;
    }
    return 0;
}

/* Run `name` as an external .COM/.EXE/.BAT, searching the current directory
 * first (the DOS order, and the fallback that keeps commands resolvable when
 * PATH is empty) and then each semicolon-separated entry of PATH. Returns 1 if
 * something ran. Sets errorlevel from a launched program. */
static int run_external(const char *name, const char *tail)
{
    char dir[80];
    char *path;

    /* The current directory: "C:\" plus the cwd relative to the root. */
    strcpy(dir, "C:\\");
    t_getcwd(dir + 3);
    if (try_dir(dir, name, tail)) {
        return 1;
    }

    /* Then each ';'-separated PATH entry in turn. */
    path = env_get("PATH");
    while (path && *path) {
        int n = 0;
        while (*path && *path != ';') {
            if (n < (int)sizeof(dir) - 1) {
                dir[n++] = *path;
            }
            path++;
        }
        dir[n] = 0;
        if (n > 0 && try_dir(dir, name, tail)) {
            return 1;
        }
        if (*path == ';') {
            path++;
        }
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
    } else if (eqi(word, "SET")) {
        cmd_set(rest);
    } else if (eqi(word, "PATH")) {
        cmd_path(rest);
    } else if (eqi(word, "PROMPT")) {
        cmd_prompt(rest);
    } else if (eqi(word, "BREAK")) {
        t_putln(eqi(rest, "ON") ? "BREAK is on" : "BREAK is off");
    } else if (eqi(word, "CHCP")) {
        t_putln("Active code page: 437");
    } else if (eqi(word, "DOSSIZE")) {
        t_putln("Toka-DOS resident size: 16384 bytes");
    } else if (eqi(word, "TRUENAME")) {
        cmd_truename(rest);
    } else if (eqi(word, "LOADHIGH") || eqi(word, "LH") || eqi(word, "LOADFIX")) {
        /* No UMB model: just run the program that follows. */
        char w2[16];
        char *r2 = split_word(rest, w2, sizeof w2);
        if (w2[0]) {
            dispatch(w2, r2);
        }
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

    /* Run the startup batch if present, the way DOS does. */
    if (t_exists("C:\\AUTOEXEC.BAT")) {
        run_batch("C:\\AUTOEXEC.BAT", "");
    }

    for (;;) {
        render_prompt();
        t_getline(line, LINE_MAX);
        rest = split_word(line, word, sizeof word);
        if (word[0] == 0) {
            continue;
        }
        if (run_assignment(line)) {
            continue;
        }
        dispatch(word, rest);
    }
    /* not reached */
}
