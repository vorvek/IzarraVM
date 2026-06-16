; A DOS .COM filter: read bytes with INT 21h AH=08h (no echo) and write each back
; with AH=02h until end of input (^Z, 0x1A), then exit via AH=4Ch.
; Assemble with: nasm -f bin echo.asm -o echo.com
    org 0x100
.loop:
    mov ah, 0x08
    int 0x21
    cmp al, 0x1a
    je .done
    mov dl, al
    mov ah, 0x02
    int 0x21
    jmp .loop
.done:
    mov ax, 0x4c00
    int 0x21
