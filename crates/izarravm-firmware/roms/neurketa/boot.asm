bits 16
org 0x7c00

%define STAGE2_SEGMENT 0x0000
%define STAGE2_OFFSET 0x8000
%define STAGE2_SECTORS 16

start:
    cli
    cld
    mov ax, 0
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    mov [boot_drive], dl

    mov ax, STAGE2_SEGMENT
    mov es, ax
    mov bx, STAGE2_OFFSET
    mov ah, 0x02
    mov al, STAGE2_SECTORS
    mov ch, 0
    mov cl, 2
    mov dh, 0
    mov dl, [boot_drive]
    clc
    int 0x13
    jc disk_error

    jmp STAGE2_SEGMENT:STAGE2_OFFSET

disk_error:
    hlt
    jmp disk_error

boot_drive db 0

times 510 - ($ - $$) db 0
dw 0xaa55
