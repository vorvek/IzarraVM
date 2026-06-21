/* tree.c - TREE: print a directory structure as an indented tree.
 *
 * Usage: TREE [path]
 *   Starts at the given path, or the current directory when none is named.
 * Toka-DOS find never returns "." or "..", so the walk needs no special case
 * for those entries.
 */
#include <string.h>
#include "toka.h"

#define MAXDEPTH 8

static void indent(int depth)
{
    int n = 2 * depth;
    int i;
    for (i = 0; i < n; i++) {
        t_putc(' ');
    }
}

static void walk(const char *dir, int depth)
{
    char dta[43];                  /* find state is keyed by the dta address */
    char spec[160];
    char child[160];
    char *name;
    unsigned char attr;

    if (depth > MAXDEPTH) {
        return;
    }

    strcpy(spec, dir);
    strcat(spec, "\\*.*");

    if (t_findfirst(spec, 0x10, dta) != 0) {
        return;
    }
    do {
        attr = (unsigned char)dta[0x15];
        if (attr & 0x10) {
            name = &dta[0x1E];
            indent(depth);
            t_putln(name);
            strcpy(child, dir);
            strcat(child, "\\");
            strcat(child, name);
            walk(child, depth + 1);
        }
    } while (t_findnext(dta) == 0);
}

int main(void)
{
    char *p = t_cmdtail();
    char start[160];
    int k = 0;

    while (*p == ' ' || *p == '\t') {
        p++;
    }
    if (*p) {
        while (*p && *p != ' ' && *p != '\t' && k < 159) {
            start[k++] = *p++;
        }
        start[k] = 0;
    } else {
        t_getcwd(start);
    }

    t_putln(start);
    walk(start, 1);

    t_exit(0);
    return 0;
}
