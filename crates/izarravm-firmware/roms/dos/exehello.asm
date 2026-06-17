; A DOS .EXE: load DS from a relocated segment reference, print via INT 21h
; AH=09h, exit via AH=4Ch. The "mov ax, DATA / mov ds, ax" emits an MZ
; relocation; if the loader fails to apply it, DS is wrong and the printed
; bytes diverge. Build (Open Watcom wlink, authoring-only, not in CI):
;   set WATCOM=D:\DevTools\OpenWatcom
;   nasm -f obj exehello.asm -o exehello.obj
;   %WATCOM%\binnt\wlink.exe format dos name exehello.exe file exehello.obj
bits 16
segment DATA class=DATA
msg: db 'Hello from a relocated .EXE!', 13, 10, '$'
segment CODE class=CODE
..start:
    mov ax, DATA
    mov ds, ax
    mov dx, msg
    mov ah, 0x09
    int 0x21
    mov ax, 0x4c00
    int 0x21
segment STACK class=STACK stack
    resb 128
