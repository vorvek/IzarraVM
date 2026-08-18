; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; isrset.com: the OTHER half of the E10 rule, and a REGRESSION GUARD rather than
; a red fixture. Whatever the monitor does while the guest has interrupts
; disabled, once the guest's own IVT handler is finally running for line N, the
; 8259A must show IS_N SET -- the state a real chip is in between INTA and EOI.
; DJGPP's shared hardware-IRQ wrapper reads exactly this (OCW3 0x0B + IN 0x20)
; to tell a real IRQ from a spurious entry; the 0 the old early-EOI path left
; there sent it down its not-my-line branch, into a 16-entry table indexed with
; 16 and a RETF through the pair it found -- E10, MonikaTT's #GP(0) at 0xAF:78A3.
;
; Structure: hook IVT[8] with a handler that reads the ISR BEFORE any EOI and
; stashes it, then chains to the previous handler (which owns the EOI and the
; IRET). CLI, poll the IRR through OCW3 0x0A until IR0 is requested -- proving a
; tick is genuinely outstanding rather than waiting on nothing -- then STI and
; wait for the stash.
;
; Signals 0xA5 when the stashed byte has bit 0 SET. 0xE1 is the assertion
; failing (handler ran with its own line NOT in service). 0xD1 (no request ever
; appeared) and 0xD2 (the handler never ran) are setup, kept distinct so neither
; can be read as the assertion.
;
; Build: nasm -f bin isrset.asm -o isrset.com
cpu 386
org 0x100
%define OK 0xA5

start:
    xor ax, ax
    mov es, ax
    cli
    mov ax, [es:8*4]
    mov [old8], ax
    mov ax, [es:8*4+2]
    mov [old8+2], ax
    mov word [es:8*4], tick
    mov [es:8*4+2], cs
    mov byte [stash], 0
    mov byte [hit], 0

    ; ---- wait until IR0 is REQUESTED, so the STI below has something to let in
    mov ecx, 2000000
.wait_irr:
    mov al, 0x0A                  ; OCW3: read-select IRR
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .requested
    dec ecx
    jnz .wait_irr
    mov bl, 0xD1
    jmp restore
.requested:
    sti

    ; ---- wait for the handler to record what the chip showed it
    mov ecx, 8000000
.wait_hit:
    cmp byte [hit], 0
    jne .ran
    dec ecx
    jnz .wait_hit
    mov bl, 0xD2
    jmp restore
.ran:
    mov bl, OK
    test byte [stash], 0x01
    jnz restore
    mov bl, 0xE1                  ; the guest's handler ran with IS0 clear

restore:
    cli
    xor ax, ax
    mov es, ax
    mov ax, [old8]
    mov [es:8*4], ax
    mov ax, [old8+2]
    mov [es:8*4+2], ax
    sti

    mov al, 12
    out 0xE4, al                  ; REG_EXIT
    mov al, bl
    out 0xE5, al                  ; code
    mov al, 3
    out 0xE6, al                  ; CMD_EXIT
.h: jmp .h

; The guest's own timer handler. Reads the ISR BEFORE anything EOIs, then falls
; through to the previous handler, which issues the EOI and the IRET.
tick:
    push ax
    mov al, 0x0B                  ; OCW3: read-select ISR
    out 0x20, al
    in al, 0x20
    mov [cs:stash], al
    mov byte [cs:hit], 1
    pop ax
    jmp far [cs:old8]

old8:  dd 0
stash: db 0
hit:   db 0
