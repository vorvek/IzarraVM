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
; itself is reflected straight out of the #GP body without ever consulting the
; PIC bookkeeping. It is written to hold that identity down while the routing
; around it is rebuilt.
;
; The discriminating precondition is that IRQ0 is genuinely IN SERVICE when the
; INT 88h executes: that is the only state in which a line-number derivation
; could plausibly claim the vector. Two things constrain how to get there.
;
; First, it must be obtained THE LEGAL WAY -- by being inside the guest's own
; interrupt handler, where the chip really has IS0 set because the guest's own
; tick was acknowledged for it. A fixture that instead CLI'd and spun waiting
; for IS0 to appear would be relying on the monitor pinning the real IF open and
; acknowledging early, which is precisely what this campaign deletes: it would
; start failing on its own setup step the moment the monitor is fixed.
;
; Second, the remap has to come FIRST. DE0B reprograms the 8259A with a full ICW
; sequence, and an ICW1 resets the chip -- in-service state included. So there
; is no "hold a line in service, then remap"; the only reachable form of the
; state is "remap, then take a tick at the new base", which is also the shape a
; real VCPI client's clock lives in. The fixture therefore hooks IVT[0x88],
; DE0Bs the master to 0x88, and waits for a genuine hardware IRQ0 to arrive
; there.
;
; That leaves IVT[0x88] serving both entries, so a `phase` byte tells them
; apart, and the ambiguity cannot bite: the hardware entry runs with IF clear
; (an IVT dispatch clears it) and with IS0 -- the highest priority level --
; inhibiting the chip until the EOI the chained DOS handler issues on the way
; out, so nothing but the fixture's own `INT 88h` can re-enter during the body.
;
; Signals 0xA5. 0xE1 = INT 88h did not reach IVT[0x88]; 0xE2 = the in-service
; line was EOI'd across it by something other than the guest. 0xD1 (DE0B
; refused), 0xD2 (no hardware IRQ0 ever arrived at the remapped vector) and 0xD3
; (IS0 was not in service when the handler looked, so the fixture would prove
; nothing) are setup, kept distinct from the assertions.
;
; Build: nasm -f bin int88rmp.asm -o int88rmp.com
cpu 386
org 0x100
%define OK 0xA5

; Spin bound. The gsw_386 profile clocks ~25 MHz, so one 54.9 ms timer tick is
; ~1.4M emulated cycles. This loop is CMP mem,imm + DEC + JNZ, ~10 cycles per
; iteration, so a tick is ~140k iterations and the bound covers ~55 of them:
; exhaustion means no tick ever reached the remapped vector, not that the
; fixture was impatient.
%define MEM_SPIN 8000000

start:
    xor ax, ax
    mov es, ax
    cli
    mov ax, [es:8*4]              ; DOS's own tick handler: the hardware entry
    mov [old8], ax                ; below chains to it for the EOI and the IRET
    mov ax, [es:8*4+2]
    mov [old8+2], ax
    mov ax, [es:0x88*4]
    mov [old88], ax
    mov ax, [es:0x88*4+2]
    mov [old88+2], ax
    mov word [es:0x88*4], h88
    mov [es:0x88*4+2], cs

    ; ---- 1. master to 0x88, BEFORE anything is in service (the ICW sequence
    ;         DE0B runs would clear the ISR anyway)
    mov byte [moved], 1           ; set before the call that can leave it moved
    mov ax, 0xDE0B
    mov bx, 0x88
    mov cx, 0x90
    int 0x67
    or ah, ah
    jnz f_de0b

    ; ---- 2. let a genuine hardware IRQ0 arrive at the new base. Everything
    ;         else happens inside h88's hardware arm.
    mov byte [phase], 1
    sti
    mov ecx, MEM_SPIN
.wait:
    cmp byte [done], 0
    jne .ran
    dec ecx
    jnz .wait
    mov bl, 0xD2
    jmp restore
.ran:
    mov bl, [code]
    jmp restore

f_de0b: mov bl, 0xD1

restore:
    cli
    cmp byte [moved], 0
    je .ivt
    push bx
    mov ax, 0xDE0B                ; back to the DOS defaults, chip and monitor
    mov bx, 0x08                  ; bookkeeping together
    mov cx, 0x70
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

; IVT[0x88] serves both entries while the master sits at base 0x88. `phase`
; says which one this is: 1 = the hardware IRQ0 the fixture is waiting for,
; 3 = the software INT 88h the hardware arm issues from inside its own
; in-service window. Anything else is a later tick and is passed straight on.
h88:
    pushad
    cmp byte [cs:phase], 3
    je .sw
    cmp byte [cs:phase], 1
    jne .chain

    ; ---- the hardware arm: IS0 is set for real and IF is clear
    mov al, 0x0B                  ; OCW3: read-select ISR
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .in_service
    mov byte [cs:code], 0xD3
    jmp .finished
.in_service:

    ; the software interrupt on the remapped vector, from inside that window
    mov byte [cs:sw_hit], 0
    mov byte [cs:phase], 3
    int 0x88
    mov byte [cs:phase], 4
    cmp byte [cs:sw_hit], 1
    je .routed
    mov byte [cs:code], 0xE1
    jmp .finished
.routed:

    ; and nothing may have EOI'd the in-service line for us
    mov al, 0x0B
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .clean
    mov byte [cs:code], 0xE2
    jmp .finished
.clean:
    mov byte [cs:code], OK
.finished:
    mov byte [cs:phase], 4
    mov byte [cs:done], 1
.chain:
    popad
    jmp far [cs:old8]

    ; The SOFTWARE arm. It must not EOI: no line of its own is in service. If
    ; the monitor had mistaken the INT for IRQ0 this would still run, so the ISR
    ; re-read above is what separates the two.
.sw:
    mov byte [cs:sw_hit], 1
    popad
    iret

old8:   dd 0
old88:  dd 0
moved:  db 0
phase:  db 0
sw_hit: db 0
done:   db 0
code:   db 0
