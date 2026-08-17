; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; irq5ip0.com — V86-trap-tax regression fixture: IRQ5 delivered while the
; interrupted code's IP is EXACTLY 0. Runs in V86 under the TOKAEMM monitor
; (default-payload config).
;
; IRQ5 shares vector 13 with #GP. The monitor's OLD discriminator read the
; frame slot that holds a #GP's error code (always 0) or an IRQ frame's
; interrupted EIP; an IRQ arriving at IP == 0 made that slot 0 either way,
; and a slot-only build mis-routed it into the #GP path and hard-killed the
; VM (`signal32` = TestExit 144). An opcode-peek + PIC-probe fallback fixed
; it then; the current frame-ORIGIN basis (vec13_entry TEST 1, the no-error
; frame's EFLAGS.VM bit) decides it at any IP with no peek and no probe.
; This fixture stays as the regression pin for the historically hostile
; IP == 0 shape.
;
; To make IP == 0 the COMMON case deterministically, we use SB16 auto-init DMA
; playback (NOT the one-shot DSP 0xF2): once armed, the DMA block boundary
; raises IRQ5 continuously on the card's own schedule, with NO re-arm and NO
; frame surgery -- so the foreground is free to simply park on a 2-byte `jmp $`
; placed at offset 0 of its own segment. Every delivery that lands while parked
; samples return-IP 0 with the non-sensitive byte 0xEB at CS:0: exactly the
; case the buggy discriminator killed. We count ONLY those IP == 0 deliveries;
; after ROUNDS of them the handler redirects the parked frame to `done`.
;
; Signals 0xA5 via the unit-tester exit port; 0xEn names the failed step.
;
; Build: nasm -f bin irq5ip0.asm -o irq5ip0.com
cpu 386
org 0x100
%define OK 0xA5

BASE    equ 0x220                 ; SET BLASTER=A220 I5 D1 ...
RESETP  equ BASE+0x6             ; 0x226 DSP reset
RDATA   equ BASE+0xA            ; 0x22A DSP read data
WSTATUS equ BASE+0xC            ; 0x22C DSP write status / write command-data
RSTATUS equ BASE+0xE            ; 0x22E DSP read status (ack 8-bit IRQ)
ROUNDS  equ 8                     ; IP == 0 deliveries to observe
WAITCAP equ 2000000
SPIN_OFF equ 0x8000              ; the parked `jmp $` home inside our 64K image

start:
    ; 1. plant the 2-byte spin (`jmp $` = EB FE) at offset 0 of the paragraph
    ;    segment that aliases cs:SPIN_OFF, so the spin runs at seg2:0000.
    mov byte [SPIN_OFF], 0xEB
    mov byte [SPIN_OFF+1], 0xFE
    mov ax, cs
    add ax, SPIN_OFF >> 4
    mov [spin_seg], ax

    ; 2. hook IVT 0x0D (IRQ5 on the DOS master-PIC base) + unmask IRQ5
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
    and al, 0xDF                  ; unmask IRQ5
    out 0x21, al
    sti

    ; 3. DSP reset handshake -> 0xAA on read-data
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

    ; 4. program 8237 DMA channel 1 for an auto-init 8-bit read of a small
    ;    buffer. The bytes are irrelevant (we discard the audio); we only need
    ;    the block boundary to raise IRQ5 over and over. Auto-init reloads the
    ;    base addr/count at TC, so the counter never runs dry.
    mov al, 0x05                  ; mask channel 1 (bit2 set, chan 01)
    out 0x0A, al
    xor al, al
    out 0x0C, al                  ; clear the byte flip-flop
    mov al, 0x59                  ; mode: single(01)+auto-init(1<<4)+read(01<<2)+ch1
    out 0x0B, al                  ; = 0101 1001b: read, auto-init, channel 1
    ; the .COM segment is where cs points; DMA needs the physical byte address.
    ; Compute phys = (cs<<4)+dma_buf. cs<<4 fits 20 bits; keep it in eax.
    xor eax, eax
    mov ax, cs
    shl eax, 4
    add eax, dma_buf              ; eax = 20-bit physical base of dma_buf
    out 0x02, al                  ; ch1 base+current address low
    mov al, ah
    out 0x02, al                  ; ch1 base+current address high
    ; page register (A16-A23) for channel 1 is port 0x83
    shr eax, 16
    out 0x83, al
    ; count = DMABUF_LEN-1 (transfers = count+1)
    xor al, al
    out 0x0C, al                  ; clear flip-flop for the count pair
    mov ax, DMABUF_LEN-1
    out 0x03, al                  ; ch1 count low
    mov al, ah
    out 0x03, al                  ; ch1 count high
    mov al, 0x01                  ; unmask channel 1 (bit2 clear, chan 01)
    out 0x0A, al

    ; 5. DSP: set a fast sample rate + a small block, then arm 8-bit auto-init
    ;    output (0x1C). The block boundary raises IRQ5 continuously from here.
    mov byte [count], 0
    ; time constant (0x40): tc = 256 - 1000000/rate. Use a high rate so the
    ; small block drains fast and IRQs come often. tc=0xA6 ~= 11kHz.
    mov al, 0x40
    call dsp_write
    mov al, 0xA6
    call dsp_write
    ; block size (0x48): low, high. A small block => frequent block-boundary IRQ.
    mov al, 0x48
    call dsp_write
    mov al, (DSP_BLOCK-1) & 0xFF
    call dsp_write
    mov al, ((DSP_BLOCK-1) >> 8) & 0xFF
    call dsp_write
    mov al, 0x1C                  ; 8-bit auto-init output: raises IRQ5 on TC,
    call dsp_write                ; and auto-reloads -> a continuous IRQ5 stream

    ; 6. park at seg2:0000. From here the foreground only ever executes the
    ;    `jmp $` at IP 0, so every auto-init IRQ5 interrupts at IP == 0.
    push word [spin_seg]
    push word 0
    retf                          ; far jump to seg2:0000

done:
    ; 7. silence the card, restore the vector, report success
    mov al, 0xD0                  ; DSP: halt DMA
    call dsp_write
    mov al, 0x05                  ; re-mask channel 1
    out 0x0A, al
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

; Write AL to the DSP command/data port once write-status bit7 is clear.
dsp_write:
    push dx
    push cx
    mov ah, al
    mov dx, WSTATUS
    mov ecx, WAITCAP
.wait:
    in al, dx
    test al, 0x80
    jz .rdy
    dec ecx
    jnz .wait
.rdy:
    mov al, ah
    out dx, al
    pop cx
    pop dx
    ret

; IRQ5 handler. Ack the DSP 8-bit interrupt (read 0x22E), EOI, and count ONLY
; the deliveries whose frame is seg2:0000 (the IP == 0 case under test). Auto-
; init needs no re-arm, so the handler just returns; the parked foreground is
; re-interrupted at IP 0 on the next block boundary. After ROUNDS counted
; deliveries, rewrite the return frame to `done`.
irq5_handler:
    push bp
    mov bp, sp
    push ax
    push dx
    mov dx, RSTATUS               ; ack the 8-bit DSP interrupt
    in al, dx
    mov al, 0x20                  ; EOI the master PIC
    out 0x20, al
    cmp word [bp+2], 0            ; frame IP == 0?
    jne .out
    mov ax, [cs:spin_seg]
    cmp [bp+4], ax                ; ... of the spin segment?
    jne .out
    inc byte [cs:count]
    cmp byte [cs:count], ROUNDS
    jb .out
    mov word [bp+2], done         ; enough IP == 0 hits: return to `done`
    mov [bp+4], cs
.out:
    pop dx
    pop ax
    pop bp
    iret

f_reset: mov al, 0xE1
         jmp sig

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

DSP_BLOCK  equ 64                 ; DSP block size (samples per IRQ boundary)
DMABUF_LEN equ 256                ; DMA buffer length in bytes

old0d:    dd 0
count:    db 0
spin_seg: dw 0
align 16
dma_buf:  times DMABUF_LEN db 0x80    ; unsigned-PCM midscale (silence)
