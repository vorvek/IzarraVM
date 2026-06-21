/* deltree.c - DELTREE: recursively delete a directory and all its contents.
 *
 * Usage: DELTREE dirname
 *
 * Walks the directory with t_findfirst/t_findnext, deleting files and
 * recursing into subdirectories. Each recursion level keeps its own find
 * buffer so the inner walk does not clobber the outer one.
 */
#include <string.h>
#include "toka.h"

/* Recursively empty `dir` of all files and subdirectories, then the caller
 * removes `dir` itself. Each level has its own 43-byte DTA. */
static void clear_dir(const char *dir)
{
    char dta[43];                 /* a separate DTA per recursion level */
    static char spec[160];
    static char path[160];
    char *name;
    unsigned char attr;

    /* Build the search spec: dir + "\" + "*.*" */
    strcpy(spec, dir);
    strcat(spec, "\\");
    strcat(spec, "*.*");

    if (t_findfirst(spec, 0x10, dta) != 0) {
        return;
    }

    do {
        name = (char *)dta + 0x1E;

        /* Skip the "." and ".." pseudo-entries. */
        if (name[0] == '.' &&
            (name[1] == 0 || (name[1] == '.' && name[2] == 0))) {
            continue;
        }

        /* Build the full path: dir + "\" + name */
        strcpy(path, dir);
        strcat(path, "\\");
        strcat(path, name);

        t_puts("Deleting ");
        t_putln(name);

        attr = ((unsigned char *)dta)[0x15];
        if (attr & 0x10) {
            clear_dir(path);
            t_rmdir(path);
        } else {
            t_delete(path);
        }
    } while (t_findnext(dta) == 0);
}

int main(void)
{
    char *p = t_cmdtail();
    static char dir[160];
    int k = 0;

    /* Skip leading whitespace in the command tail. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    if (*p == 0) {
        t_putln("Required parameter missing");
        t_exit(1);
    }

    /* Copy the directory name up to the next whitespace. */
    while (*p && *p != ' ' && *p != '\t' && k < 159) {
        dir[k++] = *p++;
    }
    dir[k] = 0;

    clear_dir(dir);
    t_rmdir(dir);

    t_exit(0);
    return 0;
}
