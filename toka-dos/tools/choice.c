/* choice.c - CHOICE: prompt for one keypress and set the exit code.
 *
 * Usage: CHOICE [/C:keys] [text]
 *   /C:keys  the set of valid keys to accept (default "YN")
 *   text     an optional prompt message printed before the key list
 *
 * Prints the message (if any) then the keys in brackets, e.g. "[Y,N]?", with no
 * newline. Then it loops on t_getkey(), uppercases each key, and waits until one
 * matches the key set. On a match it echoes the chosen key, prints a newline, and
 * exits with the 1-based index of that key (DOS ERRORLEVEL convention: 1 for the
 * first key, 2 for the second, and so on). Non-matching keys are ignored.
 */
#include <string.h>
#include "toka.h"

static char keys[64];
static char text[128];

int main(void)
{
    char *p = t_cmdtail();
    int nkeys = 0;
    int ntext = 0;
    int i;
    int key;
    char ch;

    /* Skip leading whitespace. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    /* Parse an optional /C:keys switch. */
    if (p[0] == '/' && (p[1] == 'C' || p[1] == 'c') && p[2] == ':') {
        p += 3;
        while (*p && *p != ' ' && *p != '\t' && nkeys < 63) {
            ch = *p++;
            if (ch >= 'a' && ch <= 'z') {
                ch = (char)(ch - 'a' + 'A');
            }
            keys[nkeys++] = ch;
        }
        keys[nkeys] = 0;
        while (*p == ' ' || *p == '\t') {
            p++;
        }
    }

    /* Default key set when none was given. */
    if (nkeys == 0) {
        keys[0] = 'Y';
        keys[1] = 'N';
        keys[2] = 0;
        nkeys = 2;
    }

    /* The remaining tail is the prompt message. */
    while (*p && ntext < 127) {
        text[ntext++] = *p++;
    }
    text[ntext] = 0;

    /* Print the message (if any) then the keys in brackets. */
    if (ntext > 0) {
        t_puts(text);
    }
    t_putc('[');
    for (i = 0; i < nkeys; i++) {
        if (i > 0) {
            t_putc(',');
        }
        t_putc(keys[i]);
    }
    t_puts("]?");

    /* Loop until a valid key is pressed. */
    for (;;) {
        key = t_getkey();
        ch = (char)key;
        if (ch >= 'a' && ch <= 'z') {
            ch = (char)(ch - 'a' + 'A');
        }
        for (i = 0; i < nkeys; i++) {
            if (keys[i] == ch) {
                t_putc(ch);
                t_putc('\r');
                t_putc('\n');
                t_exit(i + 1);
            }
        }
    }
}
