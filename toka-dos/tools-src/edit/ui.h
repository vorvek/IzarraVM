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
#endif
