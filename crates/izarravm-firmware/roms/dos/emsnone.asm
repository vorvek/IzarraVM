; emsnone.com — SP-4b M2 default-off EMS contract fixture. Runs in V86 under
; TOKAEMM loaded with a bare DEVICE=C:\TOKAEMM.SYS (no RAM argument): the
; manager must answer INT 67h as a FRAMELESS EMM386-NOEMS-style manager —
; present, version 4.0, zero pages, no page frame, allocation refused.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin emsnone.asm -o emsnone.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; 1. version (46h): present and 4.0 even without a frame
    mov ah, 0x46
    int 0x67
    or ah, ah
    jnz f_ver
    cmp al, 0x40
    jne f_ver

    ; 2. page frame (41h): no frame -> AH = 80h (EMM386 NOEMS convention)
    mov ah, 0x41
    int 0x67
    cmp ah, 0x80
    jne f_frame

    ; 3. page counts (42h): zero free, zero total, status OK
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_counts
    or bx, bx
    jnz f_counts
    or dx, dx
    jnz f_counts

    ; 4. allocate one page (43h): refused with 87h (more than the zero total)
    mov ah, 0x43
    mov bx, 1
    int 0x67
    cmp ah, 0x87
    jne f_alloc

    mov al, OK
    jmp sig

f_ver:    mov al, 0xE1
          jmp sig
f_frame:  mov al, 0xE2
          jmp sig
f_counts: mov al, 0xE3
          jmp sig
f_alloc:  mov al, 0xE4

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h
