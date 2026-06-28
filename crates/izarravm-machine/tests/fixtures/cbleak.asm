; CBLEAK.COM - install an INT 33h callback and exit without clearing it.
;
; The callback exits the VM through the Lotura unit-tester. The host-side test
; runs this under IZCMD, waits for the prompt to return, injects mouse motion, and
; expects the callback not to fire because the child process has exited.
;
; Assemble: nasm -f bin cbleak.asm -o CBLEAK.COM
    cpu 386
    org 0x100

UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3

start:
    mov ax, 0x000C                    ; set user callback
    mov cx, 0x0001                    ; motion events
    push cs
    pop es
    mov dx, stale_callback
    int 0x33
    mov ax, 0x4C00
    int 0x21

stale_callback:
    push ax
    push dx
    mov dx, UT_INDEX
    mov al, UT_REG_EXIT
    out dx, al
    mov dx, UT_DATA
    mov al, 99
    out dx, al
    mov dx, UT_COMMAND
    mov al, UT_CMD_EXIT
    out dx, al
    pop dx
    pop ax
    retf
