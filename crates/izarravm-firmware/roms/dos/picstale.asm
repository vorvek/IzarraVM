; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; picstale.com: routing row 3 (design section 3.5), the stale-bookkeeping
; construction. The monitor caches the 8259A's vector bases in its own
; vcpi_pic_master/vcpi_pic_slave words and reflects a hardware IRQ to
; cache_base + line. The cache is only written by the VCPI DE0Ah/DE0Bh calls --
; but the PIC ports are NOT trapped in the monitor's TSS I/O-permission bitmap
; (only 0x92 is), so a guest can reprogram the chip straight through an ICW
; sequence and leave the cache saying something the chip does not.
;
; The right answer is that the ARRIVING VECTOR decides: a request that entered
; through IDT vector 8 is IVT[8]'s, whatever the monitor last recorded.
;
; Construction:
;   1. DE0B the master to 0x88 -- both the cache and the chip move;
;   2. a direct ICW sequence on 0x20/0x21 puts the CHIP back to base 8, leaving
;      the cache stale at 0x88;
;   3. let a real IRQ0 fire and see which IVT slot runs.
;
; IVT[0x88] gets a decoy handler (EOI + IRET) so the wrong answer is REPORTED
; rather than crashing the guest through whatever byte pair was in that slot.
;
; Signals 0xA5 when IVT[8] ran. 0xE1 = IVT[0x88] ran instead: the monitor
; reflected the arriving line through its stale cache. 0xD1 (DE0B refused) and
; 0xD2 (no timer interrupt arrived at all within the bound) are setup.
;
; Build: nasm -f bin picstale.asm -o picstale.com
cpu 386
org 0x100
%define OK 0xA5

; Spin bound. gsw_386 is the project's 386DX-at-22-MHz reference
; (bench_reference.rs, "Project reference: 386DX at 22 MHz"), so one 54.9 ms
; timer tick is 22e6 * 0.0549 = ~1.2M
; emulated cycles. This loop is two CMP mem,imm plus DEC + JNZ, ~12 cycles per
; iteration, so a tick is ~100k iterations and the bound covers ~80 of them:
; exhaustion means no timer interrupt arrived at all, not that the fixture was
; impatient.
%define MEM_SPIN 8000000

start:
    xor ax, ax
    mov es, ax
    cli
    mov ax, [es:8*4]
    mov [old8], ax
    mov ax, [es:8*4+2]
    mov [old8+2], ax
    mov ax, [es:0x88*4]
    mov [old88], ax
    mov ax, [es:0x88*4+2]
    mov [old88+2], ax
    mov word [es:8*4], h8
    mov [es:8*4+2], cs
    mov word [es:0x88*4], h88
    mov [es:0x88*4+2], cs

    in al, 0x21
    mov [mask_m], al
    in al, 0xA1
    mov [mask_s], al

    ; ---- 1. DE0B: cache and chip both move to 0x88 / 0x90
    mov ax, 0xDE0B
    mov bx, 0x88
    mov cx, 0x90
    int 0x67
    mov byte [moved], 1
    or ah, ah
    jnz f_de0b

    ; ---- 2. direct ICW sequence: the CHIP goes back to base 8, the cache does
    ;         not. Untrapped ports, so this is the real 8259A.
    mov al, 0x11                  ; ICW1: edge, cascade, ICW4 to follow
    out 0x20, al
    mov al, 0x08                  ; ICW2: master base 8
    out 0x21, al
    mov al, 0x04                  ; ICW3: slave on IR2
    out 0x21, al
    mov al, 0x01                  ; ICW4: 8086 mode
    out 0x21, al
    mov al, [mask_m]              ; OCW1: the masks the ICW sequence cleared
    out 0x21, al

    ; ---- 3. let a real IRQ0 in and see where it lands
    mov byte [hit8], 0
    mov byte [hit88], 0
    sti
    mov ecx, MEM_SPIN
.wait:
    cmp byte [hit8], 0
    jne .landed
    cmp byte [hit88], 0
    jne .landed
    dec ecx
    jnz .wait
    cli
    mov bl, 0xD2
    jmp restore
.landed:
    cli
    mov bl, OK
    cmp byte [hit8], 0
    jne restore
    mov bl, 0xE1
    jmp restore

f_de0b: mov bl, 0xD1

restore:
    cmp byte [moved], 0
    je .ivt
    push bx
    mov ax, 0xDE0B                ; resync cache and chip on the DOS defaults
    mov bx, 0x08
    mov cx, 0x70
    int 0x67
    pop bx
    mov al, [mask_m]
    out 0x21, al
    mov al, [mask_s]
    out 0xA1, al
.ivt:
    xor ax, ax
    mov es, ax
    mov ax, [old8]
    mov [es:8*4], ax
    mov ax, [old8+2]
    mov [es:8*4+2], ax
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

; The right landing site: mark, then chain to DOS's own tick handler, which owns
; the EOI and the IRET.
h8:
    mov byte [cs:hit8], 1
    jmp far [cs:old8]

; The decoy. Nothing should route a base-8 IRQ0 here; if something does, EOI and
; return so the fixture can report it instead of the guest dying in the slot.
h88:
    push ax
    mov byte [cs:hit88], 1
    mov al, 0x20
    out 0x20, al
    pop ax
    iret

old8:   dd 0
old88:  dd 0
mask_m: db 0
mask_s: db 0
moved:  db 0
hit8:   db 0
hit88:  db 0
