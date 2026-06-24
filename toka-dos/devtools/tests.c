/* tests.c - TESTS.COM: a small parameterized Toka-DOS debug tool.
 *
 * Usage: TESTS <CMD> [args]
 *
 * Commands:
 *   ENV <name> <expected>  Read this program's own environment, look up <name>,
 *                          and report a 0/1 result (0 = the value matches
 *                          <expected> ignoring case, 1 = mismatch or name
 *                          absent) through the Lotura unit-tester, which stops
 *                          the machine with that code. Used by the
 *                          env-propagation integration test.
 *   ENV <name>             Print the value of <name> to the screen (no
 *                          unit-tester call), for interactive debugging.
 *
 * Any other command prints a one-line usage and exits 0. New subcommands attach
 * the same way later.
 *
 * This is a .COM, small model: DS = PSP, so the command tail is read with
 * t_cmdtail (PSP:0x80) and the environment segment word is at PSP:0x2C.
 */
#include <string.h>
#include <conio.h>
#include <i86.h>
#include "toka.h"

/* Lotura unit-tester ports (see crates/izarravm-machine/src/unittester.rs). */
#define UT_INDEX    0xE4
#define UT_DATA     0xE5
#define UT_COMMAND  0xE6
#define UT_REG_EXIT 12
#define UT_CMD_EXIT 3

/* Stop the machine with `code` via the unit-tester: select the exit-code
 * register (index 12), write the byte, then run CMD_EXIT. */
static void ut_exit(unsigned char code)
{
    outp(UT_INDEX, UT_REG_EXIT);
    outp(UT_DATA, code);
    outp(UT_COMMAND, UT_CMD_EXIT);
    for (;;) {
        /* The run loop resolves CMD_EXIT before the next instruction, so this
         * never iterates; it just guarantees no fall-through. */
    }
}

/* Case-insensitive single-char compare (environment keys are uppercase). */
static int up_eq(char a, char b)
{
    if (a >= 'a' && a <= 'z') {
        a = (char)(a - 'a' + 'A');
    }
    if (b >= 'a' && b <= 'z') {
        b = (char)(b - 'a' + 'A');
    }
    return a == b;
}

/* Our own environment segment: in a .COM, DS = PSP, so the env paragraph is the
 * word at near offset 0x2C. */
static unsigned env_segment(void)
{
    return *(unsigned *)0x2C;
}

/* Find NAME= in the environment block at env:0 and return a far pointer to its
 * value, or a NULL far pointer if absent. The block is KEY=VALUE strings, each
 * NUL-terminated, ended by an empty string. */
static char far *env_find(unsigned env, const char *name)
{
    char far *p = (char far *)MK_FP(env, 0);
    int namelen = (int)strlen(name);
    while (*p) {
        int i = 0;
        while (i < namelen && p[i] && up_eq(p[i], name[i])) {
            i++;
        }
        if (i == namelen && p[i] == '=') {
            return p + namelen + 1;
        }
        while (*p) {
            p++;
        }
        p++; /* step past the NUL to the next entry */
    }
    return (char far *)0;
}

/* Case-insensitive compare of a far env value (NUL-terminated) against a near
 * expected string. */
static int far_value_eqi(char far *value, const char *expected)
{
    while (*expected) {
        if (!up_eq(*value, *expected)) {
            return 0;
        }
        value++;
        expected++;
    }
    return *value == 0;
}

/* Skip leading blanks. */
static char *skip_ws(char *p)
{
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    return p;
}

/* Copy the next whitespace-delimited word into `word`; return the rest. */
static char *next_word(char *p, char *word, int max)
{
    int i = 0;
    p = skip_ws(p);
    while (*p && *p != ' ' && *p != '\t' && i < max - 1) {
        word[i++] = *p++;
    }
    word[i] = 0;
    return skip_ws(p);
}

/* Case-insensitive whole-word compare. */
static int eqi(const char *a, const char *b)
{
    while (*a && *b) {
        if (!up_eq(*a, *b)) {
            return 0;
        }
        a++;
        b++;
    }
    return *a == 0 && *b == 0;
}

static void cmd_env(char *rest)
{
    char name[64];
    char expected[128];
    char far *value;
    rest = next_word(rest, name, sizeof name);
    next_word(rest, expected, sizeof expected);
    if (name[0] == 0) {
        t_putln("Usage: TESTS ENV <name> [<expected>]");
        t_exit(0);
        return;
    }
    value = env_find(env_segment(), name);
    if (expected[0] == 0) {
        /* Interactive form: print the value (or nothing) and exit. */
        if (value) {
            while (*value) {
                t_putc(*value++);
            }
        }
        t_putln("");
        t_exit(0);
        return;
    }
    /* Test form: report 0 on match, 1 on mismatch or absence. */
    if (value && far_value_eqi(value, expected)) {
        ut_exit(0);
    } else {
        ut_exit(1);
    }
}

int main(void)
{
    char *p = t_cmdtail();
    char word[16];
    char *rest = next_word(p, word, sizeof word);
    if (eqi(word, "ENV")) {
        cmd_env(rest);
    } else {
        t_putln("Usage: TESTS ENV <name> [<expected>]");
    }
    t_exit(0);
    return 0;
}
