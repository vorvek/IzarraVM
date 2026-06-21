/* fasthelp.c - FASTHELP: terse one-line help for common Toka-DOS commands.
 *
 * Usage: FASTHELP [command]
 *   With no argument, list every command name and its one-line summary.
 *   With a command argument, print just that summary, or a not-found note.
 */
#include <string.h>
#include "toka.h"

struct helpentry {
    const char *name;
    const char *summary;
};

static struct helpentry table[] = {
    { "DIR",      "Lists files in a directory" },
    { "CD",       "Changes the current directory" },
    { "MD",       "Creates a new directory" },
    { "RD",       "Removes an empty directory" },
    { "COPY",     "Copies one or more files" },
    { "DEL",      "Deletes one or more files" },
    { "REN",      "Renames a file" },
    { "TYPE",     "Shows the contents of a text file" },
    { "FIND",     "Searches a file for a text string" },
    { "CLS",      "Clears the screen" },
    { "DATE",     "Shows or sets the system date" },
    { "TIME",     "Shows or sets the system time" },
    { "VER",      "Shows the Toka-DOS version" },
    { "MEM",      "Reports free and used memory" },
    { "EXIT",     "Leaves the command shell" }
};

#define NENTRIES ((int)(sizeof(table) / sizeof(table[0])))

/* Compare two command names without regard to letter case. Returns 0 when
 * they match, the way strcmp does. */
static int eqi(const char *a, const char *b)
{
    char ca, cb;
    while (*a && *b) {
        ca = *a;
        cb = *b;
        if (ca >= 'a' && ca <= 'z') {
            ca = (char)(ca - 'a' + 'A');
        }
        if (cb >= 'a' && cb <= 'z') {
            cb = (char)(cb - 'a' + 'A');
        }
        if (ca != cb) {
            return 1;
        }
        a++;
        b++;
    }
    return (*a == 0 && *b == 0) ? 0 : 1;
}

/* Print a name padded to a fixed width, then its summary, on one line. */
static void print_entry(const struct helpentry *e)
{
    int len, i;
    t_puts(e->name);
    len = (int)strlen(e->name);
    for (i = len; i < 9; i++) {
        t_putc(' ');
    }
    t_putln(e->summary);
}

int main(void)
{
    char *p = t_cmdtail();
    char arg[32];
    int k = 0;
    int i;

    arg[0] = 0;

    /* Pull the first whitespace-delimited word from the command tail. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    while (*p && *p != ' ' && *p != '\t' && k < 31) {
        arg[k++] = *p++;
    }
    arg[k] = 0;

    if (arg[0] == 0) {
        for (i = 0; i < NENTRIES; i++) {
            print_entry(&table[i]);
        }
        t_exit(0);
        return 0;
    }

    for (i = 0; i < NENTRIES; i++) {
        if (eqi(arg, table[i].name) == 0) {
            t_putln(table[i].summary);
            t_exit(0);
            return 0;
        }
    }

    t_putln("No help available");
    t_exit(1);
    return 0;
}
