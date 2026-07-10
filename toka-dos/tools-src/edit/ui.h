/* This file is part of IzarraVM and is licensed under GNU GPL version 3 only. */
/* SPDX-License-Identifier: GPL-3.0-only */

/* TokaEdit menu bar and pulldown engine. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#ifndef UI_H
#define UI_H
typedef struct {
    const char *label;   /* "&Save As..." -- '&' marks the hotkey letter */
    const char *rlabel;  /* right column, e.g. "Shift+Del", or NULL */
    int id;              /* MI_* action id, 0 = separator */
} MenuItem;
typedef struct {
    const char *title;   /* "&File" */
    const MenuItem *items;
    int nitems;
} Menu;

enum { MI_NEW = 1, MI_OPEN, MI_SAVE, MI_SAVEAS, MI_EXIT,
       MI_CUT, MI_COPY, MI_PASTE, MI_CLEAR,
       MI_FIND, MI_FINDNEXT, MI_CHANGE, MI_ABOUT };

/* run the menu bar: returns the chosen MI_* or 0 for Esc/click-away.
 * open_menu: index to open immediately (Alt+F = 0), -1 = bar armed only.
 * enabled(id) callback lets Edit-menu items grey out. */
int menu_run(int open_menu, int (*enabled)(int id));
/* paint the idle menu bar (row 0); edit.c calls this from redraw */
void menu_draw_bar(void);
/* menu index under a bar click, or -1 */
int menu_hit(int col);

/* Modal dialog: centered box, one optional text field, 1..4 buttons.
 * Returns the 0-based index of the pressed button, or -1 for Esc.
 * If field is non-NULL it is an in/out buffer (edited in place, fieldcap
 * bytes incl. NUL) shown above the buttons. prompt may contain one '\n'
 * to split into two lines (used by About). */
int dlg_run(const char *title, const char *prompt,
            char *field, int fieldcap,
            const char **buttons, int nbuttons);
/* conveniences built on dlg_run */
void dlg_msg(const char *title, const char *text);            /* [OK] */
int  dlg_yesnocancel(const char *title, const char *text);    /* 0=Yes 1=No 2=Cancel; Esc=2 */
#endif
