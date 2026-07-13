; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Direct DOS CD-driver protocol fixture. IZCDEX must already be installed.
; The program finds TOKACD through MSCDEX, calls its Strategy and Interrupt
; entry points, and checks request status and returned control blocks.
;
; Build: nasm -f bin cdprot.asm -o cdprot.com
cpu 8086
org 0x100

ST_DONE         equ 0x0100
ST_BAD_UNIT     equ 0x8101
ST_NOT_READY    equ 0x8102
ST_BAD_COMMAND  equ 0x8103
ST_BAD_LENGTH   equ 0x8105
ST_BAD_SECTOR   equ 0x8108
OK              equ 0xA6

start:
    push cs
    pop ds

    ; MSCDEX install check and first-drive query.
    xor bx, bx
    mov ax, 0x1500
    int 0x2F
    or bx, bx
    jz fail_01

    push ds
    pop es
    mov bx, device_list
    mov ax, 0x1501
    int 0x2F
    les di, [device_list + 1]
    mov ax, es
    or ax, di
    jz fail_02

    mov ax, [es:di + 6]
    mov [strategy_ptr], ax
    mov ax, [es:di + 8]
    mov [interrupt_ptr], ax
    mov ax, es
    mov [strategy_ptr + 2], ax
    mov [interrupt_ptr + 2], ax
    add di, 10
    mov si, driver_name
    mov cx, 8
    repe cmpsb
    jne fail_03

    ; Dispatcher, unit, and request-length behavior.
    mov byte [fail_code], 0x0F
    xor al, al
    mov dl, 23
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp byte [request + 13], 0
    jne fail
    mov ax, [request + 14]
    or ax, [request + 16]
    jz fail

    mov byte [fail_code], 0x10
    mov al, 1
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_BAD_COMMAND
    call expect_status

    mov byte [fail_code], 0x11
    mov al, 7
    mov dl, 12
    call init_request
    call issue_request
    mov dx, ST_BAD_LENGTH
    call expect_status

    mov byte [fail_code], 0x12
    mov al, 7
    mov dl, 13
    call init_request
    mov byte [request + 1], 1
    call issue_request
    mov dx, ST_BAD_UNIT
    call expect_status

    mov byte [fail_code], 0x13
    mov al, 7
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status

    mov byte [fail_code], 0x14
    mov al, 13
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status
    mov al, 14
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status

    ; Every supported IOCTL input control code.
    mov byte [fail_code], 0x20
    mov al, 0
    mov dl, 5
    call setup_ioctl_input
    call expect_ioctl_done
    mov ax, [control + 1]
    or ax, [control + 3]
    jz fail

    mov byte [fail_code], 0x21
    mov al, 1
    mov dl, 6
    call setup_ioctl_input
    call expect_ioctl_done

    mov byte [fail_code], 0x22
    mov al, 4
    mov dl, 9
    call setup_ioctl_input
    call expect_ioctl_done
    cmp byte [control + 1], 0
    jne fail
    cmp byte [control + 3], 1
    jne fail

    mov byte [fail_code], 0x23
    mov al, 6
    mov dl, 5
    call setup_ioctl_input
    call expect_ioctl_done
    mov ax, [control + 1]
    and ax, 0x0310
    cmp ax, 0x0310
    jne fail

    mov byte [fail_code], 0x24
    mov al, 7
    mov dl, 4
    call setup_ioctl_input
    mov byte [control + 1], 0
    call expect_ioctl_done
    cmp word [control + 2], 2048
    jne fail

    mov byte [fail_code], 0x25
    mov al, 8
    mov dl, 5
    call setup_ioctl_input
    call expect_ioctl_done
    mov ax, [control + 1]
    or ax, [control + 3]
    jz fail

    mov byte [fail_code], 0x26
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call expect_ioctl_done

    mov byte [fail_code], 0x27
    mov al, 10
    mov dl, 7
    call setup_ioctl_input
    call expect_ioctl_done
    cmp byte [control + 1], 0
    je fail
    cmp byte [control + 2], 0
    je fail

    mov byte [fail_code], 0x28
    mov al, 11
    mov dl, 7
    call setup_ioctl_input
    mov byte [control + 1], 1
    call expect_ioctl_done
    cmp byte [control + 2], 0
    jne fail
    cmp byte [control + 3], 2
    jne fail
    cmp byte [control + 4], 0
    jne fail
    cmp byte [control + 6], 0x41
    jne fail

    mov byte [fail_code], 0x29
    mov al, 12
    mov dl, 11
    call setup_ioctl_input
    call expect_ioctl_done
    cmp byte [control + 1], 0x41
    jne fail
    cmp byte [control + 2], 1
    jne fail
    cmp byte [control + 3], 1
    jne fail
    cmp word [control + 4], 0
    jne fail
    cmp byte [control + 6], 0
    jne fail
    cmp byte [control + 8], 0
    jne fail
    cmp byte [control + 9], 2
    jne fail
    cmp byte [control + 10], 0
    jne fail

    mov byte [fail_code], 0x2A
    mov al, 15
    mov dl, 11
    call setup_ioctl_input
    call expect_ioctl_done

    mov byte [fail_code], 0x2B
    mov al, 2
    mov dl, 1
    call setup_ioctl_input
    call issue_request
    mov dx, ST_BAD_COMMAND
    call expect_status

    mov byte [fail_code], 0x2C
    mov al, 0
    mov dl, 4
    call setup_ioctl_input
    call issue_request
    mov dx, ST_BAD_LENGTH
    call expect_status

    ; Two cooked sectors beginning at the ISO primary volume descriptor.
    mov byte [fail_code], 0x30
    mov byte [read_buffer], 0x5A
    mov byte [read_buffer + 4097], 0xA5
    mov al, 128
    mov dl, 27
    call init_request
    mov byte [request + 13], 0
    mov word [request + 14], read_buffer + 1
    mov word [request + 16], cs
    mov word [request + 18], 2
    mov word [request + 20], 16
    mov word [request + 22], 0
    mov byte [request + 24], 0
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp byte [read_buffer], 0x5A
    jne fail
    cmp byte [read_buffer + 4097], 0xA5
    jne fail
    cmp byte [read_buffer + 1], 1
    jne fail
    mov si, read_buffer + 2
    mov di, iso_magic
    mov cx, 5
    repe cmpsb
    jne fail

    ; Repeat one sector through a noncanonical far pointer whose offset crosses
    ; 64 KiB. Its segment maps 0xFFF0 back onto this program's guarded buffer,
    ; away from the COM stack at CS:FFFE.
    mov byte [fail_code], 0x37
    mov byte [wrap_guard_before], 0x5A
    mov byte [wrap_guard_after], 0xA5
    mov al, 128
    mov dl, 27
    call init_request
    mov word [request + 14], 0xFFF0
    mov ax, 0xFFF0
    sub ax, wrap_buffer
    mov cl, 4
    shr ax, cl
    mov dx, cs
    sub dx, ax
    mov [request + 16], dx
    mov word [request + 18], 1
    mov word [request + 20], 16
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp byte [wrap_guard_before], 0x5A
    jne fail
    cmp byte [wrap_buffer], 1
    jne fail
    cmp word [wrap_buffer + 1], 0x4443
    jne fail
    cmp byte [wrap_guard_after], 0xA5
    jne fail

    ; Red Book address mode uses packed binary 00:MM:SS:FF. LBA 16 is
    ; absolute 00:02:16.
    mov byte [fail_code], 0x38
    mov al, 128
    mov dl, 27
    call init_request
    mov byte [request + 13], 1
    mov word [request + 14], read_buffer + 1
    mov word [request + 16], cs
    mov word [request + 18], 1
    mov word [request + 20], 0x0210
    mov word [request + 22], 0
    call issue_request
    mov dx, ST_DONE
    call expect_status
    cmp byte [read_buffer + 1], 1
    jne fail
    cmp word [read_buffer + 2], 0x4443
    jne fail

    ; Seek and Prefetch use the same HSG target and no data phase.
    mov byte [fail_code], 0x31
    mov al, 131
    mov dl, 24
    call setup_seek
    call issue_request
    mov dx, ST_DONE
    call expect_status

    mov byte [fail_code], 0x32
    mov al, 130
    mov dl, 27
    call setup_seek
    call issue_request
    mov dx, ST_DONE
    call expect_status

    mov byte [fail_code], 0x33
    mov al, 128
    mov dl, 27
    call init_request
    mov word [request + 14], read_buffer + 1
    mov word [request + 16], cs
    mov word [request + 18], 1
    mov word [request + 20], 0xFFFF
    mov word [request + 22], 0xFFFF
    call issue_request
    mov dx, ST_BAD_SECTOR
    call expect_status

    ; Audio dispatch on data-only media: Play and Resume fail cleanly, Stop is
    ; an idempotent success.
    mov byte [fail_code], 0x34
    mov al, 132
    mov dl, 22
    call init_request
    mov word [request + 18], 1
    mov word [request + 20], 0
    call issue_request
    test ax, 0x8000
    jz fail

    mov byte [fail_code], 0x35
    mov al, 133
    mov dl, 13
    call init_request
    call issue_request
    mov dx, ST_DONE
    call expect_status

    mov byte [fail_code], 0x36
    mov al, 136
    mov dl, 13
    call init_request
    call issue_request
    test ax, 0x8000
    jz fail

    ; Channel control, lock state, reset, and close tray.
    mov byte [fail_code], 0x40
    mov al, 3
    mov dl, 9
    call setup_ioctl_output
    mov word [control + 1], 0x4000
    mov word [control + 3], 0x5001
    mov word [control + 5], 0x6002
    mov word [control + 7], 0x7003
    call expect_ioctl_done
    mov al, 4
    mov dl, 9
    call setup_ioctl_input
    call expect_ioctl_done
    cmp word [control + 1], 0x4000
    jne fail
    cmp word [control + 3], 0x5001
    jne fail
    cmp word [control + 5], 0x6002
    jne fail
    cmp word [control + 7], 0x7003
    jne fail

    mov byte [fail_code], 0x41
    mov al, 1
    mov dl, 2
    call setup_ioctl_output
    mov byte [control + 1], 1
    call expect_ioctl_done
    mov al, 6
    mov dl, 5
    call setup_ioctl_input
    call expect_ioctl_done
    test byte [control + 1], 2
    jnz fail

    mov al, 1
    mov dl, 2
    call setup_ioctl_output
    mov byte [control + 1], 0
    call expect_ioctl_done
    mov al, 6
    mov dl, 5
    call setup_ioctl_input
    call expect_ioctl_done
    test byte [control + 1], 2
    jz fail

    mov byte [fail_code], 0x42
    mov al, 2
    mov dl, 1
    call setup_ioctl_output
    call expect_ioctl_done
    ; Consume the Reset change latch before testing Eject's latch.
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call expect_ioctl_done

    mov byte [fail_code], 0x43
    mov al, 5
    mov dl, 1
    call setup_ioctl_output
    call expect_ioctl_done

    ; Eject is last. It raises one change indication, then media state is unknown.
    mov byte [fail_code], 0x44
    mov al, 0
    mov dl, 1
    call setup_ioctl_output
    call expect_ioctl_done
    mov al, 8
    mov dl, 5
    call setup_ioctl_input
    call issue_request
    mov dx, ST_NOT_READY
    call expect_status
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call expect_ioctl_done
    cmp byte [control + 1], 0xFF
    jne fail
    mov al, 9
    mov dl, 2
    call setup_ioctl_input
    call expect_ioctl_done
    cmp byte [control + 1], 0
    jne fail

    mov al, OK
    jmp signal

fail_01: mov al, 1
         jmp signal
fail_02: mov al, 2
         jmp signal
fail_03: mov al, 3
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

expect_ioctl_done:
    call issue_request
    mov dx, ST_DONE
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

setup_ioctl_input:
    push ax
    push dx
    call clear_control
    pop dx
    pop ax
    mov [control], al
    push dx
    mov al, 3
    mov dl, 26
    call init_request
    pop dx
    jmp set_ioctl_pointer

setup_ioctl_output:
    push ax
    push dx
    call clear_control
    pop dx
    pop ax
    mov [control], al
    push dx
    mov al, 12
    mov dl, 26
    call init_request
    pop dx
set_ioctl_pointer:
    mov word [request + 14], control
    mov word [request + 16], cs
    xor ax, ax
    mov al, dl
    mov [request + 18], ax
    ret

clear_control:
    push cs
    pop es
    mov di, control
    xor ax, ax
    mov cx, 8
    rep stosw
    ret

setup_seek:
    call init_request
    mov byte [request + 13], 0
    mov word [request + 20], 16
    mov word [request + 22], 0
    ret

driver_name:    db 'TOKACD01'
iso_magic:      db 'CD001'
device_list:    times 5 db 0
strategy_ptr:   dd 0
interrupt_ptr:  dd 0
fail_code:      db 0
request:        times 40 db 0
control:        times 16 db 0
read_buffer:    times 4098 db 0
align 16
wrap_guard_before: db 0
times 15 db 0
wrap_buffer:    times 2048 db 0
wrap_guard_after: db 0
