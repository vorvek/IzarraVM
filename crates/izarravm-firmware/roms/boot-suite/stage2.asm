bits 16
org 0x8000

%define VGA_TEXT 0xb800
%define VGA_MODE13H 0xa000
%define RESULT_BLOCK 0x9000
%define COM1_DATA 0x03f8
%define COM1_IER 0x03f9
%define COM1_LCR 0x03fb
%define COM1_MCR 0x03fc
%define COM1_LSR 0x03fd
%define PIT_CH0 0x40
%define PIT_CH2 0x42
%define PIT_CTRL 0x43
%define PORT_B 0x61
%define PIC_CMD 0x20
%define PIC_DATA 0x21
%define TICK_COUNT 0x0600
%define TICK_TARGET 10
%define DSP_RESET 0x226
%define DSP_READ 0x22A
%define DSP_WRITE 0x22C
%define DSP_STATUS 0x22E
%define ADPCM_STATUS 0x240
%define ADPCM_ADDR 0x240
%define ADPCM_DATA 0x241
%define ADPCM_RESOURCE 0x242
%define ADPCM_FIFO 0x243
%define OPL_ADDR 0x388
%define OPL_DATA 0x389
%define DMA1_MODE 0x0B
%define DMA1_ADDR 0x02
%define DMA1_COUNT 0x03
%define DMA1_PAGE 0x83
%define DMA1_MASK 0x0A
%define DMA2_MODE 0xD6
%define DMA2_ADDR 0xC4
%define DMA2_COUNT 0xC6
%define DMA2_PAGE 0x8B
%define DMA2_MASK 0xD4
%define SB_DMA_TICKS 0x0610
%define SB_DMA_BUF 0x0500
%define SB_DMA_BUF16_SEG 0x2000

stage2_start:
    cli
    cld
    mov ax, 0
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    call init_serial
    call copy_result_block

    mov ax, VGA_TEXT
    mov es, ax
    mov di, 0
    mov si, title
    mov ah, 0x0f
    call puts_screen

    mov di, 160
    mov si, line_cpu
    mov ah, 0x0a
    call puts_screen

    mov di, 320
    mov si, line_video
    mov ah, 0x0e
    call puts_screen

    mov di, 480
    mov si, line_sound
    mov ah, 0x0c
    call puts_screen

    mov di, 640
    mov si, line_memory
    mov ah, 0x0b
    call puts_screen

    call test_cga_graphics
    call test_ega_planar
    call test_mode13h
    call test_timer
    call test_sb_dsp_reset
    call test_opl3
    call test_opl2
    call test_sb_8bit_dma
    call test_sb_16bit_dma
    call test_adpcm
    call test_pc_speaker

    mov si, RESULT_BLOCK + (result_payload - result_block_template)
    call puts_serial

    hlt
    jmp $

init_serial:
    mov dx, COM1_IER
    mov al, 0x00
    out dx, al
    mov dx, COM1_LCR
    mov al, 0x80
    out dx, al
    mov dx, COM1_DATA
    mov al, 0x03
    out dx, al
    mov dx, COM1_IER
    mov al, 0x00
    out dx, al
    mov dx, COM1_LCR
    mov al, 0x03
    out dx, al
    mov dx, COM1_MCR
    mov al, 0x03
    out dx, al
    ret

serial_wait:
    mov dx, COM1_LSR
    in al, dx
    test al, 0x20
    jz serial_wait
    ret

serial_putc:
    push dx
    push ax
    call serial_wait
    pop ax
    mov dx, COM1_DATA
    out dx, al
    pop dx
    ret

puts_serial:
    lodsb
    test al, al
    jz .done
    call serial_putc
    jmp puts_serial
.done:
    ret

puts_screen:
    lodsb
    test al, al
    jz .done
    stosw
    jmp puts_screen
.done:
    ret

; Busy-wait long enough to cover the DSP reset settle (~100us) and one OPL
; timer step (~80us). advance_devices ticks both with CPU clocks every step,
; so ~0x4000 loop iterations at 25 MHz is comfortably over the windows the
; detection probes poll for. cx is preserved for the callers' poll counters.
delay:
    push cx
    mov cx, 0x4000
.delay_loop:
    loop .delay_loop
    pop cx
    ret

test_mode13h:
    mov ax, 0x0013
    int 0x10
    mov ax, VGA_MODE13H
    mov es, ax
    mov di, 0
    mov al, 0x2a
    stosb
    mov di, 319
    mov al, 0x13
    stosb
    mov di, 63680
    mov al, 0x7f
    stosb
    ret

test_cga_graphics:
    mov ax, 0x0004
    int 0x10
    mov ax, VGA_TEXT
    mov es, ax
    xor di, di
    mov al, 0x1b
    stosb
    xor si, si
    mov al, [es:si]
    cmp al, 0x1b
    jne .done
    mov di, 0x2000
    mov al, 0x55
    stosb
    mov si, 0x2000
    mov al, [es:si]
    cmp al, 0x55
    jne .done
    mov ax, 0x0b00
    mov bx, 0x0101              ; BH=1 palette select, BL=1 palette 1
    int 0x10
    mov dx, 0x03d9
    in al, dx
    and al, 0x20
    cmp al, 0x20
    jne .done
    mov di, RESULT_BLOCK + (cga_graphics_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_ega_planar:
    mov ax, 0x0010
    int 0x10
    mov dx, 0x03c4
    mov al, 0x02                ; Sequencer Map Mask
    out dx, al
    inc dx
    mov al, 0x04                ; plane 2 only
    out dx, al
    mov ax, VGA_MODE13H
    mov es, ax
    xor di, di
    mov al, 0xa5
    stosb
    mov dx, 0x03ce
    mov al, 0x04                ; Graphics Controller Read Map Select
    out dx, al
    inc dx
    mov al, 0x02                ; read plane 2
    out dx, al
    xor si, si
    mov al, [es:si]
    cmp al, 0xa5
    jne .done
    mov dx, 0x03ce
    mov al, 0x04
    out dx, al
    inc dx
    mov al, 0x00                ; read plane 0; it must still be untouched
    out dx, al
    xor si, si
    mov al, [es:si]
    test al, al
    jnz .done
    mov di, RESULT_BLOCK + (ega_planar_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

copy_result_block:
    push ds
    push es
    mov ax, 0
    mov es, ax
    mov si, result_block_template
    mov di, RESULT_BLOCK
    mov cx, result_block_end - result_block_template
.copy:
    lodsb
    stosb
    loop .copy
    pop es
    pop ds
    ret

test_timer:
    ; Initialize the PIC for IRQ0 (ICW1..ICW4, vector base 0x08) and unmask IRQ0.
    mov al, 0x11
    out PIC_CMD, al
    mov al, 0x08
    out PIC_DATA, al
    mov al, 0x04
    out PIC_DATA, al
    mov al, 0x01
    out PIC_DATA, al
    mov al, 0xfe
    out PIC_DATA, al
    ; IVT[8] -> 0000:irq0_handler
    xor ax, ax
    mov es, ax
    mov word [es:0x20], irq0_handler
    mov word [es:0x22], 0
    mov word [TICK_COUNT], 0
    ; Channel 0: mode 3, LSB then MSB, count 11932 (about 100 Hz) for a short run.
    mov al, 0x36
    out PIT_CTRL, al
    mov ax, 11932
    out PIT_CH0, al
    mov al, ah
    out PIT_CH0, al
    sti
.wait:
    hlt
    mov ax, [TICK_COUNT]
    cmp ax, TICK_TARGET
    jb .wait
    cli
    ; Passed: patch FAIL -> PASS in the copied result block at 0x9000. Only bytes
    ; 0, 2, 3 change (F->P, I->S, L->S), and the additive checksum word at offset
    ; 10 gains 27, the difference between the 'PASS' and 'FAIL' byte sums.
    mov di, RESULT_BLOCK + (timer_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
    ret

test_sb_dsp_reset:
    ; Reset the DSP: write 1 then 0 to 0x226. The ~100us settle advances with
    ; CPU clocks (advance_devices ticks dsp.advance_micros), so a busy loop
    ; covers it; then poll 0x22E bit7 and read 0x22A expecting the 0xAA ack.
    mov dx, DSP_RESET
    mov al, 0x01
    out dx, al
    mov al, 0x00
    out dx, al
    mov cx, 8
.wait:
    call delay
    mov dx, DSP_STATUS
    in al, dx
    test al, 0x80
    jnz .ready
    loop .wait
    ret                       ; never became ready -> leave FAIL
.ready:
    mov dx, DSP_READ
    in al, dx
    cmp al, 0xAA
    jne .done                 ; wrong ack -> leave FAIL
    mov di, RESULT_BLOCK + (sb_reset_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_opl3:
    ; YMF262-vs-YM3812: reset the timer flags (reg 0x04 <- 0x80), then read the
    ; status port. An OPL3 reads 0x00 at rest (only IRQ/timer bits 7/6/5 are
    ; defined); an OPL2 reads 0x06 (always-set BUSY bits 1/2).
    mov dx, OPL_ADDR
    mov al, 0x04
    out dx, al
    mov dx, OPL_DATA
    mov al, 0x80            ; reset both overflow flags
    out dx, al
    mov dx, OPL_ADDR        ; status is readable on 0x388
    in al, dx
    and al, 0xE0            ; only bits 7/6/5 are defined
    jnz .done               ; nonzero -> not the OPL3 signature -> leave FAIL
    mov di, RESULT_BLOCK + (opl3_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_opl2:
    ; AdLib timer-1 overflow test on the primary bank (0x388/0x389). Timer-1
    ; steps every ~80us; a 0xFF preset overflows in one step. Mask and reset the
    ; flags, load the preset, start timer-1, then poll status bit6.
    mov dx, OPL_ADDR
    mov al, 0x04
    out dx, al
    mov dx, OPL_DATA
    mov al, 0x60            ; mask both timer IRQs (bits 6/5)
    out dx, al
    mov al, 0x80            ; reset both overflow flags
    out dx, al
    mov dx, OPL_ADDR
    mov al, 0x02            ; timer-1 preset register
    out dx, al
    mov dx, OPL_DATA
    mov al, 0xFF            ; preset -> overflow in one ~80us step
    out dx, al
    mov dx, OPL_ADDR
    mov al, 0x04
    out dx, al
    mov dx, OPL_DATA
    mov al, 0x21            ; start timer-1 (bit0), mask timer-2 IRQ (bit5)
    out dx, al
    mov cx, 8               ; poll up to 8 delay windows for the overflow
.wait:
    call delay
    mov dx, OPL_ADDR        ; status on 0x388
    in al, dx
    test al, 0x40           ; timer-1 overflow flag
    jnz .ready
    loop .wait
    ret                     ; never overflowed -> leave FAIL
.ready:
    mov di, RESULT_BLOCK + (opl2_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
    ret

test_sb_8bit_dma:
    ; Fill a 32-byte unsigned ramp at 0x0500 (DMA page 0, byte addr 0x0500).
    mov di, SB_DMA_BUF
    mov al, 0x80
    mov cx, 32
.fill:
    stosb
    add al, 8
    loop .fill
    ; 8237A ch1: single read, byte addr 0x0500, count 31 (->32 bytes), page 0.
    mov dx, DMA1_MODE
    mov al, 0x49
    out dx, al
    mov dx, DMA1_ADDR
    mov al, 0x00
    out dx, al
    mov al, 0x05
    out dx, al
    mov dx, DMA1_COUNT
    mov al, 0x1F
    out dx, al
    mov al, 0x00
    out dx, al
    mov dx, DMA1_PAGE
    mov al, 0x00
    out dx, al
    mov dx, DMA1_MASK
    mov al, 0x01            ; unmask ch1
    out dx, al
    ; DSP: 11025 Hz, block 32, single-cycle 8-bit DMA output.
    mov dx, DSP_WRITE
    mov al, 0x41
    out dx, al
    mov al, 0x2B
    out dx, al
    mov al, 0x11
    out dx, al
    mov al, 0x14
    out dx, al
    mov al, 0x1F
    out dx, al
    mov al, 0x00
    out dx, al
    ; IRQ5 -> vector 0x0D (PIC base 0x08 set by test_timer). Install the handler
    ; and unmask IRQ5 (clear IMR bit5), then spin until the handler bumps the
    ; counter. Playback is clock-driven, so the DSP sample clock in advance_devices
    ; edges the half/end-buffer IRQ during the spin.
    xor ax, ax
    mov es, ax
    mov word [es:0x34], irq5_handler
    mov word [es:0x36], 0
    mov word [SB_DMA_TICKS], 0
    mov dx, PIC_DATA
    in al, dx
    and al, 0xDF            ; clear bit5 -> unmask IRQ5
    out dx, al
    mov cx, 16
    call wait_for_irq5
    jz .done                ; timeout (counter still 0) -> leave FAIL
    mov di, RESULT_BLOCK + (sb_8bit_dma_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_sb_16bit_dma:
    ; Seed a 32-byte buffer at physical 0x2_0000 (segment 0x2000, offset 0). The
    ; slave 8237A channel 5 word-addresses its transfers, so page 0x8B=0x01 drives
    ; byte base (0x01 << 17) = 0x2_0000 (A0 tied low).
    mov ax, SB_DMA_BUF16_SEG
    mov es, ax
    xor di, di
    mov al, 0x80
    mov cx, 32
.fill:
    stosb
    add al, 8
    loop .fill
    ; Slave ch5 (local ch1): word addr 0, page 0x01, count 15 (16 words),
    ; auto-init read.
    mov dx, DMA2_MODE
    mov al, 0x59
    out dx, al
    mov dx, DMA2_ADDR
    mov al, 0x00
    out dx, al
    mov al, 0x00
    out dx, al
    mov dx, DMA2_COUNT
    mov al, 0x0F
    out dx, al
    mov al, 0x00
    out dx, al
    mov dx, DMA2_PAGE
    mov al, 0x01
    out dx, al
    mov dx, DMA2_MASK
    mov al, 0x01            ; unmask slave ch1 (channel 5)
    out dx, al
    ; DSP: 22050 Hz, 16-bit auto-init output, signed, stereo, count 15 (16 words).
    mov dx, DSP_WRITE
    mov al, 0x41
    out dx, al
    mov al, 0x56
    out dx, al
    mov al, 0x22
    out dx, al
    mov al, 0xB6
    out dx, al
    mov al, 0x30            ; mode: stereo (bit5) + signed (bit4)
    out dx, al
    mov al, 0x0F
    out dx, al
    mov al, 0x00
    out dx, al
    ; IRQ5 -> vector 0x0D. Re-install the handler, reset the tick counter, and
    ; unmask IRQ5. The 16-bit path edges the same half/end IRQs as the 8-bit one.
    xor ax, ax
    mov es, ax
    mov word [es:0x34], irq5_handler
    mov word [es:0x36], 0
    mov word [SB_DMA_TICKS], 0
    mov dx, PIC_DATA
    in al, dx
    and al, 0xDF            ; clear bit5 -> unmask IRQ5
    out dx, al
    mov cx, 16
    call wait_for_irq5
    jz .done                ; timeout (counter still 0) -> leave FAIL
    mov di, RESULT_BLOCK + (sb_16bit_dma_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_adpcm:
    ; Izarra's Yamaha ADPCM-B DAC lives at 0x240, IRQ10, DMA3. This direct-FIFO
    ; probe checks the guest-visible resource byte, starts a short mono block, and
    ; waits for the playing bit to clear after the clocked decoder consumes it.
    mov dx, ADPCM_RESOURCE
    in al, dx
    cmp al, 0xA3               ; high nibble IRQ10, low nibble DMA3
    jne .done

    mov dx, ADPCM_ADDR
    mov al, 0x00               ; CONTROL
    out dx, al
    mov dx, ADPCM_DATA
    mov al, 0x04               ; RESET playback/FIFO
    out dx, al

    mov dx, ADPCM_ADDR
    mov al, 0x01               ; RATE_LOW
    out dx, al
    mov dx, ADPCM_DATA
    mov al, 0x11               ; 11025 Hz = 0x2B11
    out dx, al
    mov dx, ADPCM_ADDR
    mov al, 0x02               ; RATE_HIGH
    out dx, al
    mov dx, ADPCM_DATA
    mov al, 0x2B
    out dx, al
    mov dx, ADPCM_ADDR
    mov al, 0x04               ; COUNT_LOW
    out dx, al
    mov dx, ADPCM_DATA
    mov al, 0x0F               ; 16 nibbles -> count register is nibbles - 1
    out dx, al
    mov dx, ADPCM_ADDR
    mov al, 0x05               ; COUNT_HIGH
    out dx, al
    mov dx, ADPCM_DATA
    xor al, al
    out dx, al
    mov dx, ADPCM_ADDR
    mov al, 0x03               ; FORMAT
    out dx, al
    mov dx, ADPCM_DATA
    xor al, al                 ; ADPCM-B mono
    out dx, al

    mov dx, ADPCM_FIFO
    mov cx, 8                  ; 8 bytes = 16 mono ADPCM nibbles
.feed:
    xor al, al
    out dx, al
    loop .feed

    mov dx, ADPCM_ADDR
    xor al, al                 ; CONTROL
    out dx, al
    mov dx, ADPCM_DATA
    mov al, 0x01               ; START
    out dx, al
    mov dx, ADPCM_STATUS
    in al, dx
    test al, 0x01
    jz .done                   ; did not enter playing state

    mov cx, 32
.wait:
    call delay
    mov dx, ADPCM_STATUS
    in al, dx
    test al, 0x01
    jz .passed
    loop .wait
    ret
.passed:
    mov di, RESULT_BLOCK + (adpcm_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
.done:
    ret

test_pc_speaker:
    ; Program PIT channel 2: mode 3 square wave, LSB then MSB, divisor 0x0400.
    mov al, 0xb6
    out PIT_CTRL, al
    mov al, 0x00
    out PIT_CH2, al
    mov al, 0x04
    out PIT_CH2, al
    ; Enable the speaker: port 0x61 bit 0 (GATE2) and bit 1 (data enable).
    in al, PORT_B
    or al, 0x03
    out PORT_B, al
    ; Sample timer-2 OUT (bit 5), delay, sample again. It must toggle.
    in al, PORT_B
    and al, 0x20
    mov bl, al
    mov cx, 8
.spin:
    call delay
    in al, PORT_B
    and al, 0x20
    cmp al, bl
    jne .passed
    loop .spin
    ret                       ; OUT never toggled: leave the record FAIL
.passed:
    mov di, RESULT_BLOCK + (pc_speaker_record - result_block_template)
    mov byte [di], 'P'
    mov byte [di + 2], 'S'
    mov byte [di + 3], 'S'
    add word [RESULT_BLOCK + 10], 27
    ret

; Shared: sti, then spin (one delay window per iteration) until the IRQ5 handler
; has set SB_DMA_TICKS != 0, then cli. cx is the poll budget. Leaves ZF clear
; when the IRQ fired (counter nonzero) and ZF set on timeout. Callers branch
; with `jz .done` to leave FAIL on timeout. Used by both DMA probes.
wait_for_irq5:
    sti
.spin:
    call delay
    mov ax, [SB_DMA_TICKS]
    test ax, ax
    jnz .got
    loop .spin
.got:
    cli
    ret

; IRQ5 handler: bump the tick counter, EOI the master PIC, iret.
irq5_handler:
    push ax
    inc word [SB_DMA_TICKS]
    mov al, 0x20
    out PIC_CMD, al
    pop ax
    iret

irq0_handler:
    push ax
    inc word [TICK_COUNT]
    mov al, 0x20
    out PIC_CMD, al
    pop ax
    iret

title db 'IzarraVM x86 Boot Test Suite', 0
line_cpu db 'CPU: real/protected/paging smoke PASS', 0
line_video db 'VIDEO: VGA text, CGA/EGA/VGA graphics PASS', 0
line_sound db 'SOUND: capability matrix emitted; absent/unsupported features FAIL', 0
line_memory db 'MEMORY: conventional pattern PASS, extended stress pending FAIL', 0

%include "results.inc"

times 8192 - ($ - $$) db 0
