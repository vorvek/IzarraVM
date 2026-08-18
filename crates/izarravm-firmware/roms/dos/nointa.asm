; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; nointa.com: the E10 metal invariant, probed from the guest side. While a V86
; guest holds interrupts disabled, NOTHING may acknowledge the 8259A on its
; behalf: an INTA is the guest's to issue, through its own IVT handler, once it
; re-enables. The master ISR must therefore read 00 for the whole CLI window,
; however many timer requests pile up in the IRR behind it.
;
; The fixture is SELF-PROVING, because a CLI'd guest that reads the ISR too
; early reads 00 for the wrong reason (nothing has happened yet). So it first
; polls the IRR through OCW3 0x0A until IR0 is REQUESTED; reaching the ISR read
; at all means a timer request demonstrably exists. Only then does it look.
;
; The PIC ports are not in the monitor's TSS I/O-permission bitmap (only 0x92
; is), so both the OUTs and the INs here reach the real chip.
;
; Expected on a monitor that runs its V86 guest at real IOPL 3: the guest's CLI
; clears the real IF, the request sits in the IRR untaken, ISR reads 00 -> 0xA5.
; Expected on the virtual-IF monitor: the real IF is pinned open, so the FIRST
; tick is acknowledged (ISR bit 0 set) and parked in `vip` the instant it
; appears, the SECOND tick is what finally shows up in the IRR, and the ISR read
; returns 01 -> 0xE1. 0xD1 is the third possibility -- no request ever became
; visible in the IRR -- kept distinct so an exhausted poll can never be read as
; either answer.
;
; Signals via the unit-tester exit port.
;
; Build: nasm -f bin nointa.asm -o nointa.com
cpu 386
org 0x100
%define OK 0xA5

start:
    cli                           ; on the virtual-IF monitor this is a #GP that
                                  ; only clears VIF; the real IF stays open

    ; ---- 1. wait until IR0 is REQUESTED (the proof the ISR read is not early)
    mov ecx, 2000000              ; bound: many 54.9 ms ticks of guest time
.wait_irr:
    mov al, 0x0A                  ; OCW3: read-select IRR
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .requested
    dec ecx
    jnz .wait_irr
    mov bl, 0xD1                  ; no request ever appeared: prove nothing
    jmp done

    ; ---- 2. with a request demonstrably outstanding, the ISR must be empty
.requested:
    mov al, 0x0B                  ; OCW3: read-select ISR
    out 0x20, al
    in al, 0x20
    test al, 0x01
    jnz .acknowledged
    mov bl, OK
    jmp done
.acknowledged:
    mov bl, 0xE1                  ; something INTA'd on the guest's behalf

done:
    sti                           ; hand the line back to DOS's own tick handler
    mov al, 12
    out 0xE4, al                  ; REG_EXIT
    mov al, bl
    out 0xE5, al                  ; code
    mov al, 3
    out 0xE6, al                  ; CMD_EXIT
.h: jmp .h
