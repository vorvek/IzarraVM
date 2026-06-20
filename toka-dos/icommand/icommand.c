/* icommand.c - the Toka-DOS command interpreter (ICOMMAND.COM).
 *
 * Skeleton for P0: set 80x25 text mode, show the version banner, then run a
 * prompt loop. A handful of internal commands run in process; anything else is
 * resolved on the C: drive and launched through EXEC, exactly like a real
 * COMMAND.COM. The full internal command set, PATH search, and batch processing
 * arrive in later phases.
 */
#include <string.h>
#include "toka.h"

#define LINE_MAX 128

static const char *VERSION_LINE = "Toka-DOS v3.0";

/* Uppercase a single ASCII letter. */
static char up(char c)
{
    if (c >= 'a' && c <= 'z') {
        return (char)(c - 'a' + 'A');
    }
    return c;
}

/* Case-insensitive compare of a NUL-terminated token against a literal. */
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

/* Skip leading spaces and tabs. */
static char *skip_ws(char *p)
{
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    return p;
}

/* Copy the first whitespace-delimited word of `src` into `word` (uppercased),
 * and return a pointer to the rest of `src` (past the word). */
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

/* Try to run `name` as an external C:\NAME.COM then C:\NAME.EXE, passing `tail`
 * as the command tail. Returns 1 if a program was launched, 0 if not found. */
static int run_external(const char *name, const char *tail)
{
    char path[24];
    int err;

    /* Build "C:\NAME.COM" with the name capped to 8.3 territory. */
    strcpy(path, "C:\\");
    strncat(path, name, 12);
    strcat(path, ".COM");
    err = t_exec(path, tail);
    if (err != 2) {
        return 1; /* launched, or failed for a reason other than not-found */
    }

    strcpy(path, "C:\\");
    strncat(path, name, 12);
    strcat(path, ".EXE");
    err = t_exec(path, tail);
    if (err != 2) {
        return 1;
    }
    return 0;
}

int main(void)
{
    char line[LINE_MAX];
    char word[16];
    char *rest;

    t_setmode3();
    t_putln(VERSION_LINE);
    t_putln("");

    for (;;) {
        t_puts("C:\\>");
        t_getline(line, LINE_MAX);
        rest = split_word(line, word, sizeof(word));

        if (word[0] == 0) {
            continue;
        }
        if (eqi(word, "EXIT")) {
            t_exit(0);
        } else if (eqi(word, "VER")) {
            t_putln(VERSION_LINE);
        } else if (eqi(word, "CLS")) {
            t_cls();
        } else if (eqi(word, "ECHO")) {
            t_putln(rest);
        } else {
            if (!run_external(word, rest)) {
                t_putln("Bad command or file name");
            }
        }
    }
    /* not reached */
}
