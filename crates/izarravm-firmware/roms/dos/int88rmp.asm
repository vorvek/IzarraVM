; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; int88rmp.com: routing row 2 (design section 3.5). Once a VCPI client has moved
; the master's base to 0x88, vector 0x88 means two different things: it is where
; IRQ0 now arrives, and it is still an ordinary software-interrupt vector the
; guest may call. A guest `INT 88h` must reach IVT[0x88] as a software interrupt
; -- it must NOT be converted into "IRQ line 0" and it must not make anything
; EOI the chip on the guest's behalf.
;
; This is a PIN of the round-trip identity rather than a red fixture: the
; monitor's default-gate arm re-derives the line by consulting the chip's own
; ISR, so it is self-correcting for the software-INT case, and the software INT
; itself is reflected straight from the #GP body without ever consulting the PIC
; bookkeeping. It is written to hold that identity down while the routing around
; it is rebuilt.
;
; The discriminating precondition is that IRQ0 is genuinely IN SERVICE when the
; INT 88h executes: that is the only state in which a line-number derivation
; could plausibly claim the vector. The fixture CLIs, waits for the chip to show
; IS0, and only then executes INT 88h; it then re-reads the ISR to prove nothing
; EOI'd behind its back.
;
; Signals 0xA5. 0xE1 = INT 88h did not reach IVT[0x88]; 0xE2 = the in-service
; line was EOI'd by something other than the guest. 0xD1 (DE0B refused) and 0xD2
; (no line ever went into service, so the fixture would prove nothing) are
; setup.
;
; Build: nasm -f bin int88rmp.asm -o int88rmp.com
cpu 386
org 0x100
%define OK 0xA5

start:
    xor ax, ax
    mov es, ax
    cli
    mov ax, [es:0x88*4]
    mov [old88], ax
    mov ax, [es:0x88*4+2]
    mov [old88+2], ax
    mov word [es:0x88*4], h88
    mov [es:0x88*4+2], cs

    ; ---- 1. master to 0x88. The lines stay UNMASKED on purpose: the fixture
    ;         needs a real IRQ0 to go into service underneath it.
    mov ax, 0xDE0B
    mov bx, 0x88
    mov cx, 0x90
    int 0x67
    mov byte [moved], 1
    or ah, ah
    jnz f_de0b

    ; ---- 2. wait until IRQ0 is actually in service
    mov ecx, 2000000
.wait_isr:
    mov al, 0x0B                  ; OCW3: read-select ISR
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .in_service
    dec ecx
    jnz .wait_isr
    mov bl, 0xD2
    jmp restore

    ; ---- 3. the software interrupt on the remapped vector
.in_service:
    mov byte [hit], 0
    int 0x88
    cmp byte [hit], 1
    jne f_route

    ; ---- 4. and nothing may have EOI'd the in-service line for us
    mov al, 0x0B
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jz f_eoi

    mov bl, OK
    jmp restore

f_route: mov bl, 0xE1
         jmp restore
f_eoi:   mov bl, 0xE2
         jmp restore
f_de0b:  mov bl, 0xD1

restore:
    cmp byte [moved], 0
    je .ivt
    push bx
    mov ax, 0xDE0B                ; back to the DOS defaults; the line still in
    mov bx, 0x08                  ; service is delivered to IVT[8] on the STI
    mov cx, 0x70                  ; below, and DOS's own handler EOIs it
    int 0x67
    pop bx
.ivt:
    xor ax, ax
    mov es, ax
    mov ax, [old88]
    mov [es:0x88*4], ax
    mov ax, [old88+2]
    mov [es:0x88*4+2], ax
    sti

    mov al, 12
    out 0xE4, al                  ; REG_EXIT
    mov al, bl
    out 0xE5, al                  ; code
    mov al, 3
    out 0xE6, al                  ; CMD_EXIT
.h: jmp .h

; A SOFTWARE-interrupt handler: it must not EOI, because no line of its own is
; in service. If the monitor mistook the INT for IRQ0 this would still run, so
; the ISR re-read in step 4 is what separates the two.
h88:
    mov byte [cs:hit], 1
    iret

old88: dd 0
moved: db 0
hit:   db 0
