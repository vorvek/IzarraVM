; Izarra keyboard BIOS (phase 1): real INT 09h / INT 16h, echo to B8000.
; Assemble with: nasm -f bin kbd-bios.asm -o kbd-bios.bin
bits 16
org 0

%define ROM_SEG        0xf000      ; this ROM is mapped at 0xF0000
%define VGA_TEXT_SEG   0xb800
%define CURSOR_OFF     0x0050      ; BDA: cursor offset into the text buffer (RAM)

%include "kbd-bios-core.inc"

reset:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov ss, ax
    mov sp, 0x7000

    ; Install IVT[09h] -> int09, IVT[16h] -> int16 (CS = ROM_SEG).
    mov word [0x09*4], int09
    mov word [0x09*4+2], ROM_SEG
    mov word [0x16*4], int16
    mov word [0x16*4+2], ROM_SEG

    ; BDA keyboard buffer: head = tail = KB_BUF_START, flags = 0.
    mov ax, 0x0040
    mov ds, ax
    mov word [KB_BUF_HEAD], KB_BUF_START
    mov word [KB_BUF_TAIL], KB_BUF_START
    mov byte [KB_FLAGS], 0

    ; Program the 8259 PICs so IRQ0..7 map to INT 08h..0Fh (IRQ1 -> INT 09h).
    ; ICW1 (edge, cascade, ICW4 to follow), ICW2 base, ICW3 cascade, ICW4 8086.
    mov al, 0x11
    out 0x20, al           ; master ICW1
    out 0xa0, al           ; slave  ICW1
    mov al, 0x08
    out 0x21, al           ; master ICW2: vector base 08h
    mov al, 0x70
    out 0xa1, al           ; slave  ICW2: vector base 70h
    mov al, 0x04
    out 0x21, al           ; master ICW3: slave on IR2
    mov al, 0x02
    out 0xa1, al           ; slave  ICW3: cascade identity 2
    mov al, 0x01
    out 0x21, al           ; master ICW4: 8086 mode
    out 0xa1, al           ; slave  ICW4: 8086 mode

    ; Unmask IRQ1 on the master PIC (clear bit 1 of the 0x21 mask).
    in al, 0x21
    and al, 0xfd
    out 0x21, al

    ; Clear the screen and home the cursor (cursor tracked in the BDA, in RAM).
    call clear_screen
    sti

main_loop:
    mov ah, 0x00            ; INT 16h: blocking read
    int 0x16                ; AL = ASCII, AH = scancode
    cmp al, 0
    je main_loop            ; skip non-ASCII keys (arrows, etc.) for the demo
    call putchar
    jmp main_loop

; Text output to B8000 (cursor tracked in the BDA at 0040:CURSOR_OFF, in RAM,
; because writes back into the ROM image are dropped by the bus).
clear_screen:
    push ds
    push es
    push di
    mov ax, VGA_TEXT_SEG
    mov es, ax
    xor di, di
    mov cx, 80*25
    mov ax, 0x0720         ; space, grey on black
    rep stosw
    mov ax, 0x0040
    mov ds, ax
    mov word [CURSOR_OFF], 0
    pop di
    pop es
    pop ds
    ret

; putchar: AL = ASCII. Handles CR/LF minimally.
putchar:
    push ds
    push es
    push di
    push bx
    push dx
    mov bx, 0x0040
    mov ds, bx
    mov bx, VGA_TEXT_SEG
    mov es, bx
    mov di, [CURSOR_OFF]
    cmp al, 13
    je .cr
    cmp al, 10
    je .lf
    mov ah, 0x07
    mov [es:di], ax
    add di, 2
    jmp .save
.cr:
    mov ax, di
    xor dx, dx
    mov bx, 160
    div bx
    mul bx                 ; di = (di/160)*160 (start of line)
    mov di, ax
    jmp .save
.lf:
    add di, 160
.save:
    mov [CURSOR_OFF], di
    pop dx
    pop bx
    pop di
    pop es
    pop ds
    ret

; Reset vector at 0xFFFF0 (file offset 0xFFF0 in a 64K ROM)
    times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp ROM_SEG:reset
    times 0x10000 - ($ - $$) db 0
