; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Guest-owned Toka-DOS CD stack fixture. Runs from AUTOEXEC after IZCDEX.
; It distinguishes the real SHSUCDX-derived redirector from the host fallback,
; checks the MSCDEX device list contains a non-null TOKACD header, then opens
; and verifies D:\PROBE.TXT through ordinary DOS file I/O.
;
; Build: nasm -f bin cdtest.asm -o cdtest.com
cpu 8086
org 0x100

OK              equ 0xA5
SHSUCDX_QUERY   equ 0xBABE

start:
    push cs
    pop ds

    ; SHSUCDX v3 private installation query. The old host HLE leaves BX at
    ; BABEh and AL clear, while the guest redirector returns AL=FFh, replaces
    ; BX with its compile flags, and publishes a non-empty drive table.
    mov ax, 0x1100
    mov bx, SHSUCDX_QUERY
    int 0x2F
    cmp al, 0xFF
    jne fail_redirector
    cmp bx, SHSUCDX_QUERY
    je fail_redirector
    or cx, cx
    jz fail_drive_count
    or dx, dx
    jz fail_drive_count
    mov ax, es
    or ax, di
    jz fail_drive_table

    ; MSCDEX get-device-list. The host fallback returns a null header pointer;
    ; the guest stack must return TOKACD's real character-device header.
    push ds
    pop es
    mov bx, device_list
    mov ax, 0x1501
    int 0x2F
    mov ax, [device_list + 1]
    or ax, [device_list + 3]
    jz fail_device_header

    ; Read a known file from D: through DOS, which now routes through IZCDEX,
    ; TOKACD, and the secondary-channel ATAPI PIO path.
    mov dx, probe_path
    mov ax, 0x3D00
    int 0x21
    jc fail_open
    mov bx, ax

    mov dx, read_buffer
    mov cx, probe_expected_len
    mov ah, 0x3F
    int 0x21
    jc fail_read_close
    cmp ax, probe_expected_len
    jne fail_data_close

    push cs
    pop es
    mov si, read_buffer
    mov di, probe_expected
    mov cx, probe_expected_len
    repe cmpsb
    jne fail_data_close

    mov ah, 0x3E
    int 0x21
    mov al, OK
    jmp signal

fail_read_close:
    mov al, 0xE6
    jmp close_signal
fail_data_close:
    mov al, 0xE7
close_signal:
    push ax
    mov ah, 0x3E
    int 0x21
    pop ax
    jmp signal

fail_redirector:    mov al, 0xE1
                    jmp signal
fail_drive_count:   mov al, 0xE2
                    jmp signal
fail_drive_table:   mov al, 0xE3
                    jmp signal
fail_device_header: mov al, 0xE4
                    jmp signal
fail_open:          mov al, 0xE5

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

device_list:
    times 5 db 0
probe_path:
    db 'D:\PROBE.TXT', 0
probe_expected:
    db 'TOKA-CD-OK', 13, 10
probe_expected_len equ $ - probe_expected
read_buffer:
    times probe_expected_len db 0
