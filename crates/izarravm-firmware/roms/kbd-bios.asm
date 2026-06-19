; Izarra keyboard BIOS (phase 1): real INT 09h / INT 16h, echo to B8000.
; Assemble with: nasm -f bin kbd-bios.asm -o kbd-bios.bin
bits 16
org 0

%define ROM_SEG        0xf000      ; this ROM is mapped at 0xF0000
%define VGA_TEXT_SEG   0xb800
%define KB_BUF_START   0x001e      ; BDA: keyboard buffer
%define KB_BUF_HEAD    0x001a      ; BDA: head pointer (offset into 0x40 seg)
%define KB_BUF_TAIL    0x001c      ; BDA: tail pointer
%define KB_FLAGS       0x0017      ; BDA: shift flags

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

    ; Unmask IRQ1 on the master PIC (clear bit 1 of the 0x21 mask).
    in al, 0x21
    and al, 0xfd
    out 0x21, al

    ; Clear the screen and home the cursor (tracked in DI within ROM_SEG data).
    call clear_screen
    sti

main_loop:
    mov ah, 0x00            ; INT 16h: blocking read
    int 0x16                ; AL = ASCII, AH = scancode
    cmp al, 0
    je main_loop            ; skip non-ASCII keys (arrows, etc.) for the demo
    call putchar
    jmp main_loop

; INT 09h: keyboard hardware ISR
int09:
    push ax
    push bx
    push ds
    in al, 0x60            ; read scancode
    mov bl, al
    test bl, 0x80          ; break code? update shift state, do not enqueue
    jnz .break
    ; Make of a shift key sets the held bit; shift keys never enqueue ASCII.
    cmp bl, 0x2a
    je .set_lshift
    cmp bl, 0x36
    je .set_rshift
    ; Map scancode -> ASCII using the shift state, then enqueue (scancode:ascii).
    push cx
    push si
    mov ax, 0x0040
    mov ds, ax
    movzx si, bl
    mov al, [cs:scan2ascii + si]   ; unshifted ASCII (0 if none)
    test byte [KB_FLAGS], 0x03     ; either shift held?
    jz .have_ascii
    mov al, [cs:scan2ascii_shift + si]
.have_ascii:
    mov ah, bl                     ; scancode in AH, ASCII in AL
    call kb_enqueue
    pop si
    pop cx
    jmp .eoi
.set_lshift:
    mov ax, 0x0040
    mov ds, ax
    or byte [KB_FLAGS], 0x02
    jmp .eoi
.set_rshift:
    mov ax, 0x0040
    mov ds, ax
    or byte [KB_FLAGS], 0x01
    jmp .eoi
.break:
    and bl, 0x7f
    ; Left shift (0x2a) / right shift (0x36): clear the held bit on break.
    cmp bl, 0x2a
    je .clr_lshift
    cmp bl, 0x36
    je .clr_rshift
    jmp .eoi
.clr_lshift:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS], 0xfd
    jmp .eoi
.clr_rshift:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS], 0xfe
.eoi:
    mov al, 0x20
    out 0x20, al           ; EOI to master PIC
    pop ds
    pop bx
    pop ax
    iret

; INT 16h: keyboard services
int16:
    cmp ah, 0x00
    je .read
    cmp ah, 0x01
    je .peek
    cmp ah, 0x02
    je .flags
    iret
.read:
    sti
.read_wait:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    pop ds
    je .read_wait          ; buffer empty: spin (IRQ fills it)
    push ds
    mov cx, 0x0040
    mov ds, cx
    mov bx, [KB_BUF_HEAD]
    mov ax, [bx]           ; AX = scancode:ascii
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .read_store
    mov bx, KB_BUF_START
.read_store:
    mov [KB_BUF_HEAD], bx
    pop ds
    iret
.peek:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .peek_empty
    mov ax, [bx]
    pop ds
    clc
    iret
.peek_empty:
    pop ds
    stc
    iret
.flags:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov al, [KB_FLAGS]
    pop ds
    xor ah, ah
    iret

; BDA ring enqueue: AX = scancode:ascii
; Enters with DS = 0x0040. Drops the key if the buffer is full.
kb_enqueue:
    push bx
    push cx
    mov bx, [KB_BUF_TAIL]
    mov cx, bx
    add cx, 2
    cmp cx, KB_BUF_START + 32
    jb .no_wrap
    mov cx, KB_BUF_START
.no_wrap:
    cmp cx, [KB_BUF_HEAD]
    je .full               ; tail+2 == head: full, drop
    mov [bx], ax
    mov [KB_BUF_TAIL], cx
.full:
    pop cx
    pop bx
    ret

; Text output to B8000 (cursor tracked in cs:cursor)
clear_screen:
    push es
    push di
    mov ax, VGA_TEXT_SEG
    mov es, ax
    xor di, di
    mov cx, 80*25
    mov ax, 0x0720         ; space, grey on black
    rep stosw
    mov word [cs:cursor], 0
    pop di
    pop es
    ret

; putchar: AL = ASCII. Handles CR/LF minimally.
putchar:
    push es
    push di
    push bx
    mov bx, VGA_TEXT_SEG
    mov es, bx
    mov di, [cs:cursor]
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
    mov [cs:cursor], di
    pop bx
    pop di
    pop es
    ret

cursor: dw 0

; Scancode (Set 1) -> ASCII tables (US). 0 = no ASCII / handled elsewhere.
; Index by make scancode (0x00..0x3a covers the main block used by the demo).
; Covers the main typing block; extend toward 0x53 as needed.
scan2ascii:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0','-','=', 8,  9     ; 00-0f
    db 'q','w','e','r','t','y','u','i','o','p','[',']', 13, 0,  'a','s'      ; 10-1f
    db 'd','f','g','h','j','k','l',';',39, '`', 0,  92, 'z','x','c','v'      ; 20-2f
    db 'b','n','m',',','.','/', 0,  '*', 0,  ' '                            ; 30-39
    times 0x80 - ($ - scan2ascii) db 0

scan2ascii_shift:
    db 0,    27,  '!','@','#','$','%','^','&','*','(',')','_','+', 8,  9     ; 00-0f
    db 'Q','W','E','R','T','Y','U','I','O','P','{','}', 13, 0,  'A','S'      ; 10-1f
    db 'D','F','G','H','J','K','L',':',34, '~', 0,  '|','Z','X','C','V'      ; 20-2f
    db 'B','N','M','<','>','?', 0,  '*', 0,  ' '                            ; 30-39
    times 0x80 - ($ - scan2ascii_shift) db 0

; Reset vector at 0xFFFF0 (file offset 0xFFF0 in a 64K ROM)
    times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp ROM_SEG:reset
    times 0x10000 - ($ - $$) db 0
