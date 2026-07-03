/* TokaEdit smoke-test main. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
/* Temporary: exercises the TUI hardware layer only. Replaced by the real
 * editor main in a later task. */
#include "tui.h"

#define AT_TEXT 0x07
#define AT_SEL  0x70
#define AT_BAR  0x70
#define AT_HOT  0x7F
#define AT_MSEL 0x07
#define AT_DIS  0x78

int main(void) {
    Event e;

    scr_init();
    mouse_init();

    scr_fill(0, 0, 80, 25, ' ', AT_TEXT);

    scr_fill(0, 0, 80, 1, ' ', AT_BAR);
    scr_put(0, 1, "  File  Edit  Search  Help", AT_BAR);

    scr_fill(1, 0, 80, 1, ' ', AT_BAR);
    scr_put(1, 35, " Untitled ", AT_BAR);

    scr_fill(24, 0, 80, 1, ' ', AT_BAR);
    scr_put(24, 0, "F1=About  F3=Repeat Find", AT_BAR);
    scr_put(24, 78 - 13, "Line:1  Col:1", AT_BAR);

    scr_cursor(2, 0);

    do {
        ev_wait(&e);
    } while (e.kind != EV_KEY);

    scr_exit();
    return 0;
}
