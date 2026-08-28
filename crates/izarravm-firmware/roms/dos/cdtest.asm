; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Guest CD stack fixture for the IzarraCD ROM extension. Runs from AUTOEXEC
; on a full Toka-DOS boot. It checks the MSCDEX-style install surface the
; BIOS serves (INT 2Fh AX=1500h/1501h), verifies the ROM device header by
; name through the returned far pointer, then opens and verifies
; D:\PROBE.TXT through ordinary DOS file I/O — which the kernel forwards to
; the BIOS redirector.
;
; Build: nasm -f bin cdtest.asm -o cdtest.com
cpu 8086
org 0x100

OK              equ 0xA5

start:
    push cs
    pop ds

    ; Install check: AL=FFh marks the extensions present, BX = drive count
    ; (one), CX = the first drive (D: = 3).
    mov ax, 0x1500
    xor bx, bx
    int 0x2F
    cmp al, 0xFF
    jne fail_install
    cmp bx, 1
    jne fail_drive_count
    cmp cx, 3
    jne fail_drive_letter

    ; Get-device-list: one 5-byte entry, subunit + the ROM header pointer.
    push ds
    pop es
    mov bx, device_list
    mov ax, 0x1501
    int 0x2F
    mov ax, [device_list + 1]
    or ax, [device_list + 3]
    jz fail_device_header

    ; The header must carry the device name TOKACD01 at offset 10.
    les si, [device_list + 1]
    mov di, header_name
    mov cx, 8
.name_check:
    mov al, [es:si + 10]
    cmp al, [di]
    jne fail_header_name
    inc si
    inc di
    loop .name_check

    ; Read a known file from D: through DOS. The kernel forwards the
    ; redirector calls to the BIOS, which serves them from the host index.
    push cs
    pop es
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

fail_install:       mov al, 0xE1
                    jmp signal
fail_drive_count:   mov al, 0xE2
                    jmp signal
fail_drive_letter:  mov al, 0xE3
                    jmp signal
fail_device_header: mov al, 0xE4
                    jmp signal
fail_header_name:   mov al, 0xE8
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
header_name:
    db 'TOKACD01'
probe_path:
    db 'D:\PROBE.TXT', 0
probe_expected:
    db 'TOKA-CD-OK', 13, 10
probe_expected_len equ $ - probe_expected
read_buffer:
    times probe_expected_len db 0
