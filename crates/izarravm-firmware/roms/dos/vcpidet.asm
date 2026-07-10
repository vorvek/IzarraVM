; vcpidet.com: TOKAEMM VCPI presence fixture. Runs in V86 under a bare
; DEVICE=C:\DOS\TOKAEMM.SYS (frameless default): the manager must answer the
; VCPI presence call even without an EMS pool (the EMM386-NOEMS precedent),
; refuse a not-yet-implemented subfunction with 8Fh, preserve untouched
; registers across the call, and keep the plain EMS interface working on the
; shared INT 67h.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin vcpidet.asm -o vcpidet.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; 1. VCPI presence (DE00h): AH=0, BH=1 (major), BL=0 (minor)
    mov dx, 0x1234               ; canary: spec says DX is unchanged on output
    mov ax, 0xDE00
    int 0x67
    or ah, ah
    jnz f_pres
    cmp bx, 0x0100
    jne f_pres

    ; 2. register preservation across the VCPI call
    cmp dx, 0x1234
    jne f_regs

    ; 3. undefined/not-yet-implemented subfunction: AH=8Fh
    mov ax, 0xDE7F
    int 0x67
    cmp ah, 0x8F
    jne f_undef

    ; 4. plain EMS still answers on the shared vector (version 46h -> 4.0)
    mov ah, 0x46
    int 0x67
    or ah, ah
    jnz f_ems
    cmp al, 0x40
    jne f_ems

    mov al, OK
    jmp sig

f_pres:   mov al, 0xE1
          jmp sig
f_regs:   mov al, 0xE2
          jmp sig
f_undef:  mov al, 0xE3
          jmp sig
f_ems:    mov al, 0xE4

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h
