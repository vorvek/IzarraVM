; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; GSWMODE.COM - runtime CPU speed switch. Writes the Lotura mode register
; (port 0xE1) to retarget the GSW-586's live CPU speed without rebooting.
; This is a *runtime-only* override: it never touches CMOS, so the BIOS boot
; default (set by the BIOS setup speed menu) is unaffected.
;
; Usage: GSWMODE 386-slow | 386 | 486 | 586   (case-insensitive)
;   No argument or an unrecognized argument prints usage plus the CURRENT mode
;   (read back from port 0xE1) and writes nothing.
;   The removed 286 name prints its replacement.
;
; Port 0xE1 codes (see crates/izarravm-firmware/roms/izbios-defs.inc
; PORT_LOTURA_MODE and izbios-bootbox.inc bx_spd_row_to_code):
;   0 = 386, 1 = 486, 2 = 586, 3 = 386-slow
;
; Build: nasm -f bin gswmode.asm -o gswmode.com
    cpu 386
    org 0x100

start:
    ; Command tail: PSP:0x80 = length byte, PSP:0x81.. = text, CR-terminated.
    mov cl, [0x80]
    xor ch, ch
    mov si, 0x81
    ; Skip leading spaces/tabs.
.skip:
    jcxz .to_no_arg
    mov al, [si]
    cmp al, ' '
    je .skip_adv
    cmp al, 9                      ; tab
    je .skip_adv
    jmp .have_start
.skip_adv:
    inc si
    dec cx
    jmp .skip
.to_no_arg:
    jmp .no_arg
.have_start:
    ; Copy and uppercase one token. The longest accepted name is 386-SLOW.
    mov di, tok
    xor bx, bx
.copy:
    jcxz .token_done
    mov al, [si]
    inc si
    dec cx
    cmp al, 13                     ; CR
    je .token_done
    cmp al, ' '
    je .token_separator
    cmp al, 9
    je .token_separator
    cmp bx, 8
    jae .to_no_arg2
    cmp al, 'a'
    jb .upper_ok
    cmp al, 'z'
    ja .upper_ok
    sub al, 0x20                   ; lowercase -> uppercase
.upper_ok:
    stosb
    inc bx
    jmp .copy
.token_separator:
    mov al, '$'
    stosb
.skip_trailing:
    jcxz .tok_ready
    lodsb
    dec cx
    cmp al, 13
    je .tok_ready
    cmp al, ' '
    je .skip_trailing
    cmp al, 9
    je .skip_trailing
.to_no_arg2:
    jmp .no_arg
.token_done:
    mov al, '$'
    stosb
    test bx, bx
    jz .to_no_arg2
.tok_ready:
    mov si, tok
    mov di, c386slow
    call streq
    jc .match386slow
    mov si, tok
    mov di, s386
    call streq
    jc .match386
    mov si, tok
    mov di, s486
    call streq
    jc .match486
    mov si, tok
    mov di, s586
    call streq
    jc .match586
    mov si, tok
    mov di, c286
    call streq
    jc .removed286
    jmp .no_arg

.match386slow:
    mov al, 3
    mov dx, s386slow
    jmp .apply
.match386:
    mov al, 0
    mov dx, s386
    jmp .apply
.match486:
    mov al, 1
    mov dx, s486
    jmp .apply
.match586:
    mov al, 2
    mov dx, s586

.apply:
    push dx                        ; the mode name, for the confirmation message
    out 0xE1, al
    mov ah, 0x09
    mov dx, msg_switch1
    int 0x21
    pop dx
    mov ah, 0x09
    int 0x21
    mov dx, msg_switch2
    mov ah, 0x09
    int 0x21
    mov ax, 0x4c00
    int 0x21

.removed286:
    mov ah, 0x09
    mov dx, msg_removed286
    int 0x21
    mov ax, 0x4c01
    int 0x21

.no_arg:
    mov ah, 0x09
    mov dx, msg_usage
    int 0x21
    in al, 0xE1
    mov si, s386
    cmp al, 0
    je .cur
    mov si, s486
    cmp al, 1
    je .cur
    mov si, s586
    cmp al, 2
    je .cur
    mov si, s386slow
.cur:
    mov dx, msg_cur1
    mov ah, 0x09
    int 0x21
    push si
    pop dx
    mov ah, 0x09
    int 0x21
    mov dx, msg_cur2
    mov ah, 0x09
    int 0x21
    mov ax, 0x4c01
    int 0x21

; streq: compare two '$'-terminated strings. CF=1 on match.
streq:
    push si
    push di
.loop:
    mov al, [si]
    cmp al, [di]
    jne .no
    cmp al, '$'
    je .yes
    inc si
    inc di
    jmp .loop
.yes:
    pop di
    pop si
    stc
    ret
.no:
    pop di
    pop si
    clc
    ret

tok:    times 9 db '$'
c286:   db '286', '$'
c386slow: db '386-SLOW', '$'
s386slow: db '386-slow', '$'
s386:   db '386', '$'
s486:   db '486', '$'
s586:   db '586', '$'
msg_switch1: db 'GSWMODE: switched to ', '$'
msg_switch2: db '.', 13, 10, '$'
msg_usage:   db 'Usage: GSWMODE 386-slow|386|486|586', 13, 10, '$'
msg_removed286: db "CPU mode '286' was removed; use '386-slow'.", 13, 10, '$'
msg_cur1:    db 'Current mode: ', '$'
msg_cur2:    db 13, 10, '$'
