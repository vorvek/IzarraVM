; A DOS .COM that prints a file: open C:\HELLO.TXT for read (INT 21h AH=3Dh),
; read it (AH=3Fh), close it (AH=3Eh), then write the bytes to stdout one at a
; time (AH=02h) and exit (AH=4Ch). Exit code 1 on any file error.
; Assemble with: nasm -f bin type.asm -o type.com
    org 0x100
    mov ax, 0x3d00          ; AH=3Dh open existing, AL=00 read access
    mov dx, filename
    int 0x21
    jc  .error
    mov bx, ax              ; file handle
    mov ah, 0x3f            ; read
    mov cx, 0x4000          ; up to 16 KiB (the fixture file is tiny)
    mov dx, buffer
    int 0x21
    jc  .error
    mov cx, ax              ; bytes read -> print counter
    push cx
    mov ah, 0x3e            ; close (BX still the handle)
    int 0x21
    pop cx
    mov si, buffer
.print:
    jcxz .done
    mov dl, [si]
    mov ah, 0x02
    int 0x21
    inc si
    dec cx
    jmp .print
.done:
    mov ax, 0x4c00
    int 0x21
.error:
    mov ax, 0x4c01
    int 0x21
filename:
    db 'C:\HELLO.TXT', 0
buffer:
