; mousetst.com — SP-4b M4 mouse-under-V86 e2e fixture. Runs from AUTOEXEC after
; LH TOKAMOUS (the default AUTOEXEC loads the INT 33h driver into a TOKAEMM UMB)
; with the whole system in V86 under the monitor.
;
; Proves the slave-PIC path end-to-end under virtualization: host
; inject_mouse_wheel -> 8042 -> IRQ12 -> vector 0x74 -> the monitor's slave
; reflect stub -> guest INT 74h ISR -> TOKAMOUS packet handler -> INT 33h fn 03h
; wheel counter. Signals 0xA5 via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin mousetst.asm -o mousetst.com
cpu 386
org 0x100
%define OK 0xA5

POLL_CAP equ 2000000              ; generous: the host injects between run chunks

start:
    ; 1. driver present: INT 33h reset returns AX=0xFFFF
    xor ax, ax
    int 0x33
    cmp ax, 0xFFFF
    jne f_nodrv

    ; 2. wheel API present: AX=0x0011 -> AX=0x574D ("WM", CuteMouse API)
    mov ax, 0x0011
    int 0x33
    cmp ax, 0x574D
    jne f_api

    ; 3. poll fn 03h until the signed wheel counter (BH) goes nonzero. The host
    ;    injects positive detents once the boot settles; each fn 03h consumes
    ;    the accumulated count.
    mov ecx, POLL_CAP
.poll:
    push ecx
    mov ax, 0x0003
    int 0x33
    pop ecx
    test bh, bh
    jnz .got
    dec ecx
    jnz .poll
    jmp f_none                    ; cap hit: no wheel event ever arrived

.got:
    test bh, 0x80                 ; positive detents must read positive
    jnz f_sign

    mov al, OK
    jmp sig

f_nodrv: mov al, 0xE1
         jmp sig
f_api:   mov al, 0xE2
         jmp sig
f_none:  mov al, 0xE3
         jmp sig
f_sign:  mov al, 0xE4

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h
