; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; MOUSEGFX.COM - verify TOKAMOUS draws, moves, hides and restores its graphics
; cursor in VGA mode 13h, takes a program's vertical range as given, and hides
; the cursor again on a mode set (the INT 10h hook).
;
; Runs after TOKAMOUS is resident. Paints a known 16x16 block in A000, shows the
; cursor over it and checks three pixels against the default arrow masks:
;   (0,0)  screen 3FFFh bit15 clear, cursor 0000h -> 00h (black outline)
;   (1,1)  screen 1FFFh bit14 clear, cursor 4000h bit14 set -> 0Fh (white body)
;   (15,0) screen 3FFFh bit0 set, cursor clear -> the background, 20h
; Exits through the Lotura unit-test port with 0 on success or the failing step.
;
; Assemble: nasm -f bin mousegfx.asm -o MOUSEGFX.COM
    cpu 386
    org 0x100

UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3

BLOCK_X         equ 100               ; pixel column of the painted block
BLOCK_Y         equ 50                ; pixel row of the painted block
BACKGROUND      equ 0x20

%define PIX(x, y) ((y) * 320 + (x))

start:
    mov ax, 0x0013                    ; 320x200x256
    int 0x10

    xor ax, ax                        ; reset
    int 0x33
    cmp ax, 0xFFFF
    mov al, 1
    jne exit

    mov ax, 0xA000
    mov es, ax

    mov ax, 0x0004                    ; setpos virtual (200,50) = pixel (100,50)
    mov cx, BLOCK_X * 2
    mov dx, BLOCK_Y
    int 0x33
    call paint_block

    mov ax, 0x0001                    ; show
    int 0x33
    mov al, 2
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], 0x00
    jne exit
    mov al, 3
    cmp byte [es:PIX(BLOCK_X + 1, BLOCK_Y + 1)], 0x0F
    jne exit
    mov al, 4
    cmp byte [es:PIX(BLOCK_X + 15, BLOCK_Y)], BACKGROUND
    jne exit

    mov ax, 0x0002                    ; hide: the block comes back intact
    int 0x33
    mov al, 5
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], BACKGROUND
    jne exit
    cmp byte [es:PIX(BLOCK_X + 1, BLOCK_Y + 1)], BACKGROUND
    jne exit

    mov ax, 0x0001                    ; show, then move one pixel right, two down
    int 0x33
    mov ax, 0x0004
    mov cx, (BLOCK_X + 1) * 2
    mov dx, BLOCK_Y + 2
    int 0x33
    mov al, 6
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], BACKGROUND   ; old corner restored
    jne exit
    mov al, 7
    cmp byte [es:PIX(BLOCK_X + 1, BLOCK_Y + 2)], 0x00 ; new corner drawn
    jne exit

    mov ax, 0x0008                    ; a range past the mode's height is kept
    xor cx, cx
    mov dx, 479
    int 0x33
    mov ax, 0x0004
    mov cx, BLOCK_X * 2
    mov dx, 300
    int 0x33
    mov ax, 0x0003
    int 0x33
    mov al, 8
    cmp dx, 300
    jne exit

    mov ax, 0x0013                    ; mode set: range back to the mode, cursor hidden
    int 0x10
    mov ax, 0x0004
    mov cx, BLOCK_X * 2
    mov dx, 300
    int 0x33
    mov ax, 0x0003
    int 0x33
    mov al, 9
    cmp dx, 199
    jne exit
    call paint_block
    mov ax, 0x0004                    ; hidden: a move draws nothing
    mov cx, BLOCK_X * 2
    mov dx, BLOCK_Y
    int 0x33
    mov al, 10
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], BACKGROUND
    jne exit
    mov ax, 0x0001
    int 0x33
    mov al, 11
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], 0x00
    jne exit

    ; fn 09h: a custom shape (keep everything, invert the first pixel) redraws
    mov ax, 0x0009
    xor bx, bx
    xor cx, cx
    push cs
    pop es
    mov dx, dot_cursor
    int 0x33
    mov ax, 0xA000
    mov es, ax
    mov al, 12
    cmp byte [es:PIX(BLOCK_X, BLOCK_Y)], BACKGROUND ^ 0x0F
    jne exit
    mov al, 13
    cmp byte [es:PIX(BLOCK_X + 1, BLOCK_Y)], BACKGROUND
    jne exit

    ; ---- planar 640x480x16 (mode 12h): the same arrow through the graphics
    ; controller. The block is painted and read back with the BIOS pixel
    ; services (INT 10h 0Ch/0Dh), so the check does not share the driver's
    ; register programming. The mode set reset the masks to the default arrow
    ; and hid the cursor; the range is now 0..639 x 0..479.
    mov ax, 0x0012
    int 0x10
    xor ax, ax                        ; reset: default masks, range from the mode
    int 0x33
    mov ax, 0x0003
    int 0x33
    mov al, 14
    cmp cx, 319                       ; centred in 0..639
    jne exit
    mov al, 5                         ; paint colour
    call paint_block_bios
    mov ax, 0x0004                    ; pixel (100,50): virtual x = pixel x here
    mov cx, BLOCK_X
    mov dx, BLOCK_Y
    int 0x33
    mov ax, 0x0001
    int 0x33
    mov cx, BLOCK_X
    mov dx, BLOCK_Y
    call read_pixel_bios
    mov ah, 15
    cmp al, 0x00
    jne exit_ah
    mov cx, BLOCK_X + 1
    mov dx, BLOCK_Y + 1
    call read_pixel_bios
    mov ah, 16
    cmp al, 0x0F
    jne exit_ah
    mov cx, BLOCK_X + 15
    mov dx, BLOCK_Y
    call read_pixel_bios
    mov ah, 17
    cmp al, 5
    jne exit_ah
    mov ax, 0x0002                    ; hide: the paint comes back
    int 0x33
    mov cx, BLOCK_X + 1
    mov dx, BLOCK_Y + 1
    call read_pixel_bios
    mov ah, 18
    cmp al, 5
    jne exit_ah

    ; ---- CGA 320x200x4 (mode 04h): two-bit pixels, the invert keeps the low
    ; two bits of 0Fh, so a body pixel over colour 0 reads 3.
    mov ax, 0x0004
    int 0x10
    xor ax, ax
    int 0x33
    mov al, 2
    call paint_block_bios
    mov ax, 0x0004                    ; virtual (200,50) = pixel (100,50)
    mov cx, BLOCK_X * 2
    mov dx, BLOCK_Y
    int 0x33
    mov ax, 0x0001
    int 0x33
    mov cx, BLOCK_X
    mov dx, BLOCK_Y
    call read_pixel_bios
    mov ah, 19
    cmp al, 0
    jne exit_ah
    mov cx, BLOCK_X + 1
    mov dx, BLOCK_Y + 1
    call read_pixel_bios
    mov ah, 20
    cmp al, 3
    jne exit_ah
    mov cx, BLOCK_X + 15
    mov dx, BLOCK_Y
    call read_pixel_bios
    mov ah, 21
    cmp al, 2
    jne exit_ah
    mov ax, 0x0002
    int 0x33
    mov cx, BLOCK_X
    mov dx, BLOCK_Y
    call read_pixel_bios
    mov ah, 22
    cmp al, 2
    jne exit_ah

    ; ---- VBE 640x480x256 (mode 101h): the BDA mode byte stays stale, so the
    ; driver must take the size from the VBE mode info; a reset then gives a
    ; 0..479 range and draws nothing (the program draws its own cursor).
    mov ax, 0x4F02
    mov bx, 0x0101
    int 0x10
    cmp ax, 0x004F
    mov ah, 23                        ; mov keeps the flags of the compare
    jne exit_ah
    xor ax, ax
    int 0x33
    mov ax, 0x0004
    mov cx, 100
    mov dx, 400
    int 0x33
    mov ax, 0x0003
    int 0x33
    mov ah, 24
    cmp dx, 400
    jne exit_ah
    mov ax, 0xB800                    ; the stale text mode must not be drawn into
    mov es, ax
    mov word [es:0x0000], 0x1234
    mov ax, 0x0001
    int 0x33
    mov ah, 25
    cmp word [es:0x0000], 0x1234
    jne exit_ah

    xor al, al
    jmp exit
exit_ah:
    mov al, ah
exit:
    mov ah, al
    mov dx, UT_INDEX
    mov al, UT_REG_EXIT
    out dx, al
    mov dx, UT_DATA
    mov al, ah
    out dx, al
    mov dx, UT_COMMAND
    mov al, UT_CMD_EXIT
    out dx, al
.hang:
    jmp .hang

; Fill the 16x16 block at (BLOCK_X, BLOCK_Y) with BACKGROUND. ES = A000.
paint_block:
    push ax
    push cx
    push di
    mov di, PIX(BLOCK_X, BLOCK_Y)
    mov cx, 16
    mov al, BACKGROUND
.row:
    push cx
    push di
    mov cx, 16
    cld
    rep stosb
    pop di
    pop cx
    add di, 320
    loop .row
    pop di
    pop cx
    pop ax
    ret

; Paint the 16x16 block at (BLOCK_X, BLOCK_Y) with colour AL through INT 10h
; AH=0Ch (any graphics mode). Preserves ALL.
paint_block_bios:
    push ax
    push bx
    push cx
    push dx
    mov bl, al
    mov dx, BLOCK_Y
.row:
    mov cx, BLOCK_X
.col:
    mov ah, 0x0C
    mov al, bl
    xor bh, bh                        ; page 0
    int 0x10
    inc cx
    cmp cx, BLOCK_X + 16
    jb .col
    inc dx
    cmp dx, BLOCK_Y + 16
    jb .row
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; Read the pixel at column CX, row DX through INT 10h AH=0Dh into AL.
read_pixel_bios:
    push bx
    mov ah, 0x0D
    xor bh, bh
    int 0x10
    pop bx
    ret

dot_cursor:
    times 16 dw 0xFFFF                ; screen mask: keep every background pixel
    dw 0x8000                         ; cursor mask: invert the top-left pixel only
    times 15 dw 0x0000
