; GSWMODE.COM - runtime CPU speed switch. Writes the Lotura mode register
; (port 0xE1) to retarget the GSW-586's live CPU speed without rebooting.
; This is a *runtime-only* override: it never touches CMOS, so the BIOS boot
; default (set by the BIOS setup speed menu) is unaffected.
;
; Usage: GSWMODE 286 | 386 | 486 | 586   (case-insensitive)
;   No argument or an unrecognized argument prints usage plus the CURRENT mode
;   (read back from port 0xE1) and writes nothing.
;
; Port 0xE1 codes (see crates/izarravm-firmware/roms/izbios-defs.inc
; PORT_LOTURA_MODE and izbios-bootbox.inc bx_spd_row_to_code):
;   0 = 386, 1 = 486, 2 = 586, 3 = 286
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
    ; Uppercase the first 3 chars into `tok` (stop early on CR/space -> too short).
    mov di, tok
    mov bx, 3
.copy:
    jcxz .check_tok
    cmp bx, 0
    je .check_tok
    mov al, [si]
    cmp al, 13                     ; CR
    je .check_tok
    cmp al, ' '
    je .check_tok
    cmp al, 'a'
    jb .upper_ok
    cmp al, 'z'
    ja .upper_ok
    sub al, 0x20                   ; lowercase -> uppercase
.upper_ok:
    mov [di], al
    inc di
    inc si
    dec cx
    dec bx
    jmp .copy
.check_tok:
    cmp bx, 0                      ; must have consumed exactly 3 chars
    jne .to_no_arg2
    ; The 4th char (if any before CR/space) must not continue the token, else
    ; e.g. "2860" would falsely match "286".
    jcxz .tok_ready
    mov al, [si]
    cmp al, 13
    je .tok_ready
    cmp al, ' '
    je .tok_ready
.to_no_arg2:
    jmp .no_arg
.tok_ready:
    mov si, tok
    mov di, s286
    call streq3
    jc .match286
    mov si, tok
    mov di, s386
    call streq3
    jc .match386
    mov si, tok
    mov di, s486
    call streq3
    jc .match486
    mov si, tok
    mov di, s586
    call streq3
    jc .match586
    jmp .no_arg

.match286:
    mov al, 3
    mov dx, s286
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
    mov si, s286
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

; streq3: compare 3 bytes at DS:SI (already uppercase) to DS:DI. CF=1 on match.
streq3:
    push si
    push di
    push cx
    mov cx, 3
.loop:
    mov al, [si]
    cmp al, [di]
    jne .no
    inc si
    inc di
    loop .loop
    pop cx
    pop di
    pop si
    stc
    ret
.no:
    pop cx
    pop di
    pop si
    clc
    ret

tok:    times 3 db 0
s286:   db '286', '$'
s386:   db '386', '$'
s486:   db '486', '$'
s586:   db '586', '$'
msg_switch1: db 'GSWMODE: switched to ', '$'
msg_switch2: db '.', 13, 10, '$'
msg_usage:   db 'Usage: GSWMODE 286|386|486|586', 13, 10, '$'
msg_cur1:    db 'Current mode: ', '$'
msg_cur2:    db 13, 10, '$'
