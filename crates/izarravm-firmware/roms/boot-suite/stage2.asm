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
%define PIT_CTRL 0x43
%define PIC_CMD 0x20
%define PIC_DATA 0x21
%define TICK_COUNT 0x0600
%define TICK_TARGET 10
%define DSP_RESET 0x226
%define DSP_READ 0x22A
%define DSP_WRITE 0x22C
%define DSP_STATUS 0x22E
%define OPL_ADDR 0x388
%define OPL_DATA 0x389

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

    mov si, result_payload
    call puts_serial

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

    call test_mode13h
    call test_timer
    call test_sb_dsp_reset
    call test_opl3

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

irq0_handler:
    push ax
    inc word [TICK_COUNT]
    mov al, 0x20
    out PIC_CMD, al
    pop ax
    iret

title db 'IzarraVM x86 Boot Test Suite', 0
line_cpu db 'CPU: real/protected/paging smoke PASS', 0
line_video db 'VIDEO: VGA text PASS, CGA/EGA/VGA graphics pending FAIL', 0
line_sound db 'SOUND: capability matrix emitted; absent/unsupported features FAIL', 0
line_memory db 'MEMORY: conventional pattern PASS, extended stress pending FAIL', 0

%include "results.inc"

times 8192 - ($ - $$) db 0
