; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; vcpipic.com -- TOKAEMM VCPI DE0B remapped-PIC IRQ5 smoke test.
; Assemble: nasm -f bin vcpipic.asm -o vcpipic.com
cpu 386
org 0x100

%define OK        0xA5
%define FAIL_VCPI 0xE1
%define FAIL_DSP  0xE2
%define FAIL_IRQ  0xE3
%define FAIL_BASE 0xE4
%define FAIL_OLD  0xD5
%define FAIL_GP13 0xD6

%define BASE      0x220
%define DSP_RESET BASE+0x6
%define DSP_READ  BASE+0xA
%define DSP_WRITE BASE+0xC
%define DSP_STAT  BASE+0xE

start:
    cli
    cld
    push cs
    pop ds

    ; IRQ5 at remapped master base 20h lands on vector 25h.
    xor ax, ax
    mov es, ax
    mov word [es:0x0D * 4], old_irq5_handler
    mov [es:0x0D * 4 + 2], cs
    mov word [es:0x25 * 4], irq5_handler
    mov [es:0x25 * 4 + 2], cs
    mov word [ticks], 0

    mov al, 0xDF                  ; DE0B preserves this IRQ5-only mask
    out 0x21, al

    mov ax, 0xDE0B
    mov bx, 0x20
    mov cx, 0x28
    int 0x67
    or ah, ah
    jnz fail_vcpi

    mov ax, 0xDE0A
    int 0x67
    or ah, ah
    jnz fail_vcpi
    cmp bx, 0x20
    jne fail_base
    cmp cx, 0x28
    jne fail_base

    call reset_dsp
    jc fail_dsp

    sti
    mov al, 0xF2                  ; immediate 8-bit DSP IRQ
    call dsp_write

    mov cx, 0x4000
.wait:
    cmp word [ticks], 0
    jne success
    loop .wait
    mov al, FAIL_IRQ
    jmp signal

success:
    cli
    mov al, OK
    jmp signal

fail_vcpi:
    mov al, FAIL_VCPI
    jmp signal
fail_dsp:
    mov al, FAIL_DSP
    jmp signal
fail_base:
    mov al, FAIL_BASE
    jmp signal

reset_dsp:
    mov dx, DSP_RESET
    mov al, 1
    out dx, al
    call delay
    xor al, al
    out dx, al
    mov cx, 16
.poll:
    call delay
    mov dx, DSP_STAT
    in al, dx
    test al, 0x80
    jnz .ready
    loop .poll
    stc
    ret
.ready:
    mov dx, DSP_READ
    in al, dx
    cmp al, 0xAA
    jne .bad
    clc
    ret
.bad:
    stc
    ret

dsp_write:
    push dx
    push ax
.wait:
    mov dx, DSP_WRITE
    in al, dx
    test al, 0x80
    jnz .wait
    pop ax
    out dx, al
    pop dx
    ret

delay:
    push cx
    mov cx, 0x4000
.loop:
    loop .loop
    pop cx
    ret

irq5_handler:
    push ax
    push dx
    mov dx, DSP_STAT
    in al, dx
    mov al, 0x20
    out 0x20, al
    inc word [cs:ticks]
    pop dx
    pop ax
    iret

old_irq5_handler:
    mov al, 0x0B
    out 0x20, al
    in al, 0x20
    test al, 0x20
    jnz .old_irq
    mov al, FAIL_GP13
    jmp signal
.old_irq:
    mov al, 0x20
    out 0x20, al
    mov al, FAIL_OLD
    jmp signal

signal:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.halt:
    hlt
    jmp .halt

ticks: dw 0
