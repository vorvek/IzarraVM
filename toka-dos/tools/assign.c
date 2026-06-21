/* assign.c - ASSIGN: cosmetic drive-letter redirection.
 *
 * On real DOS, ASSIGN redirected one drive letter to another. Toka-DOS keeps a
 * single host filesystem view, so there is nothing to redirect. We accept any
 * arguments, say so, and exit clean.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    t_putln("ASSIGN is cosmetic on Toka-DOS.");
    t_exit(0);
    return 0;
}
