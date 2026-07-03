/* TokaEdit DOS TUI hardware layer. Part of the Toka-DOS project, GPL-3.0-only. Copyright (c) 2026 the IzarraVM project. */
#ifndef TUI_H
#define TUI_H

void scr_init(void);                 /* force mode 3 */
void scr_exit(void);                 /* set mode 3 again (clears screen) */
void scr_put(int row, int col, const char *s, unsigned char attr);
void scr_putc(int row, int col, char ch, unsigned char attr);
void scr_fill(int row, int col, int w, int h, char ch, unsigned char attr);
void scr_cursor(int row, int col);   /* move the hardware cursor */
void scr_cursor_hide(void);
/* save/restore a screen rectangle (VRAM bytes) for pulldowns/dialogs;
 * buf must hold w*h*2 bytes */
void scr_save_rect(int row, int col, int w, int h, char *buf);
void scr_restore_rect(int row, int col, int w, int h, const char *buf);

/* one unified input event */
typedef struct {
    int kind;              /* EV_* below */
    unsigned char scan;    /* EV_KEY: INT 16h scancode  */
    unsigned char ascii;   /* EV_KEY: ASCII (0 for extended keys) */
    unsigned char mods;    /* EV_KEY: bit0 shift, bit1 ctrl, bit2 alt */
    int mrow, mcol;        /* EV_MOUSE_*: text cell */
} Event;
enum { EV_KEY, EV_MOUSE_DOWN, EV_MOUSE_DRAG, EV_MOUSE_UP, EV_ALT_TAP };
void ev_wait(Event *e);    /* blocking; polls keyboard + mouse + Alt-tap */

int  mouse_present(void);  /* cached INT 33h AX=0 result; call mouse_init first */
void mouse_init(void);
void mouse_show(void);
void mouse_hide(void);     /* MUST bracket screen repaints when present */

#endif
