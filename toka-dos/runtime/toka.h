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
void t_putul(unsigned long value);/* a 32-bit unsigned decimal number        */

/* Line input via INT 21h AH=0Ah. Returns the length, NUL-terminates `out`,
 * and echoes a CR LF so the caller lands on a fresh line. `max` is capped to
 * 127. */
int t_getline(char *out, int max);

/* Screen control via INT 10h. */
void t_setmode3(void);            /* 80x25 text, mode 03h (also clears)       */
void t_cls(void);                 /* clear the screen                         */
/* Update one text cell only when its character or attribute changed. */
void t_putcell(int row, int col, char ch, unsigned char attr);

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

/* As t_exec, but with an explicit environment. `env_seg` is the paragraph of a
 * DOS-format environment block to give the child, or 0 to inherit the parent
 * environment (the t_exec behavior). */
int t_exec_env(const char *path, const char *tail, unsigned env_seg);

/* --- DOS file and directory services (INT 21h) ------------------------------
 * Path-based calls take a DOS path (drive-qualified, absolute, or relative to
 * the current directory). Directory calls return 0 on success or the DOS error
 * code on failure. */

/* File handles. t_open mode: 0 read, 1 write, 2 read/write. Handle >= 5 on
 * success, -1 on error. */
int t_open(const char *path, int mode);
int t_create(const char *path);
int t_read(int handle, void *buf, int count);  /* bytes read, -1 on error */
int t_write(int handle, const void *buf, int count);
void t_close(int handle);

int t_mkdir(const char *path);
int t_rmdir(const char *path);
int t_chdir(const char *path);
int t_delete(const char *path);
int t_rename(const char *oldpath, const char *newpath);
void t_getcwd(char *buf);   /* current dir from root, no leading slash, into buf */

/* Find first/next. `dta` is a 43-byte buffer the result is written into; read
 * the 8.3 name at dta+0x1E (ASCIIZ), attribute at dta+0x15, size dword at
 * dta+0x1A. Returns 0 when a match was written, nonzero when none. */
int t_findfirst(const char *spec, int attr, void *dta);
int t_findnext(void *dta);

void t_getdate(int *year, int *month, int *day, int *dow);
void t_gettime(int *hour, int *min, int *sec);

/* The exit code of the last EXEC'd child (AH=4Dh), for ERRORLEVEL. */
int t_lastexit(void);
/* Read one key without echo (AH=07h); returns the ASCII byte. */
int t_getkey(void);
/* Read one key via the BIOS keyboard (INT 16h AH=00h). Returns the full word:
 * the scancode in the high byte and the ASCII in the low byte, so callers can
 * see extended keys (arrows, Home/End, F-keys) that carry ASCII 0. */
int t_readkey16(void);
/* Move the hardware text cursor (INT 10h AH=02h, page 0). */
void t_setcursor(int row, int col);
/* True if `path` exists (a file or, with a wildcard, any match). */
int t_exists(const char *path);

#endif /* TOKA_H */
