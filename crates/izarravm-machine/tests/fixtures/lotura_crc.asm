; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only
;
; LOTCRC.COM - guest-authored fixed-golden coverage for legacy VEGA scanout.
;
; Assemble: nasm -f bin lotura_crc.asm -o LOTCRC.COM

    bits 16
    cpu 386
    org 0x100

UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_CRC      equ 8
UT_REG_EXIT     equ 12
UT_CMD_CRC      equ 1
UT_CMD_EXIT     equ 3

VGA_STATUS      equ 0x3DA
HGC_STATUS      equ 0x3BA

%macro WAIT_VGA 0
    mov dx, VGA_STATUS
    mov ah, 0x08
    mov cx, 3
    call wait_frames
%endmacro

%macro WAIT_HGC 0
    mov dx, HGC_STATUS
    mov ah, 0x80
    mov cx, 3
    call wait_frames
%endmacro

%macro WAIT_MONO 0
    mov dx, HGC_STATUS
    mov ah, 0x08
    mov cx, 3
    call wait_frames
%endmacro

%macro CHECK_CRC 1
    mov ebx, [%1]
    call check_crc
%endmacro

start:
    cld
    push cs
    pop ds
    call set_crc_window
    mov byte [test_id], 1

    mov al, 0x00
    call text_mode_pattern
    WAIT_VGA
    CHECK_CRC golden_mode00

    mov al, 0x01
    call text_mode_pattern
    WAIT_VGA
    CHECK_CRC golden_mode01

    mov al, 0x02
    call text_mode_pattern
    WAIT_VGA
    CHECK_CRC golden_mode02

    mov al, 0x03
    call text_mode_pattern
    WAIT_VGA
    CHECK_CRC golden_mode03

    call text_split_preset
    WAIT_VGA
    CHECK_CRC golden_text_split_preset

    mov al, 0x04
    call cga_pattern
    WAIT_VGA
    CHECK_CRC golden_mode04

    mov al, 0x05
    call cga_pattern
    WAIT_VGA
    CHECK_CRC golden_mode05

    mov al, 0x06
    call cga_pattern
    WAIT_VGA
    CHECK_CRC golden_mode06

    mov al, 0x07
    call text_mode_pattern
    WAIT_MONO
    CHECK_CRC golden_mode07

    mov al, 0x0D
    call planar_pattern
    WAIT_VGA
    CHECK_CRC golden_mode0d

    mov al, 0x17
    mov ah, 0xA3                    ; word addressing
    call write_crtc
    WAIT_VGA
    CHECK_CRC golden_mode0d_word

    mov al, 0x14
    mov ah, 0x40                    ; doubleword addressing
    call write_crtc
    WAIT_VGA
    CHECK_CRC golden_mode0d_dword

    mov al, 0x14
    xor ah, ah
    call write_crtc
    mov al, 0x17
    mov ah, 0xE3                    ; return to byte addressing
    call write_crtc
    WAIT_VGA
    CHECK_CRC golden_mode0d_byte_roundtrip

    mov ax, 0x008D                  ; mode 0Dh, preserve planar memory
    int 0x10
    WAIT_VGA
    CHECK_CRC golden_mode0d_no_clear

    mov al, 0x0E
    call planar_pattern
    WAIT_VGA
    CHECK_CRC golden_mode0e

    mov al, 0x0F
    call planar_pattern
    WAIT_MONO
    CHECK_CRC golden_mode0f

    mov al, 0x10
    call planar_pattern
    WAIT_VGA
    CHECK_CRC golden_mode10

    mov al, 0x11
    call planar_pattern
    WAIT_VGA
    CHECK_CRC golden_mode11

    mov al, 0x12
    call planar_pattern
    WAIT_VGA
    CHECK_CRC golden_mode12

    call mode13_pattern
    WAIT_VGA
    CHECK_CRC golden_mode13

    mov ax, 0x0093                  ; mode 13h, preserve video memory
    int 0x10
    WAIT_VGA
    CHECK_CRC golden_mode13_no_clear

    call mode_x_pattern
    WAIT_VGA
    CHECK_CRC golden_mode_x

    call mode_x_pan
    WAIT_VGA
    CHECK_CRC golden_mode_x_pan

    call hercules_pattern
    WAIT_HGC
    CHECK_CRC golden_hercules

    xor al, al
    jmp ut_exit

; The same rectangle is used for every mode. It includes two text rows, both
; CGA/Hercules interleave banks, and both halves of the split-screen fixture.
set_crc_window:
    mov dx, UT_INDEX
    xor al, al
    out dx, al
    mov dx, UT_DATA
    xor al, al                      ; x = 0
    out dx, al
    out dx, al
    out dx, al                      ; y = 0
    out dx, al
    mov al, 32                      ; width = 32
    out dx, al
    xor al, al
    out dx, al
    mov al, 32                      ; height = 32
    out dx, al
    xor al, al
    out dx, al
    ret

; Wait for three vertical-sync edges. A mode set resets the beam at line zero,
; so the extra frame keeps setup writes out of the presented golden. Hercules
; uses the trailing edge because its vertical-sync status bit is active low.
wait_frames:
.next:
    in al, dx
    test al, ah
    jnz .next
.start:
    in al, dx
    test al, ah
    jz .start
    loop .next
    ret

check_crc:
    mov dx, UT_COMMAND
    mov al, UT_CMD_CRC
    out dx, al

    mov dx, UT_INDEX
    mov al, UT_REG_CRC
    out dx, al
    mov dx, UT_DATA
    in al, dx
    mov [actual_crc], al
    in al, dx
    mov [actual_crc + 1], al
    in al, dx
    mov [actual_crc + 2], al
    in al, dx
    mov [actual_crc + 3], al
    mov eax, [actual_crc]

    cmp eax, ebx
    jne .failed
    inc byte [test_id]
    ret
.failed:
    mov al, [test_id]
    jmp ut_exit

ut_exit:
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

write_crtc:
    mov dx, 0x3D4
    out dx, al
    inc dx
    mov al, ah
    out dx, al
    ret

write_seq:
    mov dx, 0x3C4
    out dx, al
    inc dx
    mov al, ah
    out dx, al
    ret

write_gc:
    mov dx, 0x3CE
    out dx, al
    inc dx
    mov al, ah
    out dx, al
    ret

; AL selects one of the BIOS text modes 00h-03h or 07h.
text_mode_pattern:
    mov [bios_mode], al
    xor ah, ah
    int 0x10
    mov al, [bios_mode]
    cmp al, 0x07
    je .mono
    mov dx, 0x3D4
    mov bx, 0xB800
    jmp .cursor
.mono:
    mov dx, 0x3B4
    mov bx, 0xB000
.cursor:
    mov al, 0x0A
    out dx, al
    inc dx
    mov al, 0x20                    ; cursor disabled
    out dx, al
    mov es, bx
    xor di, di
    mov si, text_row_0
    mov cx, 8
    rep movsw
    mov al, [bios_mode]
    cmp al, 0x02
    jb .forty_columns
    mov di, 160
    jmp .second_row
.forty_columns:
    mov di, 80
.second_row:
    mov si, text_row_1
    mov cx, 8
    rep movsw
    ret

text_split_preset:
    mov di, 320
    mov si, text_row_2
    mov cx, 8
    rep movsw

    mov al, 0x0C
    xor ah, ah
    call write_crtc
    mov al, 0x0D
    mov ah, 80                      ; top region starts at text row 1
    call write_crtc
    mov al, 0x08
    mov ah, 3                       ; preset row scan
    call write_crtc

    mov dx, 0x3D4
    mov al, 0x07
    out dx, al
    inc dx
    in al, dx
    and al, 0xEF                    ; line compare bit 8 = 0
    out dx, al
    dec dx
    mov al, 0x09
    out dx, al
    inc dx
    in al, dx
    and al, 0xBF                    ; line compare bit 9 = 0
    out dx, al
    dec dx
    mov al, 0x18
    out dx, al
    inc dx
    mov al, 15                     ; bottom region starts at scanline 16
    out dx, al
    ret

cga_pattern:
    xor ah, ah
    int 0x10
    mov ax, 0xB800
    mov es, ax
    xor di, di
    mov si, cga_rows
    mov cx, 8
    rep movsb
    mov di, 0x2000
    mov cx, 8
    rep movsb
    mov di, 80
    mov cx, 8
    rep movsb
    mov di, 0x2050
    mov cx, 8
    rep movsb
    ret

; AL selects one of the BIOS planar modes 0Dh-12h.
planar_pattern:
    xor ah, ah
    int 0x10
    mov cx, 256
    ; fall through

; Fill each plane with a different non-repeating byte stream. CX is the number
; of bytes per plane. The pattern makes byte, word, and doubleword CRTC address
; transforms select visibly different pixels.
fill_planar:
    mov [fill_count], cx
    mov al, 0x05
    xor ah, ah                     ; write mode 0
    call write_gc
    mov al, 0x08
    mov ah, 0xFF                   ; all bits writable
    call write_gc
    mov ax, 0xA000
    mov es, ax
    mov si, plane_seeds
    mov bl, 1
    mov bp, 4
.plane:
    mov al, 0x02
    mov ah, bl
    call write_seq
    xor di, di
    mov al, [si]
    inc si
    mov cx, [fill_count]
.byte:
    stosb
    add al, 0x25
    loop .byte
    shl bl, 1
    dec bp
    jnz .plane
    ret

mode13_pattern:
    mov ax, 0x0013
    int 0x10
    mov ax, 0xA000
    mov es, ax
    xor di, di
    mov al, 1
    mov bp, 32
.row:
    mov cx, 32
.pixel:
    stosb
    add al, 13
    loop .pixel
    add di, 288
    dec bp
    jnz .row
    ret

mode_x_pattern:
    mov ax, 0x0013
    int 0x10
    mov al, 0x04
    mov ah, 0x06                    ; disable chain-4
    call write_seq
    mov al, 0x08
    mov ah, 0xFF
    call write_gc
    mov ax, 0xA000
    mov es, ax
    mov si, plane_seeds
    mov bl, 1
    mov bp, 4
.plane:
    mov al, 0x02
    mov ah, bl
    call write_seq
    xor di, di
    mov al, [si]
    inc si
    push bp
    mov bp, 32
.row:
    mov cx, 8
.pixel:
    stosb
    add al, 0x25
    loop .pixel
    add di, 72
    dec bp
    jnz .row
    pop bp
    shl bl, 1
    dec bp
    jnz .plane
    ret

mode_x_pan:
    mov dx, VGA_STATUS
    in al, dx                       ; attribute controller index phase
    mov dx, 0x3C0
    mov al, 0x33                    ; register 13h, palette address source on
    out dx, al
    mov al, 3
    out dx, al
    ret

hercules_pattern:
    mov ax, 0x0007
    int 0x10
    mov dx, 0x3BF
    mov al, 0x01                    ; permit graphics
    out dx, al
    mov dx, 0x3B8
    mov al, 0x0A                    ; graphics and video enable
    out dx, al
    mov ax, 0xB000
    mov es, ax
    mov si, hgc_rows
    xor di, di
    mov cx, 4
    rep movsb
    mov di, 0x2000
    mov cx, 4
    rep movsb
    mov di, 0x4000
    mov cx, 4
    rep movsb
    mov di, 0x6000
    mov cx, 4
    rep movsb
    mov di, 90
    mov cx, 4
    rep movsb
    ret

text_row_0:
    dw 0x1F41, 0x2E42, 0x3D43, 0x4C44, 0x5B45, 0x6A46, 0x7947, 0x0F48
text_row_1:
    dw 0x7149, 0x624A, 0x534B, 0x4450, 0x356D, 0x266E, 0x1778, 0x0F79
text_row_2:
    dw 0x4F30, 0x3E31, 0x2D32, 0x1C33, 0x0B34, 0x7A35, 0x6936, 0x5877

cga_rows:
    db 0x1B, 0xE4, 0x39, 0xC6, 0x55, 0xAA, 0x0F, 0xF0
    db 0x93, 0x6C, 0x87, 0x78, 0x33, 0xCC, 0x5A, 0xA5
    db 0xE1, 0x1E, 0xD2, 0x2D, 0xC3, 0x3C, 0xB4, 0x4B
    db 0x7D, 0xD7, 0x6E, 0xE6, 0x5F, 0xF5, 0x4C, 0xC4

plane_seeds:
    db 0x13, 0x57, 0x9B, 0xDF

hgc_rows:
    db 0x96, 0x69, 0xC3, 0x3C
    db 0x81, 0x42, 0x24, 0x18
    db 0xF0, 0x0F, 0xAA, 0x55
    db 0x87, 0x78, 0xCC, 0x33
    db 0xE1, 0x1E, 0xD2, 0x2D

actual_crc      dd 0
test_id         db 0
fill_count      dw 0
bios_mode       db 0
golden_mode00                dd 0x29B6F403
golden_mode01                dd 0x29B6F403
golden_mode02                dd 0x29B6F403
golden_mode03                dd 0x7BA3229B
golden_text_split_preset     dd 0xCFA071C7
golden_mode04                dd 0x5989DE52
golden_mode05                dd 0x52CD0418
golden_mode06                dd 0xD60E9FE0
golden_mode07                dd 0x82756C64
golden_mode0d                dd 0x7D261561
golden_mode0d_word           dd 0x587E11E2
golden_mode0d_dword          dd 0xB2FEB482
golden_mode0d_byte_roundtrip dd 0x7D261561
golden_mode0d_no_clear       dd 0x7D261561
golden_mode0e                dd 0x38D0C6BD
golden_mode0f                dd 0x73A19A77
golden_mode10                dd 0x50970F90
golden_mode11                dd 0x29FF5744
golden_mode12                dd 0x50970F90
golden_mode13                dd 0x6B36257E
golden_mode13_no_clear       dd 0x6B36257E
golden_mode_x                dd 0xF3B86D04
golden_mode_x_pan            dd 0x49B8AC41
golden_hercules              dd 0xEB65B913
