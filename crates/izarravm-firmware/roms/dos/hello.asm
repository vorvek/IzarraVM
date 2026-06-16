; A minimal DOS .COM: print a message with INT 21h AH=09h, then exit via AH=4Ch.
; Assemble with: nasm -f bin hello.asm -o hello.com
    org 0x100
    mov ah, 0x09
    mov dx, message
    int 0x21
    mov ax, 0x4c00
    int 0x21
message:
    db 'Hello, world!', 13, 10, '$'
