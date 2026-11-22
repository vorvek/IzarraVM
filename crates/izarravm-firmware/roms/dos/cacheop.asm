; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; cacheop.com: TOKAEMM V86 cache-flush emulation fixture. INVD and WBINVD are
; privileged on the 486 and later, so a V86 task gets #GP(0) and the monitor
; must emulate them: run a ring-0 WBINVD, step past the instruction, resume.
; Prefixed forms must work too (up to 13 prefixes (15 bytes) are legal).
; Verifies:
;   - the program runs in V86 (CR0.PE set), so the test proves something;
;   - WBINVD, INVD, WBINVD leave every general register unchanged;
;   - four prefixed forms advance IP by prefix count + 2;
;   - IF and DF are unchanged after the cache operations.
; INT 06h (#UD) and INT 0Dh (#GP reflection) are hooked. A wrong path then
; ends with a named code and not with a hang.
;
; Signals 0xA5 (success) through the unit-tester exit port.
;   0xE1 not in V86, 0xE2 register changed, 0xE3 flags changed,
;   0xE4 wrong IP step after a prefixed form,
;   0xE6 guest got INT 06h, 0xED guest got INT 0Dh.
;
; Build: nasm -f bin cacheop.asm -o cacheop.com
cpu 486
org 0x100
%define OK 0xA5

start:
    ; Hook INT 06h (0:18h) and INT 0Dh (0:34h) with interrupts off.
    cli
    xor ax, ax
    mov es, ax
    mov word [es:0x18], on_ud
    mov [es:0x1A], cs
    mov word [es:0x34], on_gp
    mov [es:0x36], cs
    sti

    ; 1. SMSW is not privileged. PE set means this code runs in V86.
    smsw ax
    test al, 1
    jz f_notv86

    ; Flags before: set DF, then record IF and DF.
    std
    pushf
    pop ax
    and ax, 0x0600
    mov [flags0], ax

    ; 2. Sentinels in every general register, then three cache operations.
    mov eax, 0x11111111
    mov ebx, 0x22222222
    mov ecx, 0x33333333
    mov edx, 0x44444444
    mov esi, 0x55555555
    mov edi, 0x66666666
    mov ebp, 0x77777777
    wbinvd
    invd
    wbinvd
    cmp eax, 0x11111111
    jne f_reg
    cmp ebx, 0x22222222
    jne f_reg
    cmp ecx, 0x33333333
    jne f_reg
    cmp edx, 0x44444444
    jne f_reg
    cmp esi, 0x55555555
    jne f_reg
    cmp edi, 0x66666666
    jne f_reg
    cmp ebp, 0x77777777
    jne f_reg

    ; 3. Prefixed forms. Each is followed by a marker, INC SI. A wrong IP
    ;    step lands inside the next instruction and SI does not reach 4.
    xor si, si
    db 0x2E, 0x0F, 0x09           ; CS: WBINVD
    inc si
    db 0x66, 0x0F, 0x09           ; o32 WBINVD
    inc si
    db 0xF3, 0x0F, 0x08           ; REP INVD
    inc si
    db 0x26, 0x67, 0x0F, 0x09     ; ES: a32 WBINVD
    inc si
    cmp si, 4
    jne f_step

    ; 4. IF and DF must be as before.
    pushf
    pop ax
    and ax, 0x0600
    cmp ax, [flags0]
    jne f_flags
    cld

    mov al, OK
    jmp sig

on_ud:      mov al, 0xE6
            jmp sig
on_gp:      mov al, 0xED
            jmp sig
f_notv86:   mov al, 0xE1
            jmp sig
f_reg:      mov al, 0xE2
            jmp sig
f_flags:    mov al, 0xE3
            jmp sig
f_step:     mov al, 0xE4

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

align 4
flags0: dw 0
