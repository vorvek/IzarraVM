; sndtst.com — SP-4b M4 SB16-IRQ5-under-V86 e2e fixture. Runs in V86 under the
; TOKAEMM monitor (default-payload config).
;
; IRQ5 lands on vector 13 — the SAME vector as #GP, which the monitor uses for
; sensitive-instruction emulation. This fixture proves the monitor's
; discriminator: it hooks INT 0Dh, resets the DSP, then requests immediate
; 8-bit interrupts (DSP command 0xF2) while spinning a CLI/STI-dense loop, so
; IRQ5 deliveries interleave with genuine #GPs on the same vector. Each
; delivery must reach the guest handler exactly like real iron.
;
; Signals 0xA5 via the unit-tester exit port; 0xEn names the failed step.
;
; Build: nasm -f bin sndtst.asm -o sndtst.com
cpu 386
org 0x100
%define OK 0xA5

BASE    equ 0x220                 ; SET BLASTER=A220 I5 ...
RESETP  equ BASE+0x6
RDATA   equ BASE+0xA
WDATA   equ BASE+0xC
RSTATUS equ BASE+0xE
ROUNDS  equ 8
WAITCAP equ 1000000

start:
    ; 1. hook IVT 0x0D (IRQ5 on the DOS master-PIC base) + unmask IRQ5
    cli
    xor ax, ax
    mov ds, ax
    mov eax, [0x0D*4]
    mov [cs:old0d], eax
    mov word [0x0D*4], irq5_handler
    mov [0x0D*4+2], cs
    push cs
    pop ds
    in al, 0x21
    and al, 0xDF
    out 0x21, al
    sti

    ; 2. DSP reset handshake -> 0xAA on read-data
    mov dx, RESETP
    mov al, 1
    out dx, al
    mov cx, 64
.rst_hold:
    loop .rst_hold
    xor al, al
    out dx, al
    mov ecx, WAITCAP
.rst_wait:
    mov dx, RSTATUS
    in al, dx
    test al, 0x80                 ; data available?
    jnz .rst_read
    dec ecx
    jnz .rst_wait
    jmp f_reset
.rst_read:
    mov dx, RDATA
    in al, dx
    cmp al, 0xAA
    jne f_reset

    ; 3. ROUNDS x (request IRQ -> wait inside a CLI/STI storm)
    mov byte [cs:count], 0
    xor bl, bl                    ; expected handler count
.round:
    inc bl
    mov dx, WDATA
    mov al, 0xF2                  ; DSP: raise the 8-bit IRQ immediately
    out dx, al
    mov ecx, WAITCAP
.wait:
    cli                           ; each pair = two #GPs on vector 13 around a
    nop                           ; one-instruction delivery window
    sti
    nop
    cmp [cs:count], bl
    je .next
    dec ecx
    jnz .wait
    jmp f_noirq
.next:
    cmp bl, ROUNDS
    jb .round

    ; 4. restore the vector, report success
    cli
    xor ax, ax
    mov ds, ax
    mov eax, [cs:old0d]
    mov [0x0D*4], eax
    push cs
    pop ds
    sti
    mov al, OK
    jmp sig

; IRQ5 handler: ack the DSP (read 0x22E clears the 8-bit interrupt), count, EOI.
irq5_handler:
    push ax
    push dx
    mov dx, RSTATUS
    in al, dx
    inc byte [cs:count]
    mov al, 0x20
    out 0x20, al
    pop dx
    pop ax
    iret

f_reset: mov al, 0xE1
         jmp sig
f_noirq: mov al, 0xE2

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

old0d: dd 0
count: db 0
