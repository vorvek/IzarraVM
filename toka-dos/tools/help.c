/* help.c - HELP: print the Toka-DOS command reference, or one-line help
 * for a single command.
 *
 * Usage: HELP [command]
 *   With no argument, lists the built-in commands grouped a few per line.
 *   With a command name, prints one line describing that command.
 *   Unknown commands report that no help is available.
 *
 * All help text is hardcoded; HELP reads no files.
 */
#include <string.h>
#include "toka.h"

int main(void)
{
    char *p = t_cmdtail();
    static char cmd[32];
    int k;

    /* Skip the leading blanks DOS puts before the first argument. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    /* Copy the first word of the tail into cmd, uppercasing as we go so the
     * comparisons below stay simple. */
    k = 0;
    while (*p && *p != ' ' && *p != '\t' && k < 31) {
        char c = *p++;
        if (c >= 'a' && c <= 'z') {
            c = (char)(c - 'a' + 'A');
        }
        cmd[k++] = c;
    }
    cmd[k] = 0;

    if (cmd[0] == 0) {
        t_putln("Toka-DOS command reference");
        t_putln("");
        t_putln("DIR COPY DEL TYPE REN MD RD CD");
        t_putln("DATE TIME VOL VER CLS ECHO SET PATH PROMPT");
        t_putln("FIND FINDSTR SORT MORE ATTRIB MOVE XCOPY TREE");
        t_putln("DELTREE COMP FC REPLACE EXPAND LABEL HELP");
        t_exit(0);
    }

    if (strcmp(cmd, "DIR") == 0) {
        t_putln("DIR - list files and subdirectories in a directory.");
    } else if (strcmp(cmd, "COPY") == 0) {
        t_putln("COPY - copy one or more files to another location.");
    } else if (strcmp(cmd, "DEL") == 0) {
        t_putln("DEL - delete one or more files.");
    } else if (strcmp(cmd, "TYPE") == 0) {
        t_putln("TYPE - display the contents of a text file.");
    } else if (strcmp(cmd, "REN") == 0) {
        t_putln("REN - rename a file or files.");
    } else if (strcmp(cmd, "MD") == 0) {
        t_putln("MD - create a directory.");
    } else if (strcmp(cmd, "RD") == 0) {
        t_putln("RD - remove an empty directory.");
    } else if (strcmp(cmd, "CD") == 0) {
        t_putln("CD - display or change the current directory.");
    } else if (strcmp(cmd, "DATE") == 0) {
        t_putln("DATE - display or set the system date.");
    } else if (strcmp(cmd, "TIME") == 0) {
        t_putln("TIME - display or set the system time.");
    } else if (strcmp(cmd, "VOL") == 0) {
        t_putln("VOL - display the disk volume label.");
    } else if (strcmp(cmd, "VER") == 0) {
        t_putln("VER - display the Toka-DOS version.");
    } else if (strcmp(cmd, "CLS") == 0) {
        t_putln("CLS - clear the screen.");
    } else if (strcmp(cmd, "ECHO") == 0) {
        t_putln("ECHO - display a message or toggle command echoing.");
    } else if (strcmp(cmd, "SET") == 0) {
        t_putln("SET - display or change environment variables.");
    } else if (strcmp(cmd, "PATH") == 0) {
        t_putln("PATH - display or set the executable search path.");
    } else if (strcmp(cmd, "PROMPT") == 0) {
        t_putln("PROMPT - change the command prompt text.");
    } else if (strcmp(cmd, "FIND") == 0) {
        t_putln("FIND - search a file for lines containing a string.");
    } else if (strcmp(cmd, "FINDSTR") == 0) {
        t_putln("FINDSTR - search files for lines matching patterns.");
    } else if (strcmp(cmd, "SORT") == 0) {
        t_putln("SORT - sort lines of text and display the result.");
    } else if (strcmp(cmd, "MORE") == 0) {
        t_putln("MORE - display text one screen at a time.");
    } else if (strcmp(cmd, "ATTRIB") == 0) {
        t_putln("ATTRIB - display or change file attributes.");
    } else if (strcmp(cmd, "MOVE") == 0) {
        t_putln("MOVE - move files or rename a directory.");
    } else if (strcmp(cmd, "XCOPY") == 0) {
        t_putln("XCOPY - copy files and directory trees.");
    } else if (strcmp(cmd, "TREE") == 0) {
        t_putln("TREE - show the directory structure graphically.");
    } else if (strcmp(cmd, "DELTREE") == 0) {
        t_putln("DELTREE - delete a directory and all its contents.");
    } else if (strcmp(cmd, "COMP") == 0) {
        t_putln("COMP - compare the contents of two files.");
    } else if (strcmp(cmd, "FC") == 0) {
        t_putln("FC - compare two files and show the differences.");
    } else if (strcmp(cmd, "REPLACE") == 0) {
        t_putln("REPLACE - replace files in one directory from another.");
    } else if (strcmp(cmd, "EXPAND") == 0) {
        t_putln("EXPAND - expand a compressed file.");
    } else if (strcmp(cmd, "LABEL") == 0) {
        t_putln("LABEL - create or change a disk volume label.");
    } else if (strcmp(cmd, "HELP") == 0) {
        t_putln("HELP - display help for Toka-DOS commands.");
    } else {
        t_putln("No help available for that command");
        t_exit(1);
    }

    t_exit(0);
    return 0;
}
