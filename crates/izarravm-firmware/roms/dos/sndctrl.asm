; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; SNDCTRL.COM - ReSonique 2 Sound System Configuration.
;
; A text-mode setup tool for the card's IRQ/DMA assignment, in the spirit of the
; Miles Audio / SBCONFIG setup screens of the era: pick a value from the list the
; hardware actually supports, and never be offered one that would collide with
; the other device on the same card.
;
; What it changes, and when it takes effect:
;   * The live hardware, immediately. The SB16 mixer's Interrupt/DMA Setup
;     registers (0x80/0x81) and the WSS config register (0x531) are both
;     writable, so neither device needs a reboot to move.
;   * CMOS NVRAM 0x1B-0x21, so the assignment survives a power cycle. Those
;     bytes sit inside the 0x10..0x2D checksum window, so 0x2E/0x2F are
;     refreshed too -- without that the next POST discards the whole NVRAM as
;     corrupt, taking the keyboard layout and CPU speed down with it.
;   * BLASTER and SETSOUND in the shell's MASTER environment, so a program
;     started after this tool sees the new routing without a reboot.
;   * The matching SET lines in C:\AUTOEXEC.BAT, so the next boot agrees.
;
; Existing variables are updated, never created: a BLASTER the user deleted on
; purpose stays deleted, and the summary says so rather than inventing one.
;
; Usage:
;   SNDCTRL                     full-screen configuration
;   SNDCTRL /S                  print the current assignment and exit
;   SNDCTRL /SBIRQ:7 /MPU:330   set values from the command line and exit
;   SNDCTRL /B                  two-row boot summary and exit (no CMOS writes)
;   SNDCTRL /B /T               boot summary with a tree-styled prefix
;   SNDCTRL /?                  usage
;
; /T only styles the /B summary. Used alone, or paired with anything other
; than /B, it is accepted and simply has nothing to style -- a no-op, not an
; error, the same way /S with no value switches is a no-op on the hardware.
;
; Build: nasm -f bin sndctrl.asm -o sndctrl.com
    cpu 386
    org 0x100

; ---- hardware ---------------------------------------------------------------
SB_BASE        equ 0x220
SB_RESET       equ SB_BASE + 6         ; 0x226
SB_READ_DATA   equ SB_BASE + 0x0A      ; 0x22A
SB_READ_STATUS equ SB_BASE + 0x0E      ; 0x22E
SB_MIXER_IDX   equ SB_BASE + 4         ; 0x224
SB_MIXER_DAT   equ SB_BASE + 5         ; 0x225
MIX_IRQ_SETUP  equ 0x80
MIX_DMA_SETUP  equ 0x81
WSS_BASE       equ 0x530
WSS_ID         equ WSS_BASE            ; board/version ID; 0xFF means absent
WSS_CONFIG     equ WSS_BASE + 1        ; (irq << 4) | dma, readable AND writable

; ---- CMOS -------------------------------------------------------------------
; Mirrors `address::CMOS_*` in crates/izarravm-machine/src/firmware_contract.rs.
; Keep the two in step: the host reseeds the whole block if the magic is wrong.
CM_MAGIC       equ 0x1B
CM_MAGIC_VALUE equ 'R'
CM_SB_IRQ      equ 0x1C
CM_SB_DMA8     equ 0x1D
CM_SB_DMA16    equ 0x1E
CM_WSS_IRQ     equ 0x1F
CM_WSS_DMA     equ 0x20
CM_MPU_PORT    equ 0x21
CM_SUM_FIRST   equ 0x10
CM_SUM_LAST    equ 0x2D
CM_SUM_HI      equ 0x2E
CM_SUM_LO      equ 0x2F

; ---- Izarra 3000 palette ----------------------------------------------------
; Blink is turned off for the duration, so backgrounds 8-15 are real colours and
; the bright-white box and its dark-grey shadow render as intended.
A_BOX     equ 0xF0       ; black on bright white: body, borders, fixed values
A_TITLE   equ 0xF4       ; red on bright white: branding and section titles
A_FIELD   equ 0x0F       ; white on black: an editable value, drawn as an input
A_SEL     equ 0x4F       ; white on red: the selected input or menu row
A_SHADOW  equ 0x80       ; dark grey block under and beside the box

BOX_ROW   equ 1
BOX_COL   equ 4
BOX_W     equ 72
BOX_H     equ 21

; ---- field records (16 bytes each) ------------------------------------------
FLD_ROW   equ 0
FLD_COL   equ 1
FLD_WIDTH equ 2
FLD_FMT   equ 3          ; FMT_DEC or FMT_MPU
FLD_CMOS  equ 4
FLD_PEER  equ 5          ; field whose value must differ, or 0xFF
FLD_VALUE equ 6
FLD_DEV   equ 7          ; DEV_SB / DEV_WSS / DEV_ANY
FLD_OPTS  equ 8          ; word -> count byte, then the permitted values
FLD_NAME  equ 10         ; word -> ASCIIZ label, used by /S and the CLI errors
FLD_SIZE  equ 16

F_SBIRQ   equ 0
F_SBDMA   equ 1
F_SBD16   equ 2
F_WSIRQ   equ 3
F_WSDMA   equ 4
F_MPU     equ 5
F_COUNT   equ 6

DEV_SB    equ 0
DEV_WSS   equ 1
DEV_ANY   equ 2

FMT_DEC   equ 0
FMT_MPU   equ 1

TOKEN_MAX equ 12
AUTOEXEC_MAX equ 4096

%define FVAL(i) (fields + (i) * FLD_SIZE + FLD_VALUE)

; =============================================================================
start:
    cld
    call read_hardware
    call parse_tail
    jc .usage_error
    cmp byte [want_usage], 0
    jne .usage
    cmp byte [want_status], 0
    jne .status
    cmp byte [want_boot], 0
    jne .boot
    cmp byte [want_apply], 0
    jne .cli_apply
    jmp interactive

.usage_error:
    cmp byte [bad_kind], 0
    jne .value_error
    mov si, msg_bad_switch
    call print
    mov si, token
    call print
    mov si, msg_crlf
    call print
.usage:
    mov si, msg_usage
    call print
    mov ax, 0x4c01
    int 0x21

.value_error:
    movzx si, byte [cur_field]
    shl si, 4
    add si, fields
    push si
    mov si, [si + FLD_NAME]
    call print
    mov si, msg_permitted
    call print
    pop si
    call print_options
    mov si, msg_crlf
    call print
    mov ax, 0x4c01
    int 0x21

.status:
    call report
    mov ax, 0x4c00
    int 0x21

; /B is a read-only summary: it reuses read_hardware's already-probed state
; (the same fields report() prints) and never touches the mixer, WSS config
; register, CMOS, the environment, or AUTOEXEC.BAT.
.boot:
    call boot_report
    mov ax, 0x4c00
    int 0x21

; A command line that sets anything applies it and exits without drawing. It
; runs the same apply_all that the full-screen F10 runs, so a scripted change
; and a hand-made one cannot drift apart.
.cli_apply:
    call check_collisions
    jc .collision
    call apply_all
    call report
    mov ax, 0x4c00
    int 0x21
.collision:
    mov si, msg_collision
    call print
    mov ax, 0x4c01
    int 0x21

; =============================================================================
; Hardware probe and current-value read.
;
; The CARD is the authority for its own routing, not the CMOS copy: reading the
; mixer and the config register back means the screen always opens on what the
; hardware is really doing, even if something moved it since boot. Only the MPU
; port has no hardware register to read -- both MPU ports stay decoded either
; way, and the setting only decides which one BLASTER advertises -- so that one
; comes from CMOS.
; =============================================================================
read_hardware:
    call probe_sb
    call probe_wss
    cmp byte [sb_present], 0
    je .no_sb
    mov al, MIX_IRQ_SETUP
    call mixer_read
    mov bl, 7                   ; lowest set bit wins, matching the mixer model
    test al, 0x01
    jz .try_i5
    mov bl, 2
    jmp .irq_done
.try_i5:
    test al, 0x02
    jz .try_i7
    mov bl, 5
    jmp .irq_done
.try_i7:
    test al, 0x04
    jnz .irq_done
    test al, 0x08
    jz .irq_done
    mov bl, 10
.irq_done:
    mov [FVAL(F_SBIRQ)], bl
    mov al, MIX_DMA_SETUP
    call mixer_read
    mov ah, al
    mov bl, 1
    test al, 0x01
    jz .try_d1
    xor bl, bl
    jmp .dma8_done
.try_d1:
    test al, 0x02
    jnz .dma8_done
    test al, 0x08
    jz .dma8_done
    mov bl, 3
.dma8_done:
    mov [FVAL(F_SBDMA)], bl
    mov al, ah
    mov bl, 5
    test al, 0x40
    jz .try_h7
    mov bl, 6
    jmp .dma16_done
.try_h7:
    test al, 0x80
    jz .dma16_done
    mov bl, 7
.dma16_done:
    mov [FVAL(F_SBD16)], bl
.no_sb:
    cmp byte [wss_present], 0
    je .no_wss
    mov dx, WSS_CONFIG
    in al, dx
    mov ah, al
    shr al, 4
    mov bl, al
    mov si, opt_wssirq
    call in_options
    jc .wss_irq_ok
    mov bl, 11                  ; unreadable selection: fall back to the default
.wss_irq_ok:
    mov [FVAL(F_WSIRQ)], bl
    mov al, ah
    and al, 0x0F
    mov bl, al
    mov si, opt_dma8
    call in_options
    jc .wss_dma_ok
    xor bl, bl
.wss_dma_ok:
    mov [FVAL(F_WSDMA)], bl
.no_wss:
    mov al, CM_MAGIC
    call cmos_read
    cmp al, CM_MAGIC_VALUE
    jne .mpu_default
    mov al, CM_MPU_PORT
    call cmos_read
    and al, 1
    mov [FVAL(F_MPU)], al
.mpu_default:
    call build_blaster
    ret

; The documented Sound Blaster detection: pulse the reset port, then wait for
; the DSP to raise data-available and hand back 0xAA.
;
; The wait is bounded by the BIOS tick counter, not by an iteration count. The
; DSP needs ~100us of settle time to answer, and how many poll instructions fit
; into 100us depends entirely on the CPU persona -- a count tuned on the 200 MHz
; part reports "not installed" on the 22 MHz one, and vice versa. Three ticks
; (~165ms) is far longer than any real settle and still instant to a person. The
; iteration cap underneath it is a backstop for a machine whose timer interrupt
; is not running, where the tick would never advance.
probe_sb:
    mov dx, SB_RESET
    mov al, 1
    out dx, al
    mov cx, 200
.settle:
    in al, dx
    loop .settle
    xor al, al
    out dx, al
    push es
    xor ax, ax
    mov es, ax
    mov bx, [es:0x46C]          ; BIOS timer tick, low word
    xor cx, cx                  ; 65536 polls
.wait:
    mov dx, SB_READ_STATUS
    in al, dx
    test al, 0x80
    jnz .ready
    mov ax, [es:0x46C]
    sub ax, bx                  ; unsigned difference, so midnight wrap is fine
    cmp ax, 3
    jae .absent
    dec cx
    jnz .wait
    jmp .absent
.ready:
    mov dx, SB_READ_DATA
    in al, dx
    cmp al, 0xAA
    jne .absent
    mov byte [sb_present], 1
.absent:
    pop es
    ret

; The codec's board/version ID. An absent card leaves the port on open bus.
probe_wss:
    mov dx, WSS_ID
    in al, dx
    cmp al, 0xFF
    je .absent
    mov byte [wss_present], 1
.absent:
    ret

; Is BL in the option list at SI? CF=1 when yes. Preserves everything but AX.
in_options:
    push cx
    push si
    lodsb
    mov cl, al
    xor ch, ch
.loop:
    jcxz .no
    lodsb
    cmp al, bl
    je .yes
    dec cx
    jmp .loop
.yes:
    pop si
    pop cx
    stc
    ret
.no:
    pop si
    pop cx
    clc
    ret

; =============================================================================
; Command tail parsing (PSP:0x80 length, PSP:0x81 text, CR-terminated).
; Every token is /SWITCH or /SWITCH:VALUE. On a bad token CF=1 and `token`
; holds what was rejected, so the error names it.
; =============================================================================
parse_tail:
    mov cl, [0x80]
    xor ch, ch
    mov si, 0x81
.next:
    call skip_blanks
    jcxz .finish
    mov al, [si]
    cmp al, 13
    jne .token
.finish:
    clc
    ret
.token:
    cmp al, '/'
    je .switch
    cmp al, '-'                 ; DOS tools took either lead-in
    je .switch
    call copy_bad_token
    stc
    ret
.bad_value:
    mov byte [bad_kind], 1
    stc
    ret
.bad:
    stc
    ret
.switch:
    inc si
    dec cx
    call read_keyword
    jc .bad
    call find_switch            ; BX -> table entry
    jc .bad
    mov al, [bx + 2]
    mov [cur_kind], al
    mov al, [bx + 3]
    mov [cur_field], al
    ; Kinds 2-3 are the value switches and MUST stay contiguous here -- this
    ; is the range test that routes them; every other kind is a flag and
    ; falls through to the .flag chain below, which must give it an explicit
    ; arm (see the guard note at sw_table).
    cmp byte [cur_kind], 2
    jb .flag
    cmp byte [cur_kind], 3
    ja .flag
    ; A value switch needs a ':' or '=' and then digits.
    jcxz .bad
    mov al, [si]
    cmp al, ':'
    je .separator
    cmp al, '='
    jne .bad
.separator:
    inc si
    dec cx
    call read_number            ; AX = value
    jc .bad
    cmp byte [cur_kind], 3
    je .mpu
    cmp ax, 15
    ja .bad
    mov bl, al
    call set_field
    jc .bad_value
    mov byte [want_apply], 1
    jmp .next
.mpu:
    ; Ports are named the way they are printed: /MPU:300 or /MPU:330.
    cmp ax, 300
    je .mpu_low
    cmp ax, 330
    jne .bad_value
    mov byte [FVAL(F_MPU)], 1
    mov byte [want_apply], 1
    jmp .next
.mpu_low:
    mov byte [FVAL(F_MPU)], 0
    mov byte [want_apply], 1
    jmp .next
.flag:
    cmp byte [cur_kind], 0
    jne .flag_status
    mov byte [want_usage], 1
    jmp .next
.flag_status:
    cmp byte [cur_kind], 1
    jne .flag_boot
    mov byte [want_status], 1
    jmp .next
.flag_boot:
    cmp byte [cur_kind], 4
    jne .flag_tree
    mov byte [want_boot], 1
    jmp .next
.flag_tree:
    cmp byte [cur_kind], 5      ; guard, not an else: an unrouted future kind
    jne .bad                    ; must reject rather than silently become /T
    mov byte [tree_mode], 1
    jmp .next

; Copy the rest of the current token into `token` so the error can quote it.
copy_bad_token:
    push cx
    push si
    mov di, token
    xor bx, bx
.loop:
    jcxz .done
    mov al, [si]
    cmp al, ' '
    je .done
    cmp al, 9
    je .done
    cmp al, 13
    je .done
    cmp bx, TOKEN_MAX - 1
    jae .done
    mov [di], al
    inc di
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    mov byte [di], 0
    pop si
    pop cx
    ret

; Read the switch keyword into `token`, uppercased. CF=1 when empty or too long.
read_keyword:
    mov di, token
    xor bx, bx
.loop:
    jcxz .done
    mov al, [si]
    cmp al, ':'
    je .done
    cmp al, '='
    je .done
    cmp al, ' '
    je .done
    cmp al, 9
    je .done
    cmp al, 13
    je .done
    cmp bx, TOKEN_MAX - 1
    jae .bad
    cmp al, 'a'
    jb .store
    cmp al, 'z'
    ja .store
    sub al, 0x20
.store:
    mov [di], al
    inc di
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    mov byte [di], 0
    test bx, bx
    jz .bad
    clc
    ret
.bad:
    mov byte [di], 0
    stc
    ret

; Find `token` in the switch table. BX -> entry on success, CF=1 when unknown.
find_switch:
    push si
    mov bx, sw_table
.loop:
    mov ax, [bx]
    test ax, ax
    jz .no
    mov si, token
    mov di, ax
    call str_eq
    jc .yes
    add bx, 4
    jmp .loop
.yes:
    pop si
    clc
    ret
.no:
    pop si
    stc
    ret

; ASCIIZ compare, SI vs DI. CF=1 on match. Clobbers AX, SI, DI.
str_eq:
    mov al, [si]
    cmp al, [di]
    jne .no
    test al, al
    jz .yes
    inc si
    inc di
    jmp str_eq
.yes:
    stc
    ret
.no:
    clc
    ret

; Parse decimal digits at SI/CX into AX. CF=1 when there were none.
read_number:
    push bx
    xor ax, ax
    xor bx, bx
.loop:
    jcxz .done
    mov dl, [si]
    cmp dl, '0'
    jb .done
    cmp dl, '9'
    ja .done
    sub dl, '0'
    push dx
    mov dx, 10
    mul dx
    pop dx
    xor dh, dh
    add ax, dx
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    test bx, bx
    pop bx
    jz .none
    clc
    ret
.none:
    stc
    ret

; Store BL into the field named by `cur_field`, rejecting a value the hardware
; cannot select. CF=1 when rejected.
set_field:
    push si
    push di
    movzx si, byte [cur_field]
    shl si, 4
    add si, fields
    mov di, si
    mov si, [si + FLD_OPTS]
    call in_options
    jnc .bad
    mov [di + FLD_VALUE], bl
    pop di
    pop si
    clc
    ret
.bad:
    pop di
    pop si
    stc
    ret

; SI -> field record. Print its permitted values, comma separated, formatted
; the way that field displays them (so the MPU port lists 300, 330 and not 0, 1).
print_options:
    mov al, [si + FLD_FMT]
    mov [list_fmt], al
    mov si, [si + FLD_OPTS]
    lodsb
    mov cl, al
    xor ch, ch
    xor bx, bx
.loop:
    jcxz .done
    test bx, bx
    jz .value
    push cx
    push si
    mov si, msg_comma
    call print
    pop si
    pop cx
.value:
    lodsb
    push cx
    mov ah, [list_fmt]
    call fmt_val
    push si
    mov si, numbuf
    call print
    pop si
    pop cx
    inc bx
    dec cx
    jmp .loop
.done:
    ret

; Advance SI/CX past spaces and tabs.
skip_blanks:
    jcxz .done
    mov al, [si]
    cmp al, ' '
    je .advance
    cmp al, 9
    jne .done
.advance:
    inc si
    dec cx
    jmp skip_blanks
.done:
    ret

; The two ways the same line can be invalid: a device sharing the other's IRQ or
; DMA channel. The menus make this unreachable interactively, so it only ever
; fires on a command line. CF=1 when the assignment collides.
check_collisions:
    cmp byte [sb_present], 0
    je .ok
    cmp byte [wss_present], 0
    je .ok
    mov al, [FVAL(F_SBIRQ)]
    cmp al, [FVAL(F_WSIRQ)]
    je .clash
    mov al, [FVAL(F_SBDMA)]
    cmp al, [FVAL(F_WSDMA)]
    je .clash
.ok:
    clc
    ret
.clash:
    stc
    ret

; =============================================================================
; Full-screen interface.
; =============================================================================
interactive:
    call video_init
    call draw_screen
    call first_field
    call draw_fields
.loop:
    call getkey
    cmp al, 27
    je .cancel
    cmp ah, 0x44                ; F10
    je .save
    cmp al, 13
    je .edit
    cmp al, ' '
    je .edit
    cmp al, 9                   ; Tab
    je .forward
    cmp ah, 0x0F                ; Shift+Tab
    je .backward
    cmp ah, 0x48                ; Up
    je .backward
    cmp ah, 0x4B                ; Left
    je .backward
    cmp ah, 0x50                ; Down
    je .forward
    cmp ah, 0x4D                ; Right
    je .forward
    jmp .loop
.backward:
    call prev_field
    call draw_fields
    jmp .loop
.forward:
    call next_field
    call draw_fields
    jmp .loop
.edit:
    call open_menu
    call draw_fields
    call draw_blaster
    jmp .loop
.save:
    call apply_all
    call video_done
    call report
    mov ax, 0x4c00
    int 0x21
.cancel:
    call video_done
    mov si, msg_cancelled
    call print
    mov ax, 0x4c00
    int 0x21

; 80x25 colour text, blink off so bright backgrounds are solid rather than
; blinking foregrounds, and no cursor. ES addresses the text buffer from here
; until video_done.
video_init:
    mov ax, 0x0003
    int 0x10
    mov ax, 0x1003
    xor bx, bx
    int 0x10
    mov ah, 0x01
    mov cx, 0x2000
    int 0x10
    mov ax, 0xB800
    mov es, ax
    ret

; Hand the screen back the way DOS expects it: default mode, blink restored.
video_done:
    mov ax, 0x0003
    int 0x10
    mov ax, 0x1003
    mov bx, 1
    int 0x10
    push ds
    pop es
    ret

getkey:
    mov ah, 0
    int 0x16
    ret

draw_screen:
    call cls
    call draw_shadow
    call draw_box
    call draw_static
    call draw_rule
    call draw_blaster
    ret

cls:
    xor di, di
    mov ax, 0x0020
    mov cx, 80 * 25
    rep stosw
    ret

; AL = row, AH = column -> DI = text-buffer offset. Preserves everything else.
screen_at:
    push ax
    push bx
    push dx
    mov bl, ah
    xor bh, bh
    xor ah, ah
    mov dx, 160
    mul dx
    shl bx, 1
    add ax, bx
    mov di, ax
    pop dx
    pop bx
    pop ax
    ret

; SI -> ASCIIZ, DI = offset, AH = attribute. DI ends past the text.
puts:
    lodsb
    test al, al
    jz .done
    stosw
    jmp puts
.done:
    ret

draw_box:
    mov al, BOX_ROW
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xDA
    stosw
    mov al, 0xC4
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xBF
    stosw
    mov bl, BOX_ROW + 1
.row:
    mov al, bl
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    mov al, ' '
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xB3
    stosw
    inc bl
    cmp bl, BOX_ROW + BOX_H - 1
    jb .row
    mov al, BOX_ROW + BOX_H - 1
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xC0
    stosw
    mov al, 0xC4
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xD9
    stosw
    ret

; Offset one row down and two columns right, the way a DOS dialog casts one.
draw_shadow:
    mov al, BOX_ROW + BOX_H
    mov ah, BOX_COL + 2
    call screen_at
    mov ax, (A_SHADOW << 8) | ' '
    mov cx, BOX_W
    rep stosw
    mov bl, BOX_ROW + 1
.row:
    mov al, bl
    mov ah, BOX_COL + BOX_W
    call screen_at
    mov ax, (A_SHADOW << 8) | ' '
    mov cx, 2
    rep stosw
    inc bl
    cmp bl, BOX_ROW + BOX_H
    jb .row
    ret

draw_static:
    mov si, static_text
.loop:
    mov al, [si]
    cmp al, 0xFF
    je .done
    mov ah, [si + 1]
    call screen_at
    mov ah, [si + 2]
    push si
    mov si, [si + 3]
    call puts
    pop si
    add si, 5
    jmp .loop
.done:
    ret

draw_rule:
    mov al, 6
    mov ah, 6
    call screen_at
    mov ax, (A_BOX << 8) | 0xC4
    mov cx, 57
    rep stosw
    ret

; The live BLASTER preview, re-rendered whenever a value changes so the string
; on screen is the string that will be written.
draw_blaster:
    mov al, 13
    mov ah, 6
    call screen_at
    mov ax, (A_BOX << 8) | ' '
    mov cx, 57
    rep stosw
    mov al, 13
    mov ah, 6
    call screen_at
    mov ah, A_BOX
    cmp byte [sb_present], 0
    je .absent
    mov si, s_blaster_eq
    call puts
    mov si, blaster_val
    call puts
    ret
.absent:
    mov si, s_no_sb
    call puts
    ret

draw_fields:
    xor bl, bl
.loop:
    cmp bl, F_COUNT
    jae .done
    mov al, A_FIELD
    cmp bl, [cur_sel]
    jne .draw
    mov al, A_SEL
.draw:
    push bx
    call draw_field
    pop bx
    inc bl
    jmp .loop
.done:
    ret

; BL = field index, AL = attribute for the input cells.
draw_field:
    mov [tmp_attr], al
    movzx si, bl
    shl si, 4
    add si, fields
    call field_enabled
    jnc .absent
    mov al, [si + FLD_ROW]
    mov ah, [si + FLD_COL]
    call screen_at
    mov ah, [tmp_attr]
    mov al, ' '
    movzx cx, byte [si + FLD_WIDTH]
    push cx
    rep stosw
    mov al, [si + FLD_VALUE]
    mov ah, [si + FLD_FMT]
    call fmt_val                ; numbuf, CX = length
    pop bx
    sub bx, cx
    shr bx, 1
    mov al, [si + FLD_ROW]
    mov ah, [si + FLD_COL]
    add ah, bl
    call screen_at
    mov ah, [tmp_attr]
    push si
    mov si, numbuf
    call puts
    pop si
    ret
.absent:
    ; A device that is not installed carries the same asterisk the inapplicable
    ; cells do, in the box attribute: nothing to edit, so nothing that looks
    ; like an input.
    mov al, [si + FLD_ROW]
    mov ah, [si + FLD_COL]
    call screen_at
    mov ah, A_BOX
    mov al, ' '
    movzx cx, byte [si + FLD_WIDTH]
    push cx
    rep stosw
    pop cx
    dec cx
    shr cx, 1                   ; same centring the static asterisks use
    mov al, [si + FLD_ROW]
    mov ah, [si + FLD_COL]
    add ah, cl
    call screen_at
    mov ah, A_BOX
    mov al, '*'
    stosw
    ret

; SI -> field record. CF=1 when the field's device is installed.
field_enabled:
    mov al, [si + FLD_DEV]
    cmp al, DEV_ANY
    je .yes
    cmp al, DEV_SB
    jne .wss
    cmp byte [sb_present], 0
    je .no
    jmp .yes
.wss:
    cmp byte [wss_present], 0
    je .no
.yes:
    stc
    ret
.no:
    clc
    ret

; BL = field index. CF=1 when that field is editable. Preserves SI.
index_enabled:
    push si
    movzx si, bl
    shl si, 4
    add si, fields
    call field_enabled
    pop si
    ret

first_field:
    mov byte [cur_sel], F_COUNT - 1
    call next_field
    ret

next_field:
    mov bl, [cur_sel]
    mov cx, F_COUNT
.loop:
    inc bl
    cmp bl, F_COUNT
    jb .check
    xor bl, bl
.check:
    call index_enabled
    jc .found
    loop .loop
    ret
.found:
    mov [cur_sel], bl
    ret

prev_field:
    mov bl, [cur_sel]
    mov cx, F_COUNT
.loop:
    test bl, bl
    jnz .step
    mov bl, F_COUNT
.step:
    dec bl
    call index_enabled
    jc .found
    loop .loop
    ret
.found:
    mov [cur_sel], bl
    ret

; AL = value, AH = format -> numbuf ASCIIZ, CX = length.
fmt_val:
    push di
    mov di, numbuf
    cmp ah, FMT_MPU
    je .mpu
    call u8dec
    jmp .terminate
.mpu:
    push si
    mov si, s_mpu300
    test al, al
    jz .copy
    mov si, s_mpu330
.copy:
    call copy_str
    pop si
.terminate:
    mov byte [di], 0
    pop di
    ret

; AL (0..19) as decimal at [DI]. DI advances; CX = digits written.
u8dec:
    mov cx, 1
    cmp al, 10
    jb .ones
    mov byte [di], '1'
    inc di
    sub al, 10
    mov cx, 2
.ones:
    add al, '0'
    mov [di], al
    inc di
    ret

; Copy the ASCIIZ at SI to [DI] without its terminator. CX = bytes copied.
copy_str:
    xor cx, cx
.loop:
    mov al, [si]
    test al, al
    jz .done
    mov [di], al
    inc si
    inc di
    inc cx
    jmp .loop
.done:
    ret

; =============================================================================
; The value menu.
;
; The list offered is the hardware's own option list minus whatever the peer
; device currently holds, so an assignment that would collide is not merely
; rejected -- it is never presented. An absent peer cannot collide, so its value
; is not withheld.
; =============================================================================
open_menu:
    movzx si, byte [cur_sel]
    shl si, 4
    add si, fields
    mov al, [si + FLD_FMT]
    mov [menu_fmt], al
    mov byte [peer_value], 0xFF
    mov bl, [si + FLD_PEER]
    cmp bl, 0xFF
    je .build
    call index_enabled
    jnc .build
    movzx di, bl
    shl di, 4
    mov al, [fields + di + FLD_VALUE]
    mov [peer_value], al
.build:
    push si
    mov si, [si + FLD_OPTS]
    mov di, menu_vals
    xor bx, bx
    lodsb
    mov cl, al
    xor ch, ch
.filter:
    jcxz .filtered
    lodsb
    cmp al, [peer_value]
    je .skip
    mov [di], al
    inc di
    inc bx
.skip:
    dec cx
    jmp .filter
.filtered:
    pop si
    mov [menu_count], bl
    test bl, bl
    jz .nothing
    ; Start on the value the field already holds.
    xor bl, bl
    mov al, [si + FLD_VALUE]
.find:
    cmp bl, [menu_count]
    jae .default
    movzx di, bl
    cmp al, [menu_vals + di]
    je .found
    inc bl
    jmp .find
.default:
    xor bl, bl
.found:
    mov [menu_sel], bl
    mov al, [si + FLD_ROW]
    inc al
    mov [menu_row], al
    mov al, [si + FLD_COL]
    dec al
    mov [menu_col], al
    mov al, [si + FLD_WIDTH]
    add al, 2
    mov [menu_w], al
    mov al, [menu_count]
    add al, 2
    mov [menu_h], al
    call menu_save
    call menu_draw
.keys:
    call getkey
    cmp al, 27
    je .cancel
    cmp al, 13
    je .accept
    cmp ah, 0x48
    je .up
    cmp ah, 0x50
    je .down
    jmp .keys
.up:
    mov bl, [menu_sel]
    test bl, bl
    jnz .up_step
    mov bl, [menu_count]
.up_step:
    dec bl
    mov [menu_sel], bl
    call menu_draw
    jmp .keys
.down:
    mov bl, [menu_sel]
    inc bl
    cmp bl, [menu_count]
    jb .down_step
    xor bl, bl
.down_step:
    mov [menu_sel], bl
    call menu_draw
    jmp .keys
.accept:
    call menu_restore
    movzx di, byte [menu_sel]
    mov al, [menu_vals + di]
    movzx si, byte [cur_sel]
    shl si, 4
    mov [fields + si + FLD_VALUE], al
    call build_blaster
    ret
.cancel:
    call menu_restore
    ret
.nothing:
    ret

menu_save:
    push si
    mov si, menu_bak
    mov bl, [menu_row]
.row:
    mov al, bl
    mov ah, [menu_col]
    call screen_at
    movzx cx, byte [menu_w]
.cell:
    mov ax, [es:di]
    mov [si], ax
    add si, 2
    add di, 2
    loop .cell
    inc bl
    mov al, bl
    sub al, [menu_row]
    cmp al, [menu_h]
    jb .row
    pop si
    ret

menu_restore:
    push si
    mov si, menu_bak
    mov bl, [menu_row]
.row:
    mov al, bl
    mov ah, [menu_col]
    call screen_at
    movzx cx, byte [menu_w]
.cell:
    mov ax, [si]
    mov [es:di], ax
    add si, 2
    add di, 2
    loop .cell
    inc bl
    mov al, bl
    sub al, [menu_row]
    cmp al, [menu_h]
    jb .row
    pop si
    ret

menu_draw:
    mov al, [menu_row]
    mov ah, [menu_col]
    call screen_at
    mov ah, A_BOX
    mov al, 0xDA
    stosw
    mov al, 0xC4
    movzx cx, byte [menu_w]
    sub cx, 2
    rep stosw
    mov al, 0xBF
    stosw
    xor bl, bl
.item:
    cmp bl, [menu_count]
    jae .bottom
    mov al, [menu_row]
    add al, bl
    inc al
    mov ah, [menu_col]
    call screen_at
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    mov ah, A_BOX
    cmp bl, [menu_sel]
    jne .fill
    mov ah, A_SEL
.fill:
    mov [tmp_attr], ah
    mov al, ' '
    movzx cx, byte [menu_w]
    sub cx, 2
    rep stosw
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    movzx di, bl
    mov al, [menu_vals + di]
    mov ah, [menu_fmt]
    call fmt_val                ; numbuf, CX = length
    movzx ax, byte [menu_w]
    sub ax, 2
    sub ax, cx
    shr ax, 1
    mov [tmp_pad], al
    mov al, [menu_row]
    add al, bl
    inc al
    mov ah, [menu_col]
    inc ah
    add ah, [tmp_pad]
    call screen_at
    mov ah, [tmp_attr]
    push si
    mov si, numbuf
    call puts
    pop si
    inc bl
    jmp .item
.bottom:
    mov al, [menu_row]
    add al, [menu_count]
    inc al
    mov ah, [menu_col]
    call screen_at
    mov ah, A_BOX
    mov al, 0xC0
    stosw
    mov al, 0xC4
    movzx cx, byte [menu_w]
    sub cx, 2
    rep stosw
    mov al, 0xD9
    stosw
    ret

; =============================================================================
; Apply.
; =============================================================================
apply_all:
    mov byte [applied], 1
    call build_blaster
    call write_cmos
    call apply_hw
    call patch_env
    call patch_autoexec
    ret

; The string every consumer of this tool's output derives from.
build_blaster:
    mov di, blaster_val
    mov si, s_a220
    call copy_str
    mov al, [FVAL(F_SBIRQ)]
    call u8dec
    mov si, s_sp_d
    call copy_str
    mov al, [FVAL(F_SBDMA)]
    call u8dec
    mov si, s_sp_h
    call copy_str
    mov al, [FVAL(F_SBD16)]
    call u8dec
    mov si, s_sp_p
    call copy_str
    mov si, s_mpu300
    cmp byte [FVAL(F_MPU)], 0
    je .port
    mov si, s_mpu330
.port:
    call copy_str
    mov si, s_t6
    call copy_str
    mov byte [di], 0
    ret

; Persist the block and refresh the NVRAM checksum. Skipping the checksum would
; make the next POST throw the whole of CMOS away, so the two are one operation.
write_cmos:
    pushf
    cli
    mov ah, CM_MAGIC
    mov al, CM_MAGIC_VALUE
    call cmos_write
    mov ah, CM_SB_IRQ
    mov al, [FVAL(F_SBIRQ)]
    call cmos_write
    mov ah, CM_SB_DMA8
    mov al, [FVAL(F_SBDMA)]
    call cmos_write
    mov ah, CM_SB_DMA16
    mov al, [FVAL(F_SBD16)]
    call cmos_write
    mov ah, CM_WSS_IRQ
    mov al, [FVAL(F_WSIRQ)]
    call cmos_write
    mov ah, CM_WSS_DMA
    mov al, [FVAL(F_WSDMA)]
    call cmos_write
    mov ah, CM_MPU_PORT
    mov al, [FVAL(F_MPU)]
    call cmos_write
    xor bx, bx
    mov cl, CM_SUM_FIRST
.sum:
    mov al, cl
    call cmos_read
    xor ah, ah
    add bx, ax
    inc cl
    cmp cl, CM_SUM_LAST + 1
    jb .sum
    mov ah, CM_SUM_HI
    mov al, bh
    call cmos_write
    mov ah, CM_SUM_LO
    mov al, bl
    call cmos_write
    mov al, 0x0D                ; leave the index somewhere harmless, NMI on
    out 0x70, al
    popf
    ret

; AL = index -> AL = value. NMI stays masked for the access, as every BIOS does.
cmos_read:
    or al, 0x80
    out 0x70, al
    jmp short $+2
    in al, 0x71
    ret

; AH = index, AL = value.
cmos_write:
    push ax
    mov al, ah
    or al, 0x80
    out 0x70, al
    jmp short $+2
    pop ax
    out 0x71, al
    ret

mixer_read:
    push dx
    mov dx, SB_MIXER_IDX
    out dx, al
    jmp short $+2
    inc dx
    in al, dx
    pop dx
    ret

; AH = register, AL = value.
mixer_write:
    push dx
    push ax
    mov dx, SB_MIXER_IDX
    mov al, ah
    out dx, al
    jmp short $+2
    pop ax
    inc dx
    out dx, al
    pop dx
    ret

; Move the live hardware. Only devices that answered the probe are written; a
; machine built without one of them has nothing listening on those ports.
apply_hw:
    cmp byte [sb_present], 0
    je .wss
    mov al, [FVAL(F_SBIRQ)]
    mov bl, 0x04                ; D2 = IRQ7
    cmp al, 2
    jne .not2
    mov bl, 0x01
    jmp .irq_set
.not2:
    cmp al, 5
    jne .not5
    mov bl, 0x02
    jmp .irq_set
.not5:
    cmp al, 10
    jne .irq_set
    mov bl, 0x08
.irq_set:
    mov ah, MIX_IRQ_SETUP
    mov al, bl
    call mixer_write
    mov al, [FVAL(F_SBDMA)]
    mov bl, 0x02                ; D1 = DMA1
    test al, al
    jnz .not_d0
    mov bl, 0x01
    jmp .dma8_set
.not_d0:
    cmp al, 3
    jne .dma8_set
    mov bl, 0x08
.dma8_set:
    mov al, [FVAL(F_SBD16)]
    mov bh, 0x20                ; D5 = DMA5
    cmp al, 6
    jne .not_d6
    mov bh, 0x40
    jmp .dma16_set
.not_d6:
    cmp al, 7
    jne .dma16_set
    mov bh, 0x80
.dma16_set:
    mov al, bl
    or al, bh
    mov ah, MIX_DMA_SETUP
    call mixer_write
.wss:
    cmp byte [wss_present], 0
    je .done
    mov al, [FVAL(F_WSIRQ)]
    shl al, 4
    or al, [FVAL(F_WSDMA)]
    mov dx, WSS_CONFIG
    out dx, al
.done:
    ret

; =============================================================================
; Master environment.
;
; The shell's own environment block is what child programs inherit a copy of, so
; patching it there is what makes a game started from this prompt see the new
; routing. Walk the PSP parent chain to the root shell (whose parent is itself)
; and take its environment segment.
; =============================================================================
patch_env:
    mov byte [env_blaster], 0
    mov byte [env_setsound], 0
    call find_master_env
    jc .done
    mov si, s_blaster
    call env_update
    mov [env_blaster], al
    mov si, s_setsound
    call env_update
    mov [env_setsound], al
.done:
    push ds
    pop es
    ret

find_master_env:
    mov ah, 0x62
    int 0x21                    ; BX = this program's PSP
    mov cx, 32                  ; a corrupt chain must not spin forever
.up:
    mov es, bx
    mov ax, [es:0x16]
    test ax, ax
    jz .here
    cmp ax, bx
    je .here
    mov bx, ax
    loop .up
.here:
    mov es, bx
    mov ax, [es:0x2C]
    test ax, ax
    jz .none
    mov [env_seg], ax
    dec ax
    mov es, ax
    mov al, [es:0]
    cmp al, 0x4D                ; 'M', a link in the MCB chain
    je .mcb_ok
    cmp al, 0x5A                ; 'Z', the last block
    jne .none
.mcb_ok:
    mov cx, [es:3]              ; block size in paragraphs
    cmp cx, 0x0FFF
    jbe .size_ok
    mov cx, 0x0FFF
.size_ok:
    mov ax, cx
    shl ax, 4
    mov [env_bytes], ax
    mov es, [env_seg]
    clc
    ret
.none:
    stc
    ret

; SI -> variable name. Rewrites its value to blaster_val in the block ES points
; at. AL: 0 = not present (left alone), 1 = updated, 2 = block too full.
env_update:
    push si
    call env_find               ; BX = entry offset
    jc .absent
    mov [env_at], bx
    ; Old entry length, including its terminator.
    mov di, bx
    call env_strlen
    inc cx
    mov [env_oldlen], cx
    ; New entry length: name, separator, value, terminator.
    pop si
    push si
    call str_len
    mov ax, cx
    mov si, blaster_val
    call str_len
    add ax, cx
    add ax, 2
    mov [env_newlen], ax
    call env_extent             ; DX = bytes in use
    mov [env_used], dx
    mov ax, [env_newlen]
    sub ax, [env_oldlen]        ; signed delta
    mov [env_delta], ax
    test ax, ax
    jle .room_ok
    add ax, [env_used]
    cmp ax, [env_bytes]
    ja .full
.room_ok:
    call env_shift
    pop si
    push si
    call env_store
    pop si
    mov al, 1
    ret
.full:
    pop si
    mov al, 2
    ret
.absent:
    pop si
    xor al, al
    ret

; Slide everything after the entry by env_delta so the new value fits exactly.
env_shift:
    mov ax, [env_delta]
    test ax, ax
    jz .done
    mov si, [env_at]
    add si, [env_oldlen]
    mov cx, [env_used]
    sub cx, si                  ; bytes to move
    jbe .done
    mov di, [env_at]
    add di, [env_newlen]
    xor bp, bp
    test ax, ax
    jl .direction
    inc bp                      ; growing: copy from the top down
.direction:
    push ds
    mov ax, es
    mov ds, ax
    test bp, bp
    jz .forward
    add si, cx
    dec si
    add di, cx
    dec di
    std
    rep movsb
    cld
    pop ds
    ret
.forward:
    rep movsb
    pop ds
.done:
    ret

; Write the name, separator, value and terminator at env_at. SI -> name.
env_store:
    mov di, [env_at]
.name:
    mov al, [si]
    test al, al
    jz .equals
    mov [es:di], al
    inc si
    inc di
    jmp .name
.equals:
    mov byte [es:di], 0x3D      ; '='
    inc di
    mov si, blaster_val
.value:
    mov al, [si]
    test al, al
    jz .terminate
    mov [es:di], al
    inc si
    inc di
    jmp .value
.terminate:
    mov byte [es:di], 0
    ret

; Find the entry SI names. BX = its offset, CF=1 when absent.
env_find:
    push si
    xor bx, bx
.entry:
    cmp bx, [env_bytes]
    jae .no
    cmp byte [es:bx], 0
    je .no                      ; the terminating empty string: end of the list
    pop si
    push si
    mov di, bx
.compare:
    mov al, [si]
    test al, al
    jz .at_equals
    cmp al, [es:di]
    jne .next
    inc si
    inc di
    jmp .compare
.at_equals:
    cmp byte [es:di], 0x3D      ; '='
    je .yes
.next:
    mov di, bx
.skip:
    cmp di, [env_bytes]
    jae .no
    cmp byte [es:di], 0
    je .skipped
    inc di
    jmp .skip
.skipped:
    lea bx, [di + 1]
    jmp .entry
.yes:
    pop si
    clc
    ret
.no:
    pop si
    stc
    ret

; Length of the ASCIIZ at ES:DI, not counting the terminator, in CX.
env_strlen:
    push di
    xor cx, cx
.loop:
    cmp byte [es:di], 0
    je .done
    inc di
    inc cx
    jmp .loop
.done:
    pop di
    ret

; Bytes of the block actually in use: the variable list, its terminating empty
; string, and the DOS 3+ count word plus program path when they are present.
env_extent:
    push bx
    xor bx, bx
.entry:
    cmp bx, [env_bytes]
    jae .capped
    cmp byte [es:bx], 0
    je .terminator
.skip:
    inc bx
    cmp bx, [env_bytes]
    jae .capped
    cmp byte [es:bx], 0
    jne .skip
    inc bx
    jmp .entry
.terminator:
    mov dx, bx
    inc dx
    mov ax, bx
    add ax, 4
    cmp ax, [env_bytes]
    jae .done
    mov ax, [es:bx + 1]
    cmp ax, 1
    jne .done
    mov di, bx
    add di, 3
.path:
    cmp di, [env_bytes]
    jae .done
    cmp byte [es:di], 0
    je .path_end
    inc di
    jmp .path
.path_end:
    mov dx, di
    inc dx
.done:
    pop bx
    ret
.capped:
    mov dx, [env_bytes]
    pop bx
    ret

; Length of the ASCIIZ at DS:SI in CX. SI is preserved.
str_len:
    push si
    xor cx, cx
.loop:
    cmp byte [si], 0
    je .done
    inc si
    inc cx
    jmp .loop
.done:
    pop si
    ret

; =============================================================================
; AUTOEXEC.BAT.
;
; Only the SET lines that already exist are rewritten, and only their value: the
; rest of the file is copied through byte for byte, including its line endings,
; so a file the user has edited keeps its shape. A file with no SET BLASTER line
; is not given one.
; =============================================================================
patch_autoexec:
    mov byte [ax_result], AX_NONE
    mov ax, 0x3D00
    mov dx, s_autoexec
    int 0x21
    jc .missing
    mov bx, ax
    mov ah, 0x3F
    mov cx, AUTOEXEC_MAX
    mov dx, ax_in
    int 0x21
    jc .read_failed
    mov [ax_len], ax
    mov ah, 0x3E
    int 0x21
    mov ax, [ax_len]
    cmp ax, AUTOEXEC_MAX
    jae .too_big                ; a full buffer means the read was truncated
    call rewrite_lines
    test al, al
    jz .done                    ; no SET line to change: leave the file alone
    mov ah, 0x3C
    xor cx, cx
    mov dx, s_autoexec
    int 0x21
    jc .error
    mov bx, ax
    mov ah, 0x40
    mov cx, [ax_outlen]
    mov dx, ax_out
    int 0x21
    pushf
    push ax
    mov ah, 0x3E
    int 0x21
    pop ax
    popf
    jc .error
    cmp ax, [ax_outlen]
    jne .error
    mov byte [ax_result], AX_WRITTEN
.done:
    ret
.read_failed:
    mov ah, 0x3E
    int 0x21
.error:
    mov byte [ax_result], AX_ERROR
    ret
.too_big:
    mov byte [ax_result], AX_TOO_BIG
    ret
.missing:
    mov byte [ax_result], AX_MISSING
    ret

; Copy ax_in to ax_out, substituting the value on any SET BLASTER / SET SETSOUND
; line. Returns AL = lines changed and sets ax_outlen.
rewrite_lines:
    mov word [lines_changed], 0
    mov si, ax_in
    mov di, ax_out
    mov ax, [ax_len]
    add ax, ax_in
    mov [in_end], ax
.line:
    cmp si, [in_end]
    jae .done
    mov [ln_start], si
.scan:
    cmp si, [in_end]
    jae .body_end
    mov al, [si]
    cmp al, 13
    je .body_end
    cmp al, 10
    je .body_end
    inc si
    jmp .scan
.body_end:
    mov [ln_end], si
    cmp si, [in_end]
    jae .eol_done
    cmp byte [si], 13
    jne .maybe_lf
    inc si
    cmp si, [in_end]
    jae .eol_done
.maybe_lf:
    cmp byte [si], 10
    jne .eol_done
    inc si
.eol_done:
    mov [ln_next], si
    mov si, [ln_start]
    mov dx, s_set_blaster
    call ci_prefix
    jc .replace
    mov si, [ln_start]
    mov dx, s_set_setsound
    call ci_prefix
    jc .replace
    mov si, [ln_start]
    mov cx, [ln_next]
    sub cx, si
    call emit
    jmp .advance
.replace:
    mov si, dx
    call str_len
    mov si, dx
    call emit
    mov si, blaster_val
    call str_len
    mov si, blaster_val
    call emit
    mov si, [ln_end]
    mov cx, [ln_next]
    sub cx, si
    call emit
    inc word [lines_changed]
.advance:
    mov si, [ln_next]
    jmp .line
.done:
    mov ax, di
    sub ax, ax_out
    mov [ax_outlen], ax
    mov ax, [lines_changed]
    ret

; Copy CX bytes from SI to DI, clamped to the output buffer.
emit:
    jcxz .done
.loop:
    cmp di, ax_out + AUTOEXEC_OUT
    jae .done
    mov al, [si]
    mov [di], al
    inc si
    inc di
    dec cx
    jnz .loop
.done:
    ret

; Does the text at SI start with the uppercase ASCIIZ pattern at DX? CF=1 if so.
ci_prefix:
    push si
    push di
    mov di, dx
.loop:
    mov al, [di]
    test al, al
    jz .yes
    cmp si, [in_end]
    jae .no
    mov ah, [si]
    cmp ah, 0x61                ; 'a'
    jb .compare
    cmp ah, 0x7A                ; 'z'
    ja .compare
    sub ah, 0x20
.compare:
    cmp al, ah
    jne .no
    inc si
    inc di
    jmp .loop
.yes:
    pop di
    pop si
    stc
    ret
.no:
    pop di
    pop si
    clc
    ret

; =============================================================================
; Reporting.
; =============================================================================
print:
    push ax
    push dx
    push si
.loop:
    mov dl, [si]
    test dl, dl
    jz .done
    mov ah, 0x02
    int 0x21
    inc si
    jmp .loop
.done:
    pop si
    pop dx
    pop ax
    ret

; Print `linebuf` as built so far, then reset it.
flush_line:
    mov byte [di], 0
    push si
    mov si, linebuf
    call print
    pop si
    ret

; boot_report (below) renders these same fields in the two-row /B form; a new
; field belongs in both.
report:
    mov si, msg_head
    call print
    cmp byte [sb_present], 0
    je .no_sb
    mov di, linebuf
    mov si, s_rep_sb
    call copy_str
    mov al, [FVAL(F_SBIRQ)]
    call u8dec
    mov si, s_rep_dmal
    call copy_str
    mov al, [FVAL(F_SBDMA)]
    call u8dec
    mov si, s_rep_dmah
    call copy_str
    mov al, [FVAL(F_SBD16)]
    call u8dec
    mov si, s_crlf
    call copy_str
    call flush_line
    jmp .wss
.no_sb:
    mov si, msg_sb_absent
    call print
.wss:
    cmp byte [wss_present], 0
    je .no_wss
    mov di, linebuf
    mov si, s_rep_wss
    call copy_str
    mov al, [FVAL(F_WSIRQ)]
    call u8dec
    mov si, s_rep_dma
    call copy_str
    mov al, [FVAL(F_WSDMA)]
    call u8dec
    mov si, s_crlf
    call copy_str
    call flush_line
    jmp .mpu
.no_wss:
    mov si, msg_wss_absent
    call print
.mpu:
    mov di, linebuf
    mov si, s_rep_mpu
    call copy_str
    mov si, s_mpu300
    cmp byte [FVAL(F_MPU)], 0
    je .mpu_port
    mov si, s_mpu330
.mpu_port:
    call copy_str
    mov si, s_rep_mpu_tail
    call copy_str
    call flush_line
    mov si, msg_opl
    call print
    cmp byte [sb_present], 0
    je .after_blaster
    mov di, linebuf
    mov si, s_rep_blaster
    call copy_str
    mov si, blaster_val
    call copy_str
    mov si, s_crlf
    call copy_str
    call flush_line
.after_blaster:
    cmp byte [applied], 0
    je .done
    mov si, msg_saved
    call print
    mov al, [env_blaster]
    cmp al, 1
    jne .env_note
    mov si, msg_env_ok
    call print
    jmp .autoexec
.env_note:
    cmp al, 2
    jne .env_absent
    mov si, msg_env_full
    call print
    jmp .autoexec
.env_absent:
    mov si, msg_env_absent
    call print
.autoexec:
    mov al, [ax_result]
    cmp al, AX_WRITTEN
    jne .ax_none
    mov si, msg_ax_ok
    call print
    jmp .done
.ax_none:
    cmp al, AX_NONE
    jne .ax_missing
    mov si, msg_ax_none
    call print
    jmp .done
.ax_missing:
    cmp al, AX_MISSING
    jne .ax_error
    mov si, msg_ax_missing
    call print
    jmp .done
.ax_error:
    mov si, msg_ax_error
    call print
.done:
    ret

; =============================================================================
; /B boot summary: exactly two rows, no more (hard 25-row screen budget).
; Row 1 is the heading, printed straight from its own CRLF-terminated string.
; Row 2 is built in `linebuf` the same way report() builds its rows, reusing
; the same probed fields report() reads -- read_hardware has already run by
; the time start: gets here, same as it has for /S. Nothing here writes to
; the hardware, CMOS, the environment, or AUTOEXEC.BAT.
; =============================================================================
boot_report:
    cmp byte [tree_mode], 0
    je .no_tree_head
    mov si, msg_boot_tree
    call print
.no_tree_head:
    mov si, msg_boot_head
    call print
    mov di, linebuf
    cmp byte [tree_mode], 0
    je .no_gutter
    mov si, msg_boot_gut
    call copy_str
.no_gutter:
    mov si, msg_boot_ind
    call copy_str
    ; ---- SB16 ----
    mov si, msg_boot_sb
    call copy_str
    cmp byte [sb_present], 0
    je .sb_absent
    mov si, t_220
    call copy_str
    mov si, msg_boot_irq
    call copy_str
    mov al, [FVAL(F_SBIRQ)]
    call u8dec
    mov si, msg_boot_dmal
    call copy_str
    mov al, [FVAL(F_SBDMA)]
    call u8dec
    mov si, msg_boot_dmah
    call copy_str
    mov al, [FVAL(F_SBD16)]
    call u8dec
    jmp .sb_done
.sb_absent:
    mov si, msg_boot_absent
    call copy_str
.sb_done:
    mov si, msg_boot_gap
    call copy_str
    ; ---- WSS ----
    mov si, msg_boot_wss
    call copy_str
    cmp byte [wss_present], 0
    je .wss_absent
    mov si, t_530
    call copy_str
    mov si, msg_boot_irq
    call copy_str
    mov al, [FVAL(F_WSIRQ)]
    call u8dec
    mov si, msg_boot_dmal
    call copy_str
    mov al, [FVAL(F_WSDMA)]
    call u8dec
    jmp .wss_done
.wss_absent:
    mov si, msg_boot_absent
    call copy_str
.wss_done:
    mov si, msg_boot_gap
    call copy_str
    ; ---- MIDI ----
    ; The MPU IRQ is fixed at 9, same as s_rep_mpu_tail in report() -- there is
    ; no field for it, so it is a literal here too, not a u8dec of anything.
    mov si, msg_boot_midi
    call copy_str
    mov si, s_mpu300
    cmp byte [FVAL(F_MPU)], 0
    je .mpu_port
    mov si, s_mpu330
.mpu_port:
    call copy_str
    mov si, msg_boot_midi_irq
    call copy_str
    mov si, s_crlf              ; linebuf-builder convention, same as report()
    call copy_str
    call flush_line
    ret

; =============================================================================
; Data.
; =============================================================================
AX_NONE    equ 0
AX_WRITTEN equ 1
AX_ERROR   equ 2
AX_TOO_BIG equ 3
AX_MISSING equ 4

; The values each resource can take. Anything outside these lists is refused,
; from the command line as much as from the menus.
opt_sbirq:  db 4, 2, 5, 7, 10
opt_dma8:   db 3, 0, 1, 3
opt_dma16:  db 3, 5, 6, 7
opt_wssirq: db 4, 7, 9, 10, 11
opt_mpu:    db 2, 0, 1

; row, col, width, fmt, cmos, peer, value, device, options, name
fields:
    db  8, 43, 4, FMT_DEC, CM_SB_IRQ,   F_WSIRQ, 7, DEV_SB
    dw  opt_sbirq,  nm_sbirq,  0, 0
    db  8, 51, 4, FMT_DEC, CM_SB_DMA8,  F_WSDMA, 1, DEV_SB
    dw  opt_dma8,   nm_sbdma,  0, 0
    db  8, 58, 4, FMT_DEC, CM_SB_DMA16, 0xFF,    5, DEV_SB
    dw  opt_dma16,  nm_sbd16,  0, 0
    db  9, 43, 4, FMT_DEC, CM_WSS_IRQ,  F_SBIRQ, 11, DEV_WSS
    dw  opt_wssirq, nm_wsirq,  0, 0
    db  9, 51, 4, FMT_DEC, CM_WSS_DMA,  F_SBDMA, 0, DEV_WSS
    dw  opt_dma8,   nm_wsdma,  0, 0
    db 10, 34, 5, FMT_MPU, CM_MPU_PORT, 0xFF,    1, DEV_ANY
    dw  opt_mpu,    nm_mpu,    0, 0

nm_sbirq:  db 'Sound Blaster IRQ', 0
nm_sbdma:  db 'Sound Blaster DMA', 0
nm_sbd16:  db 'Sound Blaster 16-bit DMA', 0
nm_wsirq:  db 'Windows Sound System IRQ', 0
nm_wsdma:  db 'Windows Sound System DMA', 0
nm_mpu:    db 'MPU-401 port', 0

; Switch keyword, kind (0 usage, 1 status, 2 value, 3 MPU port, 4 boot
; summary, 5 tree style), field (unused by flag kinds).
; Routing rule enforced in parse_tail: kinds 2-3 are the value switches and
; MUST stay contiguous (parse_tail range-tests them); every other kind is a
; flag and MUST get an explicit, guarded arm in the .flag chain.
sw_table:
    dw sw_question
    db 0, 0
    dw sw_h
    db 0, 0
    dw sw_help
    db 0, 0
    dw sw_s
    db 1, 0
    dw sw_status
    db 1, 0
    dw sw_sbirq
    db 2, F_SBIRQ
    dw sw_sbdma
    db 2, F_SBDMA
    dw sw_sbdma16
    db 2, F_SBD16
    dw sw_sbhdma
    db 2, F_SBD16
    dw sw_sbdmal
    db 2, F_SBDMA
    dw sw_sbdmah
    db 2, F_SBD16
    dw sw_wssirq
    db 2, F_WSIRQ
    dw sw_wssdma
    db 2, F_WSDMA
    dw sw_mpu
    db 3, F_MPU
    dw sw_midi
    db 3, F_MPU
    dw sw_b
    db 4, 0
    dw sw_t
    db 5, 0
    dw 0
    db 0, 0

sw_question: db '?', 0
sw_h:        db 'H', 0
sw_help:     db 'HELP', 0
sw_s:        db 'S', 0
sw_status:   db 'STATUS', 0
sw_sbirq:    db 'SBIRQ', 0
sw_sbdma:    db 'SBDMA', 0
sw_sbdma16:  db 'SBDMA16', 0
sw_sbhdma:   db 'SBHDMA', 0
sw_sbdmal:   db 'SBDMAL', 0
sw_sbdmah:   db 'SBDMAH', 0
sw_wssirq:   db 'WSSIRQ', 0
sw_wssdma:   db 'WSSDMA', 0
sw_mpu:      db 'MPU', 0
sw_midi:     db 'MIDI', 0
sw_b:        db 'B', 0
sw_t:        db 'T', 0

; row, col, attribute, text
static_text:
    db  3, 21, A_TITLE
    dw t_title
    db  5,  6, A_TITLE
    dw t_h_device
    db  5, 34, A_TITLE
    dw t_h_port
    db  5, 43, A_TITLE
    dw t_h_irq
    db  5, 51, A_TITLE
    dw t_h_dma
    db  5, 58, A_TITLE
    dw t_h_dma16
    db  7,  6, A_BOX
    dw t_opl
    db  7, 35, A_BOX
    dw t_388
    db  7, 44, A_BOX
    dw t_star
    db  7, 52, A_BOX
    dw t_star
    db  7, 59, A_BOX
    dw t_star
    db  8,  6, A_BOX
    dw t_sb
    db  8, 35, A_BOX
    dw t_220
    db  9,  6, A_BOX
    dw t_wss
    db  9, 35, A_BOX
    dw t_530
    db  9, 59, A_BOX
    dw t_star
    db 10,  6, A_BOX
    dw t_midi
    db 10, 44, A_BOX
    dw t_9
    db 10, 52, A_BOX
    dw t_star
    db 10, 59, A_BOX
    dw t_star
    db 12,  6, A_TITLE
    dw t_environment
    db 15,  6, A_TITLE
    dw t_notes
    db 16,  6, A_BOX
    dw t_note1
    db 17,  6, A_BOX
    dw t_note2
    db 19,  6, A_BOX
    dw t_keys
    db 0xFF

t_title:       db 'ReSonique 2 Sound System Configuration', 0
t_h_device:    db 'Device', 0
t_h_port:      db 'Port', 0
t_h_irq:       db 'IRQ', 0
t_h_dma:       db 'DMAL', 0
t_h_dma16:     db 'DMAH', 0
t_opl:         db 'OPL3 FM synthesis', 0
t_sb:          db 'Sound Blaster 16', 0
t_wss:         db 'Windows Sound System', 0
t_midi:        db 'MIDI / MPU-401', 0
t_388:         db '388', 0
t_220:         db '220', 0
t_530:         db '530', 0
t_9:           db '9', 0
t_star:        db '*', 0
t_environment: db 'Environment', 0
t_notes:       db 'Notes', 0
t_note1:       db 'Changes take effect at once and are saved in CMOS.', 0
t_note2:       db 'BLASTER is rewritten here and in C:\AUTOEXEC.BAT.', 0
t_keys:        db 'Tab/Arrows  move     Enter  change     F10  save     Esc  cancel', 0

s_blaster:      db 'BLASTER', 0
s_setsound:     db 'SETSOUND', 0
s_blaster_eq:   db 'BLASTER=', 0
s_no_sb:        db 'No Sound Blaster detected.', 0
s_a220:         db 'A220 I', 0
s_sp_d:         db ' D', 0
s_sp_h:         db ' H', 0
s_sp_p:         db ' P', 0
s_t6:           db ' T6', 0
s_mpu300:       db '300', 0
s_mpu330:       db '330', 0
s_autoexec:     db 'C:\AUTOEXEC.BAT', 0
s_set_blaster:  db 'SET BLASTER=', 0
s_set_setsound: db 'SET SETSOUND=', 0
s_crlf:         db 13, 10, 0

s_rep_sb:       db '  Sound Blaster 16      port 220   IRQ ', 0
s_rep_wss:      db '  Windows Sound System  port 530   IRQ ', 0
s_rep_mpu:      db '  MIDI / MPU-401        port ', 0
s_rep_mpu_tail: db '   IRQ 9', 13, 10, 0
s_rep_dma:      db '   DMA ', 0
s_rep_dmal:     db '   DMAL ', 0
s_rep_dmah:     db '   DMAH ', 0
s_rep_blaster:  db '  BLASTER=', 0

msg_head:        db 'ReSonique 2 sound system', 13, 10, 0
msg_opl:         db '  OPL3 FM synthesis     port 388', 13, 10, 0
msg_sb_absent:   db '  Sound Blaster 16      not installed', 13, 10, 0
msg_wss_absent:  db '  Windows Sound System  not installed', 13, 10, 0

; ---- /B boot summary ---------------------------------------------------
; Tree glyphs match TOKAEMM/TOKAMOUS/IZCDEX's /T bytes exactly (0xC3 0xC4
; '>' ' ' for the heading, 0xB3 for the gutter) -- duplicated here per the
; family's deliberate per-tool policy rather than shared.
msg_boot_tree:     db 0xC3, 0xC4, '>', ' ', 0
msg_boot_gut:      db 0xB3, 0
msg_boot_head:     db 'ReSonique2 Configuration [Run SNDCTRL to change]', 13, 10, 0
msg_boot_ind:      times 5 db ' '     ; 5-space indent, per the /T spec
                   db 0
msg_boot_sb:       db 'SB16 ', 0
msg_boot_wss:      db 'WSS ', 0
msg_boot_midi:     db 'MIDI ', 0
msg_boot_absent:   db 'absent', 0
msg_boot_gap:      times 3 db ' '     ; 3-space gap between device groups
                   db 0
; The space precedes the letter; the letter itself sits directly against its
; digit (I7, not I 7) -- see boot_report, which appends the digits with u8dec.
msg_boot_irq:      db ' I', 0
msg_boot_dmal:     db ' D', 0
msg_boot_dmah:     db ' H', 0
msg_boot_midi_irq: db ' I9', 0       ; MPU IRQ is the fixed literal 9

msg_saved:       db 'Applied to the hardware and saved in CMOS.', 13, 10, 0
msg_env_ok:      db 'BLASTER updated in the current environment.', 13, 10, 0
msg_env_full:    db 'BLASTER left alone: no room in the master environment.', 13, 10, 0
msg_env_absent:  db 'BLASTER is not set in the environment; not created.', 13, 10, 0
msg_ax_ok:       db 'C:\AUTOEXEC.BAT updated.', 13, 10, 0
msg_ax_none:     db 'C:\AUTOEXEC.BAT has no SET BLASTER line; not created.', 13, 10, 0
msg_ax_missing:  db 'C:\AUTOEXEC.BAT not found; not created.', 13, 10, 0
msg_ax_error:    db 'C:\AUTOEXEC.BAT could not be rewritten.', 13, 10, 0
msg_cancelled:   db 'Cancelled; nothing was changed.', 13, 10, 0
msg_collision:   db 'Refused: the two devices would share an IRQ or DMA channel.', 13, 10, 0
msg_bad_switch:  db 'Unrecognised option: ', 0
msg_permitted:   db ': permitted values are ', 0
msg_comma:       db ', ', 0
msg_crlf:        db 13, 10, 0
msg_usage:
    db 'SNDCTRL - ReSonique 2 Sound System Configuration', 13, 10, 13, 10
    db '  SNDCTRL                 full-screen configuration', 13, 10
    db '  SNDCTRL /S              show the current assignment', 13, 10
    db '  SNDCTRL /B              two-row boot summary, then exit', 13, 10
    db '  SNDCTRL /B /T           boot summary with a tree-styled prefix', 13, 10
    db '  SNDCTRL /SBIRQ:n        Sound Blaster IRQ    2, 5, 7, 10', 13, 10
    db '  SNDCTRL /SBDMAL:n       Sound Blaster DMA    0, 1, 3', 13, 10
    db '  SNDCTRL /SBDMAH:n       Sound Blaster 16-bit DMA  5, 6, 7', 13, 10
    db '  SNDCTRL /WSSIRQ:n       Windows Sound Sys IRQ  7, 9, 10, 11', 13, 10
    db '  SNDCTRL /WSSDMA:n       Windows Sound Sys DMA  0, 1, 3', 13, 10
    db '  SNDCTRL /MPU:nnn        MPU-401 port         300, 330', 13, 10, 13, 10
    db 'Any setting given on the command line is applied without the', 13, 10
    db 'full-screen interface. The two devices may not share a line.', 13, 10
    db '/T only styles /B; alone or with anything else it has no effect.', 13, 10, 0

; ---- state ------------------------------------------------------------------
sb_present:  db 0
wss_present: db 0
want_usage:  db 0
want_status: db 0
want_boot:   db 0
tree_mode:   db 0
want_apply:  db 0
applied:     db 0
cur_kind:    db 0
cur_field:   db 0
bad_kind:    db 0
list_fmt:    db 0
cur_sel:     db 0
tmp_attr:    db 0
tmp_pad:     db 0
peer_value:  db 0
menu_count:  db 0
menu_sel:    db 0
menu_row:    db 0
menu_col:    db 0
menu_w:      db 0
menu_h:      db 0
menu_fmt:    db 0
env_blaster: db 0
env_setsound: db 0
ax_result:   db 0

env_seg:     dw 0
env_bytes:   dw 0
env_at:      dw 0
env_oldlen:  dw 0
env_newlen:  dw 0
env_used:    dw 0
env_delta:   dw 0
ax_len:      dw 0
ax_outlen:   dw 0
in_end:      dw 0
ln_start:    dw 0
ln_end:      dw 0
ln_next:     dw 0
lines_changed: dw 0

AUTOEXEC_OUT equ 8192

; ---- buffers ----------------------------------------------------------------
; Declared past the end of the image rather than inside it: a .COM is handed the
; whole 64K segment, so these cost address space but not file size.
image_end:
    absolute image_end
token:       resb TOKEN_MAX
numbuf:      resb 8
menu_vals:   resb 8
menu_bak:    resb 256
blaster_val: resb 64
linebuf:     resb 128
ax_in:       resb AUTOEXEC_MAX
ax_out:      resb AUTOEXEC_OUT
