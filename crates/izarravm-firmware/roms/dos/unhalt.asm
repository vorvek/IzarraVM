; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; UNHALT.COM - make the BIOS keyboard wait spin instead of halt.
;
; INT 16h AH=00h/10h halt the CPU while the keyboard buffer is empty, waking on
; IRQ1 (see kbd-bios-core.inc). That is free on real silicon but enormous here:
; a spinning wait is interpreted guest code, measured at 1,533,694,739 retired
; instructions against 24,801,209 over the same guest-clock budget.
;
; A halt is not identical to a spin, though, and this is the escape hatch for
; the difference. Two ways a program can tell:
;
;   * It masks IRQ0 and IRQ1 and then blocks. A spin loops forever; a halt has
;     nothing left to wake it. Both are hung -- no key can ever arrive -- but
;     they hang differently.
;   * It relies on guest time advancing smoothly across the wait rather than in
;     18.2 Hz steps.
;
; Neither is common, and a program old enough to care is old enough that the
; interpreter serves it comfortably. Hence a switch rather than a compromise.
;
; NOT a TSR: the flag lives in the BDA at 0040:00B4, which persists for as long
; as the machine runs and which the ROM re-reads on every wait, so this sets a
; byte and exits. Nothing stays resident and no interrupt is hooked.
;
; This covers the BIOS wait only. The Toka-DOS kernel's own idle halt is a
; separate thing with its own documented switch, IDLEHALT=0 in CONFIG.SYS; it
; fires while DOS waits for CON input, which is a path games rarely take.
;
; Usage:  UNHALT      make the keyboard wait spin
;         UNHALT /H   restore halting (the default)
;         UNHALT /?   usage
;
; Build: nasm -f bin unhalt.asm -o unhalt.com
        cpu 8086
        org 0x100

BDA_SEG         equ 0x0040
KB_NOHALT       equ 0x00b4

start:
        cld
        ; Scan the PSP command tail for a switch. 0x80 holds its length, the
        ; text starts at 0x81 and is terminated by CR.
        mov cl, [0x80]
        xor ch, ch
        mov si, 0x81
        xor bl, bl                  ; bl = 0 -> set the flag (spin)
        jcxz .apply
.scan:
        lodsb
        cmp al, '/'
        je .switch
        cmp al, '-'                 ; accept -H as well as /H
        je .switch
        loop .scan
        jmp .apply
.switch:
        dec cx
        jz .usage                   ; a bare '/' with nothing after it
        lodsb
        ; Fold to upper case so /h and /H both work.
        cmp al, 'a'
        jb .no_fold
        cmp al, 'z'
        ja .no_fold
        sub al, 0x20
.no_fold:
        cmp al, 'H'
        je .restore
        jmp .usage

.restore:
        mov bl, 1                   ; bl = 1 -> clear the flag (halt again)
.apply:
        mov ax, BDA_SEG
        mov ds, ax
        test bl, bl
        jnz .set_halt
        mov byte [KB_NOHALT], 1
        mov dx, msg_spin
        jmp .say
.set_halt:
        mov byte [KB_NOHALT], 0
        mov dx, msg_halt
.say:
        push cs
        pop ds                      ; DS:DX for INT 21h AH=09h
        mov ah, 0x09
        int 0x21
        mov ax, 0x4c00
        int 0x21

.usage:
        push cs
        pop ds
        mov dx, msg_usage
        mov ah, 0x09
        int 0x21
        mov ax, 0x4c01              ; nonzero: a .BAT can tell it did nothing
        int 0x21

msg_spin  db 'BIOS keyboard wait: spinning (HLT off).', 13, 10, '$'
msg_halt  db 'BIOS keyboard wait: halting (default).', 13, 10, '$'
msg_usage db 'UNHALT - make the BIOS keyboard wait spin instead of halt.', 13, 10
          db 13, 10
          db '  UNHALT      spin while waiting for a key', 13, 10
          db '  UNHALT /H   halt while waiting (the default)', 13, 10
          db 13, 10
          db 'For the DOS kernel idle halt, use IDLEHALT=0 in CONFIG.SYS.', 13, 10, '$'
