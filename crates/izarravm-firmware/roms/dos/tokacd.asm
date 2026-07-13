; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; TOKACD.SYS, the Toka-DOS driver for Izarra's fixed ATAPI CD-ROM.
;
; The drive is secondary IDE master at 170h/376h. Transfers use polling PIO
; with device interrupts disabled. No controller scan, DMA path, or resident
; IRQ handler is needed for this one-device machine.
;
; Build: nasm -f bin tokacd.asm -o tokacd.sys
cpu 8086
org 0

ATA_DATA        equ 0x170
ATA_ERROR       equ 0x171
ATA_FEATURES    equ 0x171
ATA_COUNT       equ 0x172
ATA_LBA_LOW     equ 0x173
ATA_LBA_MID     equ 0x174
ATA_LBA_HIGH    equ 0x175
ATA_DEVICE      equ 0x176
ATA_STATUS      equ 0x177
ATA_COMMAND     equ 0x177
ATA_CONTROL     equ 0x376

ATA_BSY         equ 0x80
ATA_DRQ         equ 0x08
ATA_ERR         equ 0x01

RH_LENGTH       equ 0
RH_UNIT         equ 1
RH_COMMAND      equ 2
RH_STATUS       equ 3
RH_DATA         equ 13

RH_IOCTL_PTR    equ 14
RH_IOCTL_COUNT  equ 18

RH_READ_ADDR    equ 13
RH_READ_BUFFER  equ 14
RH_READ_COUNT   equ 18
RH_READ_START   equ 20
RH_READ_MODE    equ 24

RH_PLAY_ADDR    equ 13
RH_PLAY_START   equ 14
RH_PLAY_COUNT   equ 18

ST_DONE         equ 0x0100
ST_BUSY         equ 0x0200
ST_ERROR        equ 0x8000

ERR_UNIT        equ 0x01
ERR_NOT_READY   equ 0x02
ERR_COMMAND     equ 0x03
ERR_LENGTH      equ 0x05
ERR_SECTOR      equ 0x08
ERR_READ        equ 0x0B
ERR_GENERAL     equ 0x0C
ERR_CHANGED     equ 0x0F

DIR_NONE        equ 0
DIR_IN          equ 1
DIR_OUT         equ 2

MEDIA_UNKNOWN   equ 0
MEDIA_PRESENT   equ 1
MEDIA_ABSENT    equ 2

; MSCDEX character-device header, including its four-byte CD extension.
device_header:
    dd 0xFFFFFFFF
    dw 0xC800
    dw strategy
    dw interrupt
    db 'TOKACD01'
    dw 0
drive_letter:
    db 0
    db 1

request_ptr:    dd 0
old_ss:         dw 0
old_sp:         dw 0
open_count:     dw 0

media_state:    db MEDIA_UNKNOWN
media_latch:    db 0
door_open:      db 0
door_locked:    db 0
audio_playing:  db 0
audio_paused:   db 0
error_context:  db 0
last_addr_mode: db 0
play_addr_mode: db 0

head_lba:       dd 0
io_lba:         dd 0
last_start:     dd 0             ; HSG LBA or packed binary 00:MM:SS:FF
last_end:       dd 0
play_msf_start: dd 0
play_msf_end:   dd 0
play_stat_start: dd 0
play_stat_end:  dd 0
play_lba_start: dd 0
play_input_start: dd 0
play_count:     dd 0
disc_capacity:  dd 0

; Output channel, volume pairs. Izarra has two physical outputs, but the DOS
; interface always carries four pairs.
audio_channels:
    db 0, 0xFF, 1, 0xFF, 2, 0xFF, 3, 0xFF

cdb:            times 12 db 0
saved_cdb:      times 12 db 0
sense_data:     times 18 db 0
response_data:  times 64 db 0
mode_data:      times 24 db 0

xfer_dir:       db DIR_NONE
xfer_seg:       dw 0
xfer_off:       dw 0
xfer_left:      dw 0
retry_xfer_dir: db 0
retry_xfer_seg: dw 0
retry_xfer_off: dw 0
retry_xfer_len: dw 0
poll_outer:     dw 0
poll_inner:     dw 0
tick_start:     dw 0

strategy:
    mov [cs:request_ptr], bx
    mov [cs:request_ptr + 2], es
    retf

; DOS can call the interrupt entry on a small caller stack. All work runs on
; this driver's guarded private stack, with maskable interrupts enabled while
; the ATAPI device is polled.
interrupt:
    pushf
    cli
    push ax
    mov [cs:old_ss], ss
    mov [cs:old_sp], sp
    mov ax, cs
    mov ss, ax
    mov sp, private_stack_top
    sti

    push bx
    push cx
    push dx
    push si
    push di
    push bp
    push ds
    push es
    cld
    push cs
    pop ds

    call dispatch_request

    ; AX is either zero or ST_ERROR plus a DOS driver error code.
    push ax
    cmp byte [cs:audio_playing], 0
    je .status_ready
    call refresh_audio_state
.status_ready:
    pop ax
    or ax, ST_DONE
    cmp byte [cs:audio_playing], 0
    je .store_status
    or ax, ST_BUSY
.store_status:
    les bx, [cs:request_ptr]
    mov [es:bx + RH_STATUS], ax

    pop es
    pop ds
    pop bp
    pop di
    pop si
    pop dx
    pop cx
    pop bx

    cli
    mov ax, [cs:old_ss]
    mov ss, ax
    mov sp, [cs:old_sp]
    pop ax
    popf
    retf

dispatch_request:
    les bx, [cs:request_ptr]
    mov word [es:bx + RH_STATUS], 0
    mov al, [es:bx + RH_COMMAND]
    cmp al, 0
    je .init
    cmp byte [es:bx + RH_UNIT], 0
    jne .bad_unit

    cmp al, 3
    je .ioctl_in
    cmp al, 7
    je .short_ok
    cmp al, 12
    je .ioctl_out
    cmp al, 13
    je .open
    cmp al, 14
    je .close
    cmp al, 128
    je .read_long
    cmp al, 130
    je .prefetch
    cmp al, 131
    je .seek
    cmp al, 132
    je .play
    cmp al, 133
    je .stop
    cmp al, 136
    je .resume
    mov ax, ST_ERROR | ERR_COMMAND
    ret

.bad_unit:
    mov ax, ST_ERROR | ERR_UNIT
    ret

.init:
    cmp byte [es:bx + RH_LENGTH], 23
    jb .bad_length
    call request_init
    ret
.ioctl_in:
    cmp byte [es:bx + RH_LENGTH], 26
    jb .bad_length
    call ioctl_input
    ret
.ioctl_out:
    cmp byte [es:bx + RH_LENGTH], 26
    jb .bad_length
    call ioctl_output
    ret
.short_ok:
    cmp byte [es:bx + RH_LENGTH], 13
    jb .bad_length
    xor ax, ax
    ret
.open:
    cmp byte [es:bx + RH_LENGTH], 13
    jb .bad_length
    inc word [cs:open_count]
    xor ax, ax
    ret
.close:
    cmp byte [es:bx + RH_LENGTH], 13
    jb .bad_length
    cmp word [cs:open_count], 0
    je .close_done
    dec word [cs:open_count]
.close_done:
    xor ax, ax
    ret
.read_long:
    cmp byte [es:bx + RH_LENGTH], 27
    jb .bad_length
    call request_read_long
    ret
.prefetch:
    cmp byte [es:bx + RH_LENGTH], 27
    jb .bad_length
    call request_seek
    ret
.seek:
    cmp byte [es:bx + RH_LENGTH], 24
    jb .bad_length
    call request_seek
    ret
.play:
    cmp byte [es:bx + RH_LENGTH], 22
    jb .bad_length
    call request_play
    ret
.stop:
    cmp byte [es:bx + RH_LENGTH], 13
    jb .bad_length
    call request_stop
    ret
.resume:
    cmp byte [es:bx + RH_LENGTH], 13
    jb .bad_length
    call request_resume
    ret
.bad_length:
    mov ax, ST_ERROR | ERR_LENGTH
    ret

request_init:
    ; DOS requires zero units in the INIT packet for character devices. MSCDEX
    ; reads the actual unit count from the extended device header.
    mov byte [es:bx + RH_DATA], 0
    mov word [es:bx + RH_DATA + 1], resident_end
    mov word [es:bx + RH_DATA + 3], cs
    mov word [es:bx + RH_DATA + 5], 0
    mov word [es:bx + RH_DATA + 7], 0
    mov byte [es:bx + RH_DATA + 9], 0

    mov byte [cs:media_state], MEDIA_UNKNOWN
    mov byte [cs:media_latch], 0
    mov byte [cs:door_open], 0
    mov byte [cs:door_locked], 0
    mov byte [cs:audio_playing], 0
    mov byte [cs:audio_paused], 0
    call probe_drive
    jnc .ok

    ; Returning an end offset of zero tells DOS to discard a driver whose fixed
    ; Izarra ATAPI endpoint is not present.
    les bx, [cs:request_ptr]
    mov word [es:bx + RH_DATA + 1], 0
    mov word [es:bx + RH_DATA + 3], cs
    mov ax, ST_ERROR | ERR_GENERAL
    ret
.ok:
    call clear_audio_state
    xor ax, ax
    ret

; IOCTL input control blocks.
ioctl_input:
    les bx, [cs:request_ptr]
    les di, [es:bx + RH_IOCTL_PTR]
    mov al, [es:di]
    cmp al, 0
    je ioctl_get_header
    cmp al, 1
    je ioctl_head
    cmp al, 4
    je ioctl_get_channels
    cmp al, 6
    je ioctl_device_status
    cmp al, 7
    je ioctl_sector_size
    cmp al, 8
    je ioctl_volume_size
    cmp al, 9
    je ioctl_media_change
    cmp al, 10
    je ioctl_audio_disk
    cmp al, 11
    je ioctl_audio_track
    cmp al, 12
    je ioctl_audio_q
    cmp al, 15
    je ioctl_audio_status
    mov ax, ST_ERROR | ERR_COMMAND
    ret

ioctl_get_header:
    call require_cb_5
    jc ioctl_bad_length
    mov word [es:di + 1], device_header
    mov word [es:di + 3], cs
    xor ax, ax
    ret

ioctl_head:
    call require_cb_6
    jc ioctl_bad_length
    cmp byte [es:di + 1], 1
    ja ioctl_unknown
    cmp byte [cs:audio_playing], 0
    jne .audio_position
    cmp byte [cs:audio_paused], 0
    jne .audio_position
    mov ax, [cs:head_lba]
    mov dx, [cs:head_lba + 2]
    cmp byte [es:di + 1], 0
    je .store
    call lba_to_packed_msf
.store:
    mov [es:di + 2], ax
    mov [es:di + 4], dx
    xor ax, ax
    ret
.audio_position:
    push es
    push di
    call read_subchannel
    jc .audio_error
    pop di
    pop es
    mov al, [cs:response_data + 11]
    mov ah, [cs:response_data + 10]
    xor dx, dx
    mov dl, [cs:response_data + 9]
    push ax
    push dx
    call packed_msf_to_lba
    jc .audio_address_error
    mov [cs:head_lba], ax
    mov [cs:head_lba + 2], dx
    cmp byte [es:di + 1], 0
    je .discard_packed
    pop dx
    pop ax
    jmp .store
.discard_packed:
    pop cx
    pop cx
    jmp .store
.audio_address_error:
    pop cx
    pop cx
    jmp ioctl_unknown
.audio_error:
    pop di
    pop es
    or ax, ST_ERROR
    ret

ioctl_get_channels:
    call require_cb_9
    jc ioctl_bad_length
    push ds
    push cs
    pop ds
    mov si, audio_channels
    inc di
    mov cx, 8
    rep movsb
    pop ds
    xor ax, ax
    ret

ioctl_device_status:
    call require_cb_5
    jc ioctl_bad_length
    ; Cooked reads, read-only media, audio play, channel control, HSG and Red
    ; Book addressing. Bit 1 is one only while the door is unlocked.
    mov ax, 0x0310
    cmp byte [cs:door_open], 0
    je .closed
    or ax, 0x0001
.closed:
    cmp byte [cs:door_locked], 0
    jne .locked
    or ax, 0x0002
.locked:
    mov [es:di + 1], ax
    mov word [es:di + 3], 0
    xor ax, ax
    ret

ioctl_sector_size:
    call require_cb_4
    jc ioctl_bad_length
    cmp byte [es:di + 1], 0
    jne ioctl_unknown
    mov word [es:di + 2], 2048
    xor ax, ax
    ret

ioctl_volume_size:
    call require_cb_5
    jc ioctl_bad_length
    push es
    push di
    call ensure_ready
    jc .error
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x25
    mov cx, 8
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    ; READ CAPACITY returns the last addressable LBA in big-endian order.
    mov ah, [cs:response_data]
    mov al, [cs:response_data + 1]
    mov dh, [cs:response_data + 2]
    mov dl, [cs:response_data + 3]
    xchg ax, dx
    add ax, 1
    adc dx, 0
    pop di
    pop es
    mov [es:di + 1], ax
    mov [es:di + 3], dx
    xor ax, ax
    ret
.error:
    pop di
    pop es
    or ax, ST_ERROR
    ret

ioctl_media_change:
    call require_cb_2
    jc ioctl_bad_length
    push es
    push di
    call ensure_ready
    pop di
    pop es
    cmp byte [cs:media_latch], 0
    je .not_latched
    mov byte [es:di + 1], 0xFF
    mov byte [cs:media_latch], 0
    xor ax, ax
    ret
.not_latched:
    cmp byte [cs:media_state], MEDIA_PRESENT
    jne .unknown
    mov byte [es:di + 1], 1
    xor ax, ax
    ret
.unknown:
    mov byte [es:di + 1], 0
    xor ax, ax
    ret

ioctl_audio_disk:
    call require_cb_7
    jc ioctl_bad_length
    push es
    push di
    call ensure_ready
    jc .error
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x43
    mov byte [cs:saved_cdb + 1], 0x02
    mov byte [cs:saved_cdb + 6], 0xAA
    mov byte [cs:saved_cdb + 8], 12
    mov cx, 12
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    pop di
    pop es
    mov al, [cs:response_data + 2]
    mov [es:di + 1], al
    mov al, [cs:response_data + 3]
    mov [es:di + 2], al
    mov al, [cs:response_data + 11]
    mov [es:di + 3], al
    mov al, [cs:response_data + 10]
    mov [es:di + 4], al
    mov al, [cs:response_data + 9]
    mov [es:di + 5], al
    mov byte [es:di + 6], 0
    xor ax, ax
    ret
.error:
    pop di
    pop es
    or ax, ST_ERROR
    ret

ioctl_audio_track:
    call require_cb_7
    jc ioctl_bad_length
    mov al, [es:di + 1]
    push es
    push di
    push ax
    call ensure_ready
    jc .error_pop_track
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x43
    mov byte [cs:saved_cdb + 1], 0x02
    pop ax
    mov [cs:saved_cdb + 6], al
    mov byte [cs:saved_cdb + 8], 12
    mov cx, 12
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    pop di
    pop es
    mov al, [cs:response_data + 11]
    mov [es:di + 2], al
    mov al, [cs:response_data + 10]
    mov [es:di + 3], al
    mov al, [cs:response_data + 9]
    mov [es:di + 4], al
    mov byte [es:di + 5], 0
    mov al, [cs:response_data + 5]
    call swap_nibbles
    mov [es:di + 6], al
    xor ax, ax
    ret
.error_pop_track:
    pop dx
.error:
    pop di
    pop es
    or ax, ST_ERROR
    ret

ioctl_audio_q:
    call require_cb_11
    jc ioctl_bad_length
    push es
    push di
    call read_subchannel
    jc .error
    pop di
    pop es
    mov al, [cs:response_data + 5]
    call swap_nibbles
    mov [es:di + 1], al
    mov al, [cs:response_data + 6]
    mov [es:di + 2], al
    mov al, [cs:response_data + 7]
    mov [es:di + 3], al
    mov al, [cs:response_data + 13]
    mov [es:di + 4], al
    mov al, [cs:response_data + 14]
    mov [es:di + 5], al
    mov al, [cs:response_data + 15]
    mov [es:di + 6], al
    mov byte [es:di + 7], 0
    mov al, [cs:response_data + 9]
    mov [es:di + 8], al
    mov al, [cs:response_data + 10]
    mov [es:di + 9], al
    mov al, [cs:response_data + 11]
    mov [es:di + 10], al
    xor ax, ax
    ret
.error:
    pop di
    pop es
    or ax, ST_ERROR
    ret

ioctl_audio_status:
    call require_cb_11
    jc ioctl_bad_length
    xor ax, ax
    cmp byte [cs:audio_paused], 0
    je .flags
    inc ax
.flags:
    mov [es:di + 1], ax
    mov ax, [cs:last_start]
    mov [es:di + 3], ax
    mov ax, [cs:last_start + 2]
    mov [es:di + 5], ax
    mov ax, [cs:last_end]
    mov [es:di + 7], ax
    mov ax, [cs:last_end + 2]
    mov [es:di + 9], ax
    xor ax, ax
    ret

ioctl_bad_length:
    mov ax, ST_ERROR | ERR_LENGTH
    ret
ioctl_unknown:
    mov ax, ST_ERROR | ERR_COMMAND
    ret

; IOCTL output control blocks.
ioctl_output:
    les bx, [cs:request_ptr]
    les di, [es:bx + RH_IOCTL_PTR]
    mov al, [es:di]
    cmp al, 0
    je ioctl_eject
    cmp al, 1
    je ioctl_lock
    cmp al, 2
    je ioctl_reset
    cmp al, 3
    je ioctl_set_channels
    cmp al, 5
    je ioctl_close_tray
    mov ax, ST_ERROR | ERR_COMMAND
    ret

ioctl_eject:
    call require_cb_1
    jc ioctl_bad_length
    ; Eject first releases a lock, as required by the DOS CD interface.
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x1E
    mov byte [cs:saved_cdb + 4], 0
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x1B
    mov byte [cs:saved_cdb + 4], 0x02
    call prepare_no_transfer
    call execute_checked
    jc .error
    mov byte [cs:door_locked], 0
    mov byte [cs:door_open], 1
    mov byte [cs:media_state], MEDIA_ABSENT
    mov byte [cs:media_latch], 1
    call clear_audio_state
    call clear_head_lba
    xor ax, ax
    ret
.error:
    or ax, ST_ERROR
    ret

ioctl_lock:
    call require_cb_2
    jc ioctl_bad_length
    mov al, [es:di + 1]
    and al, 1
    mov ah, al
    push ax
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x1E
    mov [cs:saved_cdb + 4], ah
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    pop dx
    jc .error
    mov [cs:door_locked], dh
    xor ax, ax
    ret
.error:
    or ax, ST_ERROR
    ret

ioctl_reset:
    call require_cb_1
    jc ioctl_bad_length
    call hardware_reset
    jc .error
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x4E
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .command_error
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x1E
    mov byte [cs:saved_cdb + 4], 0
    call prepare_no_transfer
    call execute_checked
    jc .command_error
    mov byte [cs:door_locked], 0
    mov byte [cs:door_open], 0
    mov byte [cs:media_state], MEDIA_UNKNOWN
    mov byte [cs:media_latch], 1
    call clear_audio_state
    call clear_head_lba
    xor ax, ax
    ret
.error:
    mov ax, ST_ERROR | ERR_GENERAL
    ret
.command_error:
    or ax, ST_ERROR
    ret

ioctl_set_channels:
    call require_cb_9
    jc ioctl_bad_length
    push es
    push di
    push ds
    push cs
    pop ds
    mov si, di
    inc si
    mov di, audio_channels
    mov cx, 8
    ; The source segment is the caller's control block.
.copy_channels:
    mov al, [es:si]
    mov [di], al
    inc si
    inc di
    loop .copy_channels
    pop ds
    pop di
    pop es

    push cs
    pop es
    mov di, mode_data
    xor ax, ax
    mov cx, 12
    rep stosw
    mov byte [cs:mode_data + 8], 0x0E
    mov byte [cs:mode_data + 9], 14
    mov al, [cs:audio_channels]
    mov [cs:mode_data + 16], al
    mov al, [cs:audio_channels + 1]
    mov [cs:mode_data + 17], al
    mov al, [cs:audio_channels + 2]
    mov [cs:mode_data + 18], al
    mov al, [cs:audio_channels + 3]
    mov [cs:mode_data + 19], al

    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x55
    mov byte [cs:saved_cdb + 1], 0x10
    mov byte [cs:saved_cdb + 8], 24
    mov byte [cs:xfer_dir], DIR_OUT
    mov word [cs:xfer_seg], cs
    mov word [cs:xfer_off], mode_data
    mov word [cs:xfer_left], 24
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    xor ax, ax
    ret
.error:
    or ax, ST_ERROR
    ret

ioctl_close_tray:
    call require_cb_1
    jc ioctl_bad_length
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x1B
    mov byte [cs:saved_cdb + 4], 0x03
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    mov byte [cs:door_open], 0
    mov byte [cs:media_state], MEDIA_UNKNOWN
    xor ax, ax
    ret
.error:
    or ax, ST_ERROR
    ret

request_read_long:
    les bx, [cs:request_ptr]
    cmp byte [es:bx + RH_READ_ADDR], 1
    ja .unsupported
    cmp byte [es:bx + RH_READ_MODE], 0
    jne .unsupported
    call request_start_lba
    jc .unsupported
    mov [cs:io_lba], ax
    mov [cs:io_lba + 2], dx

    mov cx, [es:bx + RH_READ_COUNT]
    or cx, cx
    jnz .have_count
    jmp .ok
.have_count:
    mov si, cx
    push word [es:bx + RH_READ_BUFFER]
    push word [es:bx + RH_READ_BUFFER + 2]
    call ensure_ready
    jc .ready_error
    pop dx
    pop ax
    call normalize_far_pointer
    mov [cs:xfer_off], ax
    mov [cs:xfer_seg], dx
.sector_loop:
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x28
    mov ax, [cs:io_lba]
    mov dx, [cs:io_lba + 2]
    call put_cdb_lba
    mov byte [cs:saved_cdb + 8], 1
    mov byte [cs:xfer_dir], DIR_IN
    mov word [cs:xfer_left], 2048
    mov byte [cs:error_context], 1
    call execute_checked
    jc .error
    call clear_audio_state

    mov ax, [cs:xfer_off]
    mov dx, [cs:xfer_seg]
    call normalize_far_pointer
    mov [cs:xfer_off], ax
    mov [cs:xfer_seg], dx
    add word [cs:io_lba], 1
    adc word [cs:io_lba + 2], 0
    mov ax, [cs:io_lba]
    mov [cs:head_lba], ax
    mov ax, [cs:io_lba + 2]
    mov [cs:head_lba + 2], ax
    dec si
    jnz .sector_loop
.ok:
    xor ax, ax
    ret
.unsupported:
    mov ax, ST_ERROR | ERR_COMMAND
    ret
.ready_error:
    pop dx
    pop dx
.error:
    or ax, ST_ERROR
    ret

request_seek:
    les bx, [cs:request_ptr]
    cmp byte [es:bx + RH_READ_ADDR], 1
    ja .unsupported
    call request_start_lba
    jc .unsupported
    mov [cs:io_lba], ax
    mov [cs:io_lba + 2], dx
    push ax
    push dx
    call ensure_ready
    jc .error_pop
    call clear_saved_cdb
    pop dx
    pop ax
    mov byte [cs:saved_cdb], 0x2B
    call put_cdb_lba
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    mov ax, [cs:io_lba]
    mov [cs:head_lba], ax
    mov ax, [cs:io_lba + 2]
    mov [cs:head_lba + 2], ax
    call clear_audio_state
    xor ax, ax
    ret
.error_pop:
    pop dx
    pop dx
.error:
    or ax, ST_ERROR
    ret
.unsupported:
    mov ax, ST_ERROR | ERR_COMMAND
    ret

request_play:
    les bx, [cs:request_ptr]
    cmp byte [es:bx + RH_PLAY_ADDR], 1
    ja .unsupported
    mov al, [es:bx + RH_PLAY_ADDR]
    mov [cs:play_addr_mode], al
    mov ax, [es:bx + RH_PLAY_START]
    mov dx, [es:bx + RH_PLAY_START + 2]
    mov [cs:play_input_start], ax
    mov [cs:play_input_start + 2], dx
    mov ax, [es:bx + RH_PLAY_COUNT]
    mov dx, [es:bx + RH_PLAY_COUNT + 2]
    mov [cs:play_count], ax
    mov [cs:play_count + 2], dx

    cmp byte [cs:play_addr_mode], 0
    je .address_syntax_ok
    mov ax, [cs:play_input_start]
    cmp al, 75
    jae .range_error
    cmp ah, 60
    jae .range_error
    mov ax, [cs:play_input_start + 2]
    or ah, ah
    jnz .range_error
.address_syntax_ok:
    call get_disc_capacity
    jc .error
    mov [cs:disc_capacity], ax
    mov [cs:disc_capacity + 2], dx

    mov ax, [cs:play_input_start]
    mov dx, [cs:play_input_start + 2]
    cmp byte [cs:play_addr_mode], 0
    je .start_ready
    call packed_msf_to_lba
    jc .range_error
.start_ready:
    cmp dx, [cs:disc_capacity + 2]
    ja .range_error
    jb .start_in_range
    cmp ax, [cs:disc_capacity]
    jae .range_error
.start_in_range:
    mov [cs:play_lba_start], ax
    mov [cs:play_lba_start + 2], dx
    push ax
    push dx
    call lba_to_packed_msf
    mov [cs:play_msf_start], ax
    mov [cs:play_msf_start + 2], dx
    pop dx
    pop ax
    cmp byte [cs:play_addr_mode], 0
    jne .red_start
    mov [cs:play_stat_start], ax
    mov [cs:play_stat_start + 2], dx
    jmp .start_saved
.red_start:
    mov cx, [cs:play_msf_start]
    mov [cs:play_stat_start], cx
    mov cx, [cs:play_msf_start + 2]
    mov [cs:play_stat_start + 2], cx
.start_saved:

    add ax, [cs:play_count]
    adc dx, [cs:play_count + 2]
    jc .range_error
    cmp dx, [cs:disc_capacity + 2]
    ja .range_error
    jb .end_in_range
    cmp ax, [cs:disc_capacity]
    ja .range_error
.end_in_range:
    push ax
    push dx
    call lba_to_packed_msf
    mov [cs:play_msf_end], ax
    mov [cs:play_msf_end + 2], dx
    pop dx
    pop ax
    cmp byte [cs:play_addr_mode], 0
    jne .red_end
    mov [cs:play_stat_end], ax
    mov [cs:play_stat_end + 2], dx
    jmp .end_saved
.red_end:
    mov cx, [cs:play_msf_end]
    mov [cs:play_stat_end], cx
    mov cx, [cs:play_msf_end + 2]
    mov [cs:play_stat_end + 2], cx
.end_saved:
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x47
    mov al, [cs:play_msf_start + 2]
    mov [cs:saved_cdb + 3], al
    mov al, [cs:play_msf_start + 1]
    mov [cs:saved_cdb + 4], al
    mov al, [cs:play_msf_start]
    mov [cs:saved_cdb + 5], al
    mov al, [cs:play_msf_end + 2]
    mov [cs:saved_cdb + 6], al
    mov al, [cs:play_msf_end + 1]
    mov [cs:saved_cdb + 7], al
    mov al, [cs:play_msf_end]
    mov [cs:saved_cdb + 8], al
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    mov ax, [cs:play_count]
    or ax, [cs:play_count + 2]
    jz .empty_range
    mov al, [cs:play_addr_mode]
    mov [cs:last_addr_mode], al
    mov ax, [cs:play_stat_start]
    mov [cs:last_start], ax
    mov ax, [cs:play_stat_start + 2]
    mov [cs:last_start + 2], ax
    mov ax, [cs:play_stat_end]
    mov [cs:last_end], ax
    mov ax, [cs:play_stat_end + 2]
    mov [cs:last_end + 2], ax
    mov ax, [cs:play_lba_start]
    mov [cs:head_lba], ax
    mov ax, [cs:play_lba_start + 2]
    mov [cs:head_lba + 2], ax
    mov byte [cs:audio_paused], 0
    mov byte [cs:audio_playing], 1
    xor ax, ax
    ret
.empty_range:
    call clear_audio_state
    xor ax, ax
    ret
.unsupported:
    mov ax, ST_ERROR | ERR_COMMAND
    ret
.range_error:
    mov ax, ST_ERROR | ERR_SECTOR
    ret
.error:
    or ax, ST_ERROR
    ret

request_stop:
    cmp byte [cs:audio_playing], 0
    jne .query_position
    cmp byte [cs:audio_paused], 0
    jne .already_paused
    jmp .not_playing
.query_position:
    call read_subchannel
    jc .error
    cmp byte [cs:audio_playing], 0
    je .not_playing
    ; Save the current absolute position as the point RESUME continues from.
    mov al, [cs:response_data + 11]
    mov ah, [cs:response_data + 10]
    xor dx, dx
    mov dl, [cs:response_data + 9]
    push ax
    push dx
    call packed_msf_to_lba
    jc .address_error
    mov [cs:head_lba], ax
    mov [cs:head_lba + 2], dx
    cmp byte [cs:last_addr_mode], 0
    jne .keep_red_book
    mov [cs:last_start], ax
    mov [cs:last_start + 2], dx
    pop dx
    pop ax
    jmp .position_saved
.keep_red_book:
    pop dx
    pop ax
    mov [cs:last_start], ax
    mov [cs:last_start + 2], dx
.position_saved:

    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x4B
    mov byte [cs:saved_cdb + 8], 0
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    mov byte [cs:audio_playing], 0
    mov byte [cs:audio_paused], 1
    xor ax, ax
    ret
.address_error:
    pop dx
    pop dx
    mov ax, ST_ERROR | ERR_GENERAL
    ret
.already_paused:
    xor ax, ax
    ret
.not_playing:
    call clear_audio_state
    xor ax, ax
    ret
.error:
    or ax, ST_ERROR
    ret

request_resume:
    cmp byte [cs:audio_paused], 0
    je .error_general
    call ensure_ready
    jc .error
    cmp byte [cs:audio_paused], 0
    je .error_general
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x4B
    mov byte [cs:saved_cdb + 8], 1
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .error
    call read_subchannel
    jc .confirm_error
    cmp byte [cs:audio_playing], 0
    je .error_general
    xor ax, ax
    ret
.confirm_error:
    call clear_audio_state
    or ax, ST_ERROR
    ret
.error_general:
    mov ax, ST_ERROR | ERR_GENERAL
    ret
.error:
    or ax, ST_ERROR
    ret

; Return the request's start address as DX:AX LBA.
request_start_lba:
    mov ax, [es:bx + RH_READ_START]
    mov dx, [es:bx + RH_READ_START + 2]
    cmp byte [es:bx + RH_READ_ADDR], 0
    je .ok
    call packed_msf_to_lba
    ret
.ok:
    clc
    ret

; Read current-position subchannel data and update the cached play flags.
read_subchannel:
    call ensure_ready
    jc .done
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x42
    mov byte [cs:saved_cdb + 1], 0x02
    mov byte [cs:saved_cdb + 2], 0x40
    mov byte [cs:saved_cdb + 3], 0x01
    mov byte [cs:saved_cdb + 8], 16
    mov cx, 16
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .done
    mov al, [cs:response_data + 11]
    mov ah, [cs:response_data + 10]
    xor dx, dx
    mov dl, [cs:response_data + 9]
    call packed_msf_to_lba
    jc .bad_position
    mov [cs:head_lba], ax
    mov [cs:head_lba + 2], dx
    mov al, [cs:response_data + 1]
    cmp al, 0x11
    jne .not_playing
    mov byte [cs:audio_playing], 1
    mov byte [cs:audio_paused], 0
    clc
    ret
.not_playing:
    mov byte [cs:audio_playing], 0
    cmp al, 0x12
    jne .stopped
    mov byte [cs:audio_paused], 1
    clc
    ret
.stopped:
    call clear_audio_state
    clc
.bad_position:
.done:
    ret

refresh_audio_state:
    ; This direct query does not recurse through final status generation.
    call read_subchannel
    ret

clear_audio_state:
    mov byte [cs:audio_playing], 0
    mov byte [cs:audio_paused], 0
    mov byte [cs:last_addr_mode], 0
    mov word [cs:last_start], 0
    mov word [cs:last_start + 2], 0
    mov word [cs:last_end], 0
    mov word [cs:last_end + 2], 0
    ret

clear_head_lba:
    mov word [cs:head_lba], 0
    mov word [cs:head_lba + 2], 0
    ret

; Return total addressable sectors in DX:AX. READ CAPACITY reports the last
; valid LBA, so the driver adds one after converting the big-endian reply.
get_disc_capacity:
    call ensure_ready
    jc .done
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x25
    mov cx, 8
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .done
    mov ah, [cs:response_data]
    mov al, [cs:response_data + 1]
    mov dh, [cs:response_data + 2]
    mov dl, [cs:response_data + 3]
    xchg ax, dx
    add ax, 1
    adc dx, 0
    clc
.done:
    ret

; TEST UNIT READY is the media-change observation point. Unit attention is
; cleared and retried by execute_checked, while media_latch remains set until
; DOS consumes IOCTL input 9.
ensure_ready:
    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x00
    call prepare_no_transfer
    mov byte [cs:error_context], 0
    call execute_checked
    jc .not_ready
    mov byte [cs:media_state], MEDIA_PRESENT
    mov byte [cs:door_open], 0
    clc
    ret
.not_ready:
    cmp al, ERR_NOT_READY
    jne .done
    cmp byte [cs:media_state], MEDIA_PRESENT
    jne .remember_absent
    mov byte [cs:media_latch], 1
.remember_absent:
    mov byte [cs:media_state], MEDIA_ABSENT
    call clear_audio_state
    call clear_head_lba
.done:
    stc
    ret

probe_drive:
    call hardware_reset
    jc .fail
    mov dx, ATA_LBA_MID
    in al, dx
    cmp al, 0x14
    jne .fail
    inc dx
    in al, dx
    cmp al, 0xEB
    jne .fail

    call clear_saved_cdb
    mov byte [cs:saved_cdb], 0x12
    mov byte [cs:saved_cdb + 4], 36
    mov cx, 36
    call prepare_response_input
    mov byte [cs:error_context], 0
    call execute_checked
    jc .fail
    mov al, [cs:response_data]
    and al, 0x1F
    cmp al, 5
    jne .fail
    clc
    ret
.fail:
    stc
    ret

hardware_reset:
    mov dx, ATA_CONTROL
    mov al, 0x06                  ; nIEN plus SRST
    out dx, al
    mov al, 0x02                  ; release reset, keep nIEN
    out dx, al
    call wait_not_busy
    ret

; Execute saved_cdb. On CHECK CONDITION, fetch sense, preserve unit attention
; in media_latch, and retry the original packet once.
execute_checked:
    mov al, [cs:xfer_dir]
    mov [cs:retry_xfer_dir], al
    mov ax, [cs:xfer_seg]
    mov [cs:retry_xfer_seg], ax
    mov ax, [cs:xfer_off]
    mov [cs:retry_xfer_off], ax
    mov ax, [cs:xfer_left]
    mov [cs:retry_xfer_len], ax
    call copy_saved_cdb
    call packet_transport
    jnc .ok
    call request_sense
    jc .transport_error
    mov al, [cs:sense_data + 2]
    and al, 0x0F
    cmp al, 0x06
    jne .map
    cmp byte [cs:sense_data + 12], 0x28
    jne .map
    mov byte [cs:media_latch], 1
    call clear_audio_state
    call clear_head_lba
    mov al, [cs:retry_xfer_dir]
    mov [cs:xfer_dir], al
    mov ax, [cs:retry_xfer_seg]
    mov [cs:xfer_seg], ax
    mov ax, [cs:retry_xfer_off]
    mov [cs:xfer_off], ax
    mov ax, [cs:retry_xfer_len]
    mov [cs:xfer_left], ax
    call copy_saved_cdb
    call packet_transport
    jnc .ok
    call request_sense
    jc .transport_error
.map:
    call map_sense_error
    stc
    ret
.transport_error:
    mov ax, ERR_GENERAL
    stc
    ret
.ok:
    xor ax, ax
    clc
    ret

request_sense:
    push es
    push di
    push cx
    call clear_cdb
    mov byte [cs:cdb], 0x03
    mov byte [cs:cdb + 4], 18
    mov byte [cs:xfer_dir], DIR_IN
    mov word [cs:xfer_seg], cs
    mov word [cs:xfer_off], sense_data
    mov word [cs:xfer_left], 18
    call packet_transport
    pop cx
    pop di
    pop es
    ret

map_sense_error:
    mov al, [cs:sense_data + 2]
    and al, 0x0F
    mov ah, [cs:sense_data + 12]
    cmp al, 0x02
    jne .not_no_media
    cmp ah, 0x3A
    jne .not_no_media
    mov al, ERR_NOT_READY
    xor ah, ah
    ret
.not_no_media:
    cmp al, 0x06
    jne .not_changed
    cmp ah, 0x28
    jne .not_changed
    mov byte [cs:media_latch], 1
    mov al, ERR_CHANGED
    xor ah, ah
    ret
.not_changed:
    cmp ah, 0x21
    jne .not_range
    mov al, ERR_SECTOR
    xor ah, ah
    ret
.not_range:
    cmp byte [cs:error_context], 0
    je .general
    mov al, ERR_READ
    xor ah, ah
    ret
.general:
    mov al, ERR_GENERAL
    xor ah, ah
    ret

; Polled ATAPI packet transport. The host byte-count limit is the requested
; transfer size. Every DRQ block is drained before completion.
packet_transport:
    push bx
    push cx
    push dx
    push si
    push di
    push es

    mov dx, ATA_CONTROL
    mov al, 0x02
    out dx, al
    mov dx, ATA_DEVICE
    mov al, 0xA0
    out dx, al
    mov dx, ATA_FEATURES
    xor al, al
    out dx, al
    mov dx, ATA_COUNT
    out dx, al
    inc dx
    out dx, al
    mov ax, [cs:xfer_left]
    or ax, ax
    jnz .limit_ready
    mov ax, 0x0800
.limit_ready:
    mov dx, ATA_LBA_MID
    out dx, al
    inc dx
    mov al, ah
    out dx, al
    mov dx, ATA_COMMAND
    mov al, 0xA0
    out dx, al

    call wait_drq
    jc .failed
    mov dx, ATA_DATA
    mov si, cdb
    mov cx, 6
.cdb_words:
    mov ax, [cs:si]
    out dx, ax
    add si, 2
    loop .cdb_words

.phase:
    call wait_not_busy
    jc .failed
    mov dx, ATA_CONTROL
    in al, dx
    test al, ATA_ERR
    jnz .failed
    test al, ATA_DRQ
    jz .success

    mov dx, ATA_LBA_MID
    in al, dx
    mov cl, al
    inc dx
    in al, dx
    mov ch, al
    or cx, cx
    jz .failed
    test cx, 1
    jnz .failed

    cmp byte [cs:xfer_dir], DIR_IN
    je .data_in
    cmp byte [cs:xfer_dir], DIR_OUT
    je .data_out
    jmp .drain

.data_in:
    mov ax, [cs:xfer_seg]
    mov es, ax
    mov di, [cs:xfer_off]
    mov dx, ATA_DATA
    shr cx, 1
.in_word:
    in ax, dx
    cmp word [cs:xfer_left], 2
    jb .discard_word
    mov [es:di], ax
    add di, 2
    sub word [cs:xfer_left], 2
    jmp .in_next
.discard_word:
    mov word [cs:xfer_left], 0
.in_next:
    loop .in_word
    mov [cs:xfer_off], di
    jmp .phase

.data_out:
    mov ax, [cs:xfer_seg]
    mov es, ax
    mov di, [cs:xfer_off]
    mov dx, ATA_DATA
    shr cx, 1
.out_word:
    cmp word [cs:xfer_left], 2
    jb .zero_word
    mov ax, [es:di]
    add di, 2
    sub word [cs:xfer_left], 2
    jmp .send_word
.zero_word:
    xor ax, ax
    mov word [cs:xfer_left], 0
.send_word:
    out dx, ax
    loop .out_word
    mov [cs:xfer_off], di
    jmp .phase

.drain:
    mov dx, ATA_DATA
    shr cx, 1
.drain_word:
    in ax, dx
    loop .drain_word
    jmp .phase

.success:
    xor ax, ax
    clc
    jmp .done
.failed:
    mov ax, ERR_GENERAL
    stc
.done:
    pop es
    pop di
    pop si
    pop dx
    pop cx
    pop bx
    ret

wait_drq:
    call begin_timeout
.loop:
    mov dx, ATA_CONTROL
    in al, dx
    test al, ATA_BSY
    jnz .wait
    test al, ATA_ERR
    jnz .fail
    test al, ATA_DRQ
    jnz .ok
.wait:
    call timeout_expired
    jnc .loop
.fail:
    stc
    ret
.ok:
    clc
    ret

wait_not_busy:
    call begin_timeout
.loop:
    mov dx, ATA_CONTROL
    in al, dx
    test al, ATA_BSY
    jz .ok
    call timeout_expired
    jnc .loop
    stc
    ret
.ok:
    clc
    ret

; BIOS ticks provide a ten-second wall-clock timeout. The outer poll budget is
; a second finite guard for guests whose timer interrupt is unavailable.
begin_timeout:
    push es
    mov ax, 0x0040
    mov es, ax
    mov ax, [es:0x006C]
    mov [cs:tick_start], ax
    mov word [cs:poll_outer], 128
    mov word [cs:poll_inner], 0xFFFF
    pop es
    ret

timeout_expired:
    push ax
    push bx
    push cx
    push es
    mov ax, 0x0040
    mov es, ax
    mov ax, [es:0x006C]
    sub ax, [cs:tick_start]
    cmp ax, 182
    jae .expired
    dec word [cs:poll_inner]
    jnz .active
    mov word [cs:poll_inner], 0xFFFF
    dec word [cs:poll_outer]
    jz .expired
.active:
    pop es
    pop cx
    pop bx
    pop ax
    clc
    ret
.expired:
    pop es
    pop cx
    pop bx
    pop ax
    stc
    ret

prepare_no_transfer:
    mov byte [cs:xfer_dir], DIR_NONE
    mov word [cs:xfer_seg], 0
    mov word [cs:xfer_off], 0
    mov word [cs:xfer_left], 0
    ret

prepare_response_input:
    mov byte [cs:xfer_dir], DIR_IN
    mov word [cs:xfer_seg], cs
    mov word [cs:xfer_off], response_data
    mov [cs:xfer_left], cx
    ret

clear_cdb:
    push ax
    push cx
    push di
    push es
    push cs
    pop es
    mov di, cdb
    xor ax, ax
    mov cx, 6
    rep stosw
    pop es
    pop di
    pop cx
    pop ax
    ret

clear_saved_cdb:
    push ax
    push cx
    push di
    push es
    push cs
    pop es
    mov di, saved_cdb
    xor ax, ax
    mov cx, 6
    rep stosw
    pop es
    pop di
    pop cx
    pop ax
    ret

copy_saved_cdb:
    push ax
    push cx
    push si
    push di
    push ds
    push es
    push cs
    pop ds
    push cs
    pop es
    mov si, saved_cdb
    mov di, cdb
    mov cx, 6
    rep movsw
    pop es
    pop ds
    pop di
    pop si
    pop cx
    pop ax
    ret

; Put DX:AX LBA into bytes 2-5 of saved_cdb in big-endian order.
put_cdb_lba:
    mov [cs:saved_cdb + 2], dh
    mov [cs:saved_cdb + 3], dl
    mov [cs:saved_cdb + 4], ah
    mov [cs:saved_cdb + 5], al
    ret

; Normalize DX:AX to a far pointer with offset below 16.
normalize_far_pointer:
    push bx
    mov bx, ax
    mov cl, 4
    shr ax, cl
    add dx, ax
    and bx, 0x000F
    mov ax, bx
    pop bx
    ret

; Convert packed binary 00:MM:SS:FF in DX:AX into DX:AX LBA.
packed_msf_to_lba:
    push bx
    push cx
    mov bl, al                    ; frame
    mov bh, ah                    ; second
    mov ax, dx
    and ax, 0x00FF                ; minute
    mov cx, 60
    mul cx
    xor cx, cx
    mov cl, bh
    add ax, cx
    adc dx, 0
    mov cx, 75
    mul cx
    xor cx, cx
    mov cl, bl
    add ax, cx
    adc dx, 0
    sub ax, 150
    sbb dx, 0
    jc .bad
    pop cx
    pop bx
    clc
    ret
.bad:
    pop cx
    pop bx
    stc
    ret

; Convert DX:AX LBA into packed binary 00:MM:SS:FF in DX:AX.
lba_to_packed_msf:
    push bx
    push cx
    push si
    add ax, 150
    adc dx, 0
    mov cx, 75
    div cx                        ; AX seconds total, DX frame
    mov si, dx
    xor dx, dx
    mov cx, 60
    div cx                        ; AX minutes, DX seconds
    mov bl, dl
    mov bh, 0
    mov dx, ax
    and dx, 0x00FF
    mov ax, si
    and ax, 0x00FF
    mov ah, bl
    pop si
    pop cx
    pop bx
    ret

swap_nibbles:
    push bx
    mov bl, al
    mov cl, 4
    shr al, cl
    shl bl, cl
    or al, bl
    pop bx
    ret

; Control-block length checks use the IOCTL request's byte-count field.
require_cb_1:
    mov cx, 1
    jmp require_cb
require_cb_2:
    mov cx, 2
    jmp require_cb
require_cb_4:
    mov cx, 4
    jmp require_cb
require_cb_5:
    mov cx, 5
    jmp require_cb
require_cb_6:
    mov cx, 6
    jmp require_cb
require_cb_7:
    mov cx, 7
    jmp require_cb
require_cb_9:
    mov cx, 9
    jmp require_cb
require_cb_11:
    mov cx, 11
require_cb:
    push es
    push bx
    les bx, [cs:request_ptr]
    cmp [es:bx + RH_IOCTL_COUNT], cx
    pop bx
    pop es
    jb .bad
    clc
    ret
.bad:
    stc
    ret

align 2
private_stack_guard:
    dw 0xA55A
private_stack:
    times 512 db 0
private_stack_top:
    dw 0x5AA5

resident_end:
