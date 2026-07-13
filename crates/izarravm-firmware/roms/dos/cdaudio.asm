; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Direct TOKACD audio-state fixture. The mounted mixed-mode disc has its audio
; track at HSG LBA 24 and a primary volume descriptor at LBA 16.
cpu 8086
org 0x100

ST_DONE         equ 0x0100
ST_BUSY         equ 0x0300
OK              equ 0xA7
AUDIO_LBA       equ 24

start:
    push cs
    pop ds
    push ds
    pop es
    mov bx, device_list
    mov ax, 0x1501
    int 0x2F
    les di, [device_list + 1]
    mov ax, es
    or ax, di
    jz fail_01
    mov ax, [es:di + 6]
    mov [strategy_ptr], ax
    mov ax, [es:di + 8]
    mov [interrupt_ptr], ax
    mov ax, es
    mov [strategy_ptr + 2], ax
    mov [interrupt_ptr + 2], ax

    ; The first Play also consumes the live-swap unit attention.
    mov byte [fail_code], 0x10
    call setup_play
    call issue_request
    mov dx, ST_BUSY
    call expect_status

    ; The live A-to-B swap remains latched across the successful retry, once.
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call issue_request
    mov dx, ST_BUSY
    call expect_status
    cmp byte [control + 1], 0xFF
    jne fail
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call issue_request
    mov dx, ST_BUSY
    call expect_status
    cmp byte [control + 1], 1
    jne fail

    mov byte [fail_code], 0x11
    mov al, 133
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status

    ; Audio Status must expose a paused, resumable HSG range.
    mov byte [fail_code], 0x12
    mov al, 15
    mov dl, 11
    call setup_ioctl_input
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp word [control + 1], 1
    jne fail
    cmp word [control + 3], AUDIO_LBA
    jb fail
    cmp word [control + 7], AUDIO_LBA + 10
    jne fail

    mov byte [fail_code], 0x13
    mov al, 136
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_BUSY
    call expect_status

    ; A successful Seek interrupts playback and clears the retained range.
    mov byte [fail_code], 0x14
    mov al, 131
    mov dl, 24
    call init_request
    mov word [request + 20], 16
    call issue_request
    mov dx, ST_DONE
    call expect_status
    mov al, 15
    mov dl, 11
    call setup_ioctl_input
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp word [control + 1], 0
    jne fail
    mov ax, [control + 3]
    or ax, [control + 5]
    or ax, [control + 7]
    or ax, [control + 9]
    jnz fail

    ; Red Book Play uses packed 00:MM:SS:FF and retains that representation.
    mov byte [fail_code], 0x15
    call setup_play_red
    call issue_request
    mov dx, ST_BUSY
    call expect_status
    mov al, 133
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status
    mov al, 15
    mov dl, 11
    call setup_ioctl_input
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp word [control + 1], 1
    jne fail
    cmp word [control + 3], 0x0218
    jne fail
    cmp word [control + 5], 0
    jne fail
    cmp word [control + 7], 0x0222
    jne fail
    cmp word [control + 9], 0
    jne fail
    mov al, 136
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_BUSY
    call expect_status

    ; A successful cooked read interrupts playback and clears retained state.
    mov al, 128
    mov dl, 27
    call init_request
    mov word [request + 14], read_buffer
    mov word [request + 16], cs
    mov word [request + 18], 1
    mov word [request + 20], 16
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp byte [read_buffer], 1
    jne fail
    cmp word [read_buffer + 1], 0x4443
    jne fail
    cmp word [read_buffer + 3], 0x3030
    jne fail
    cmp byte [read_buffer + 5], 0x31
    jne fail
    mov al, 15
    mov dl, 11
    call setup_ioctl_input
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp word [control + 1], 0
    jne fail
    mov ax, [control + 3]
    or ax, [control + 5]
    or ax, [control + 7]
    or ax, [control + 9]
    jnz fail

    mov al, OK
    jmp signal

fail_01: mov al, 1
         jmp signal
fail:
    mov al, [fail_code]
signal:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.hang:
    hlt
    jmp .hang

setup_play:
    mov al, 132
    mov dl, 22
    call init_request
    mov word [request + 14], AUDIO_LBA
    mov word [request + 18], 10
    ret

setup_play_red:
    mov al, 132
    mov dl, 22
    call init_request
    mov byte [request + 13], 1
    mov word [request + 14], 0x0218
    mov word [request + 16], 0
    mov word [request + 18], 10
    ret

setup_ioctl_input:
    push ax
    push dx
    push cs
    pop es
    mov di, control
    xor ax, ax
    mov cx, 8
    rep stosw
    pop dx
    pop ax
    mov [control], al
    push dx
    mov al, 3
    mov dl, 26
    call init_request
    pop dx
    mov word [request + 14], control
    mov word [request + 16], cs
    xor ax, ax
    mov al, dl
    mov [request + 18], ax
    ret

expect_status:
    cmp ax, dx
    jne fail
    ret

issue_request:
    push cs
    pop es
    mov bx, request
    call far [strategy_ptr]
    call far [interrupt_ptr]
    mov ax, [request + 3]
    ret

init_request:
    push ax
    push dx
    push cs
    pop es
    mov di, request
    xor ax, ax
    mov cx, 20
    rep stosw
    pop dx
    pop ax
    mov [request], dl
    mov [request + 2], al
    ret

device_list:    times 5 db 0
strategy_ptr:   dd 0
interrupt_ptr:  dd 0
fail_code:      db 0
request:        times 40 db 0
control:        times 16 db 0
read_buffer:    times 2048 db 0
