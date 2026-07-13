; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; TOKACD timeout fixture. The host leaves the next ATAPI PACKET unanswered;
; the driver's BIOS-tick or finite-poll guard must return 810Ch.
cpu 8086
org 0x100

start:
    push cs
    pop ds
    push ds
    pop es
    mov bx, device_list
    mov ax, 0x1501
    int 0x2F
    les di, [device_list + 1]
    mov ax, es
    or ax, di
    jz fail
    mov ax, [es:di + 6]
    mov [strategy_ptr], ax
    mov ax, [es:di + 8]
    mov [interrupt_ptr], ax
    mov ax, es
    mov [strategy_ptr + 2], ax
    mov [interrupt_ptr + 2], ax

    mov byte [request], 26
    mov byte [request + 2], 3
    mov word [request + 14], control
    mov word [request + 16], cs
    mov word [request + 18], 5
    mov byte [control], 8
    push cs
    pop es
    mov bx, request
    call far [strategy_ptr]
    call far [interrupt_ptr]
    cmp word [request + 3], 0x810C
    jne fail
    mov al, 0xA8
    jmp signal
fail:
    mov al, 0x58
signal:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.hang:
    hlt
    jmp .hang

device_list:    times 5 db 0
strategy_ptr:   dd 0
interrupt_ptr:  dd 0
request:        times 40 db 0
control:        times 5 db 0
