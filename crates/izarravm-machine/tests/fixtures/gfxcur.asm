; GFXCUR.COM - verify IZMOUSE does not draw its text cursor in graphics modes.
;
; Runs after MOUSE.COM is resident. It switches to VGA mode 12h, resets the mouse
; so the driver sizes its range to 640x480, writes sentinel bytes at the B800 text
; cursor cell the old driver would touch, calls Show Cursor, and fails if the
; sentinel changed.
;
; Assemble: nasm -f bin gfxcur.asm -o GFXCUR.COM
    cpu 386
    org 0x100

CURSOR_CELL     equ 0x126E            ; ((239 >> 3) * 80 + (319 >> 3)) * 2

UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3

start:
    mov ax, 0x0012                    ; 640x480x16 planar graphics
    int 0x10

    xor ax, ax                        ; mouse reset
    int 0x33
    cmp ax, 0xFFFF
    jne fail_reset

    mov ax, 0xB800
    mov es, ax
    mov di, CURSOR_CELL
    mov byte [es:di], 0xA5
    mov byte [es:di + 1], 0x5A

    mov ax, 0x0001                    ; show cursor
    int 0x33

    cmp byte [es:di], 0xA5
    jne fail_dirty
    cmp byte [es:di + 1], 0x5A
    jne fail_dirty

    xor al, al
    jmp ut_exit

fail_reset:
    mov al, 1
    jmp ut_exit
fail_dirty:
    mov al, 2
    jmp ut_exit

ut_exit:
    mov ah, al
    mov dx, UT_INDEX
    mov al, UT_REG_EXIT
    out dx, al
    mov dx, UT_DATA
    mov al, ah
    out dx, al
    mov dx, UT_COMMAND
    mov al, UT_CMD_EXIT
    out dx, al
.hang:
    jmp .hang
