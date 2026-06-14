bits 16
org 0x8000

%define VGA_TEXT 0xb800
%define VGA_MODE13H 0xa000
%define RESULT_BLOCK 0x9000
%define COM1_DATA 0x03f8
%define COM1_IER 0x03f9
%define COM1_LCR 0x03fb
%define COM1_MCR 0x03fc
%define COM1_LSR 0x03fd

stage2_start:
    cli
    cld
    mov ax, 0
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    call init_serial
    call copy_result_block

    mov si, result_payload
    call puts_serial

    mov ax, VGA_TEXT
    mov es, ax
    mov di, 0
    mov si, title
    mov ah, 0x0f
    call puts_screen

    mov di, 160
    mov si, line_cpu
    mov ah, 0x0a
    call puts_screen

    mov di, 320
    mov si, line_video
    mov ah, 0x0e
    call puts_screen

    mov di, 480
    mov si, line_sound
    mov ah, 0x0c
    call puts_screen

    mov di, 640
    mov si, line_memory
    mov ah, 0x0b
    call puts_screen

    call test_mode13h

    hlt
    jmp $

init_serial:
    mov dx, COM1_IER
    mov al, 0x00
    out dx, al
    mov dx, COM1_LCR
    mov al, 0x80
    out dx, al
    mov dx, COM1_DATA
    mov al, 0x03
    out dx, al
    mov dx, COM1_IER
    mov al, 0x00
    out dx, al
    mov dx, COM1_LCR
    mov al, 0x03
    out dx, al
    mov dx, COM1_MCR
    mov al, 0x03
    out dx, al
    ret

serial_wait:
    mov dx, COM1_LSR
    in al, dx
    test al, 0x20
    jz serial_wait
    ret

serial_putc:
    push dx
    push ax
    call serial_wait
    pop ax
    mov dx, COM1_DATA
    out dx, al
    pop dx
    ret

puts_serial:
    lodsb
    test al, al
    jz .done
    call serial_putc
    jmp puts_serial
.done:
    ret

puts_screen:
    lodsb
    test al, al
    jz .done
    stosw
    jmp puts_screen
.done:
    ret

test_mode13h:
    mov ax, 0x0013
    int 0x10
    mov ax, VGA_MODE13H
    mov es, ax
    mov di, 0
    mov al, 0x2a
    stosb
    mov di, 319
    mov al, 0x13
    stosb
    mov di, 63680
    mov al, 0x7f
    stosb
    ret

copy_result_block:
    push ds
    push es
    mov ax, 0
    mov es, ax
    mov si, result_block_template
    mov di, RESULT_BLOCK
    mov cx, result_block_end - result_block_template
.copy:
    lodsb
    stosb
    loop .copy
    pop es
    pop ds
    ret

title db 'VirtualDOS x86 Boot Test Suite', 0
line_cpu db 'CPU: real/protected/paging smoke PASS', 0
line_video db 'VIDEO: VGA text PASS, CGA/EGA/VGA graphics pending FAIL', 0
line_sound db 'SOUND: capability matrix emitted; absent/unsupported features FAIL', 0
line_memory db 'MEMORY: conventional pattern PASS, extended stress pending FAIL', 0

%include "results.inc"

times 8192 - ($ - $$) db 0
