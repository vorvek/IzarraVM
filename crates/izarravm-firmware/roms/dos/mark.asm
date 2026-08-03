; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; MARK.COM - place a boot-profiler phase boundary. Writes the boundary id to the
; Lotura unit tester's REG_MARK and issues CMD_MARK, which records a snapshot of
; every counter the profiler attributes per phase and lets the machine keep
; running (unlike CMD_EXIT, which stops it).
;
; Usage: MARK <0-9>
;
; Appended to AUTOEXEC.BAT by --headless-boot-profile to say "Toka-DOS is up".
; Deliberately silent on success: this runs inside the boot being measured, so
; any console output would put video and DOS teletype work in the sample.
;
; Ports (crates/izarravm-machine/src/unittester.rs):
;   0xE4 index, 0xE5 data, 0xE6 command. REG_MARK = 26, CMD_MARK = 4.
;
; Build: nasm -f bin mark.asm -o mark.com
        cpu 8086
        org 0x100

REG_MARK        equ 26
CMD_MARK        equ 4

start:
        ; Command tail: PSP:0x80 = length byte, PSP:0x81.. = text, CR-terminated.
        mov     cl, [0x80]
        xor     ch, ch
        mov     si, 0x81
.skip:
        jcxz    .usage
        mov     al, [si]
        inc     si
        dec     cx
        cmp     al, ' '
        je      .skip
        cmp     al, 9                   ; tab
        je      .skip
        cmp     al, '0'
        jb      .usage
        cmp     al, '9'
        ja      .usage
        sub     al, '0'
        call    mark
        mov     ax, 0x4c00              ; AH=4Ch terminate, AL=0
        int     0x21

.usage:
        mov     ah, 0x09
        mov     dx, msg_usage
        int     0x21
        mov     ax, 0x4c01
        int     0x21

; mark: place phase boundary AL. Clobbers AX.
mark:
        mov     ah, al
        mov     al, REG_MARK
        out     0xE4, al                ; index = REG_MARK
        mov     al, ah
        out     0xE5, al                ; data = boundary id
        mov     al, CMD_MARK
        out     0xE6, al
        ret

msg_usage:      db 'Usage: MARK <0-9>', 13, 10, '$'
