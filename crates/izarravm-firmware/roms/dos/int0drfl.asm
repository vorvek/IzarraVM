; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; int0drfl.com: routing row 1 (design section 3.5). A guest `INT 0Dh` is a
; SOFTWARE interrupt and must always reach the guest's IVT[0x0D], whatever the
; 8259A's vector bases happen to be. On this monitor vector 13 is doubly
; loaded -- it is the #GP the guest's own INT n traps through, and, while the
; master sits at the DOS-default base 8, it is also IRQ5 -- so the monitor
; carries a discriminator there.
;
; The first half is the baseline: hook IVT[0x0D], execute INT 0Dh, check the
; marker. The second half is the discriminating one: move the master base away
; from 8 with a VCPI DE0B first, so vector 13 can no longer be IRQ5 under any
; reading, and require that INT 0Dh STILL lands on IVT[0x0D].
;
; The remap window runs with every line masked and the guest CLI'd, so a tick
; arriving while the guest owns the bases cannot be reflected at a vector DOS
; has not hooked. Both bases and both masks are put back before exit, on every
; exit path.
;
; Signals 0xA5. 0xE1 = the plain INT 0Dh did not reach the handler; 0xE2 = it
; stopped reaching the handler once the master moved. 0xD1 (DE0B refused) and
; 0xD2 (the bases did not take) are setup, kept distinct from the assertions.
;
; Build: nasm -f bin int0drfl.asm -o int0drfl.com
cpu 386
org 0x100
%define OK 0xA5

start:
    xor ax, ax
    mov es, ax
    cli
    mov ax, [es:0x0D*4]
    mov [old0d], ax
    mov ax, [es:0x0D*4+2]
    mov [old0d+2], ax
    mov word [es:0x0D*4], h0d
    mov [es:0x0D*4+2], cs

    ; ---- 1. baseline: the DOS-default bases
    mov byte [hit], 0
    int 0x0D
    cmp byte [hit], 1
    jne f_first

    ; ---- 2. move the master off base 8, with the chip quiet
    in al, 0x21
    mov [mask_m], al
    in al, 0xA1
    mov [mask_s], al
    mov byte [moved], 1
    mov al, 0xFF
    out 0x21, al
    out 0xA1, al

    mov ax, 0xDE0B
    mov bx, 0x88
    mov cx, 0x90
    int 0x67
    or ah, ah
    jnz f_de0b

    mov ax, 0xDE0A                ; read the bases back: the remap must be real
    int 0x67
    cmp bx, 0x88
    jne f_bases
    cmp cx, 0x90
    jne f_bases

    ; ---- 3. the discriminating half
    mov byte [hit], 0
    int 0x0D
    cmp byte [hit], 1
    jne f_remap

    mov bl, OK
    jmp restore

f_first:  mov bl, 0xE1
          jmp restore
f_remap:  mov bl, 0xE2
          jmp restore
f_de0b:   mov bl, 0xD1
          jmp restore
f_bases:  mov bl, 0xD2

restore:
    cmp byte [moved], 0
    je .ivt
    push bx
    mov ax, 0xDE0B
    mov bx, 0x08
    mov cx, 0x70
    int 0x67
    pop bx
    mov al, [mask_m]
    out 0x21, al
    mov al, [mask_s]
    out 0xA1, al
.ivt:
    xor ax, ax
    mov es, ax
    mov ax, [old0d]
    mov [es:0x0D*4], ax
    mov ax, [old0d+2]
    mov [es:0x0D*4+2], ax
    sti

    mov al, 12
    out 0xE4, al                  ; REG_EXIT
    mov al, bl
    out 0xE5, al                  ; code
    mov al, 3
    out 0xE6, al                  ; CMD_EXIT
.h: jmp .h

h0d:
    mov byte [cs:hit], 1
    iret

old0d:  dd 0
mask_m: db 0
mask_s: db 0
moved:  db 0
hit:    db 0
