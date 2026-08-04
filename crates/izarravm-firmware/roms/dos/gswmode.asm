; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; GSWMODE.COM - CPU speed switch. Writes the Lotura mode register (port 0xE1)
; to retarget the GSW-586's live CPU speed without rebooting, and saves the
; choice in CMOS so it survives one.
;
; It used to be runtime-only, on the grounds that the BIOS setup speed menu
; owned the boot default. That left the speed as the one machine setting with
; no way to change it permanently from DOS, which stopped making sense once
; CMOS became the machine's single record of its own configuration -- the
; config file no longer carries a CPU speed at all. `/T` keeps the old
; behaviour for the case it was actually good at: running one program slower
; without committing to it.
;
; Usage: GSWMODE 386-slow | 386 | 486 | 586 [/T]   (case-insensitive)
;   /T applies the speed for this session only and leaves CMOS alone.
;   No argument or an unrecognized argument prints usage, the CURRENT mode
;   (read back from port 0xE1) and the SAVED mode (CMOS 0x12), and writes
;   nothing. The removed 286 name prints its replacement.
;
; Port 0xE1 codes (see crates/izarravm-firmware/roms/izbios-defs.inc
; PORT_LOTURA_MODE and izbios-bootbox.inc bx_spd_row_to_code) are the same
; codes CMOS 0x12 stores (GswMode::register_code):
;   0 = 386, 1 = 486, 2 = 586, 3 = 386-slow
;
; CMOS 0x12 sits inside the 0x10..0x2D checksum window, so 0x2E/0x2F are
; refreshed with it; without that the next POST discards the whole NVRAM.
;
; Build: nasm -f bin gswmode.asm -o gswmode.com
    cpu 386
    org 0x100

CM_GSW_MODE  equ 0x12
CM_SUM_FIRST equ 0x10
CM_SUM_LAST  equ 0x2D
CM_SUM_HI    equ 0x2E
CM_SUM_LO    equ 0x2F

start:
    call scan_temp_switch
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
    ; A switch after the mode name is fine -- scan_temp_switch has already read
    ; it. Anything else is a typo, and saying so beats acting on half a line.
    cmp al, '/'
    je .eat_switch
    cmp al, '-'
    jne .to_no_arg2
.eat_switch:
    jcxz .tok_ready
    lodsb
    dec cx
    cmp al, 13
    je .tok_ready
    cmp al, ' '
    je .skip_trailing
    cmp al, 9
    je .skip_trailing
    jmp .eat_switch
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
    cmp byte [temp_only], 0
    jne .announce
    call save_mode
.announce:
    mov ah, 0x09
    mov dx, msg_switch1
    int 0x21
    pop dx
    mov ah, 0x09
    int 0x21
    mov dx, msg_saved
    cmp byte [temp_only], 0
    je .tail
    mov dx, msg_session
.tail:
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
    call mode_name                 ; -> SI
    mov dx, msg_cur1
    mov ah, 0x09
    int 0x21
    push si
    pop dx
    mov ah, 0x09
    int 0x21
    mov dx, msg_crlf
    mov ah, 0x09
    int 0x21
    ; The saved mode is the one the next boot starts at, which is a different
    ; question from what the CPU is doing now, so report both.
    mov al, CM_GSW_MODE
    call cmos_read
    call mode_name
    mov dx, msg_saved1
    mov ah, 0x09
    int 0x21
    push si
    pop dx
    mov ah, 0x09
    int 0x21
    mov dx, msg_crlf
    mov ah, 0x09
    int 0x21
    mov ax, 0x4c01
    int 0x21

; AL = port 0xE1 / CMOS 0x12 mode code -> SI = its '$'-terminated name.
mode_name:
    mov si, s386
    cmp al, 0
    je .done
    mov si, s486
    cmp al, 1
    je .done
    mov si, s586
    cmp al, 2
    je .done
    mov si, s386slow
    cmp al, 3
    je .done
    mov si, s_unknown
.done:
    ret

; Set temp_only when the tail carries /T (or -T), anywhere in it. Scanned
; separately from the mode token so the switch can precede or follow it.
scan_temp_switch:
    mov cl, [0x80]
    xor ch, ch
    mov si, 0x81
.loop:
    jcxz .done
    mov al, [si]
    cmp al, '/'
    je .maybe
    cmp al, '-'
    je .maybe
.next:
    inc si
    dec cx
    jmp .loop
.maybe:
    cmp cx, 2
    jb .done
    mov al, [si + 1]
    cmp al, 'a'
    jb .compare
    cmp al, 'z'
    ja .compare
    sub al, 0x20
.compare:
    cmp al, 'T'
    jne .next
    mov byte [temp_only], 1
.done:
    ret

; AL = mode code. Persist it in CMOS and refresh the NVRAM checksum, which the
; write invalidates -- leaving it stale would make the next POST throw away the
; keyboard layout and the sound card's resources along with the speed.
save_mode:
    push ax
    pushf
    cli
    mov ah, CM_GSW_MODE
    call cmos_write
    xor bx, bx
    mov cl, CM_SUM_FIRST
.sum:
    mov al, cl
    call cmos_read
    xor ah, ah
    add bx, ax
    inc cl
    cmp cl, CM_SUM_LAST + 1
    jb .sum
    mov ah, CM_SUM_HI
    mov al, bh
    call cmos_write
    mov ah, CM_SUM_LO
    mov al, bl
    call cmos_write
    mov al, 0x0D                   ; leave the index somewhere harmless, NMI on
    out 0x70, al
    popf
    pop ax
    ret

; AL = index -> AL = value. NMI stays masked for the access, as every BIOS does.
cmos_read:
    or al, 0x80
    out 0x70, al
    jmp short $+2
    in al, 0x71
    ret

; AH = index, AL = value.
cmos_write:
    push ax
    mov al, ah
    or al, 0x80
    out 0x70, al
    jmp short $+2
    pop ax
    out 0x71, al
    ret

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

temp_only: db 0
tok:    times 9 db '$'
c286:   db '286', '$'
c386slow: db '386-SLOW', '$'
s386slow: db '386-slow', '$'
s386:   db '386', '$'
s486:   db '486', '$'
s586:   db '586', '$'
s_unknown: db '(unknown)', '$'
msg_switch1: db 'GSWMODE: switched to ', '$'
msg_saved:   db ', saved.', 13, 10, '$'
msg_session: db ' for this session only.', 13, 10, '$'
msg_crlf:    db 13, 10, '$'
msg_usage:
    db 'Usage: GSWMODE 386-slow|386|486|586 [/T]', 13, 10
    db '  The speed is saved and survives a reboot; /T applies it', 13, 10
    db '  for this session only.', 13, 10, '$'
msg_removed286: db "CPU mode '286' was removed; use '386-slow'.", 13, 10, '$'
msg_cur1:    db 'Current mode: ', '$'
msg_saved1:  db 'Saved mode:   ', '$'
