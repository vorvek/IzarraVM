/* toka.h - shared runtime for Toka-DOS tools and the ICOMMAND shell.
 *
 * Every Toka-DOS binary is a tiny real-mode .COM built small model with Open
 * Watcom and linked `system com`. These helpers wrap the DOS and BIOS interrupt
 * calls so each tool stays small and consistent. They assume the .COM model
 * where CS = DS = SS, so near pointers and DS-relative INT 21h calls just work.
 */
#ifndef TOKA_H
#define TOKA_H

/* Console output via INT 21h. */
void t_putc(char c);              /* one character (AH=02h)                  */
void t_puts(const char *s);       /* a string, no trailing newline           */
void t_putln(const char *s);      /* a string followed by CR LF              */
void t_putu(unsigned value);      /* an unsigned decimal number              */

/* Line input via INT 21h AH=0Ah. Returns the length, NUL-terminates `out`,
 * and echoes a CR LF so the caller lands on a fresh line. `max` is capped to
 * 127. */
int t_getline(char *out, int max);

/* Screen control via INT 10h. */
void t_setmode3(void);            /* 80x25 text, mode 03h (also clears)       */
void t_cls(void);                 /* clear the screen                         */

/* The command tail from the PSP (PSP:0x80), copied and NUL-terminated, with the
 * leading length byte and the trailing CR dropped. Includes the leading blank
 * DOS puts before the first argument. */
char *t_cmdtail(void);

/* Terminate the program with an exit code (AH=4Ch). Does not return. */
void t_exit(int code);

/* Run another program through EXEC (INT 21h AH=4B00h). `path` is a DOS path
 * such as "C:\\VER.COM". `tail` is the command tail text (without the length
 * byte or CR), or "" for none. Returns 0 on launch, or the DOS error code in
 * AX if the program could not be loaded (2 = not found). */
int t_exec(const char *path, const char *tail);

#endif /* TOKA_H */
