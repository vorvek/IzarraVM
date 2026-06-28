; TOKAMOUS.COM - Toka-DOS PS/2 mouse driver (INT 33h), a TSR.
; Installs an INT 33h dispatcher and registers a PS/2 packet handler with the
; BIOS (INT 15h AX=C207). The BIOS INT 74h ISR far-calls the handler per packet.
; Assemble: nasm -f bin tools/izmouse.asm -o TOKAMOUS.COM
    cpu 386
    org 0x100

VIRT_MAX_X      equ 639
VIRT_MAX_Y      equ 199
CENTER_X        equ VIRT_MAX_X / 2
CENTER_Y        equ VIRT_MAX_Y / 2
MCB_SCAN_START  equ 0x0050
MCB_SCAN_LIMIT  equ 0x0300
ARENA_TOP       equ 0xA000

start:
    jmp install

; ---- resident state (lives in the COM image, kept after TSR) ----
old_int33_off   dw 0
old_int33_seg   dw 0
cur_x           dw CENTER_X
cur_y           dw CENTER_Y
buttons         db 0
show_count      dw 0xFFFF            ; -1, hidden
min_x           dw 0
max_x           dw VIRT_MAX_X
min_y           dw 0
max_y           dw VIRT_MAX_Y
scr_max_y       dw VIRT_MAX_Y        ; vertical virtual max for the active video mode
press_cnt       times 3 dw 0
release_cnt     times 3 dw 0
press_x         times 3 dw 0
press_y         times 3 dw 0
release_x       times 3 dw 0
release_y       times 3 dw 0
mickey_x        dw 0
mickey_y        dw 0
ratio_x         dw 8
ratio_y         dw 16
accum_x         dw 0                 ; sub-pixel remainder carried by the ratio scale
accum_y         dw 0
cb_mask         dw 0
; The callback far pointer is laid out offset-then-segment so `call far [cb_off]`
; reads a valid 32-bit far pointer straight from this pair (Intel memory order:
; low word offset, high word segment). Keep cb_off immediately before cb_seg.
cb_off          dw 0
cb_seg          dw 0
cb_owner        dw 0                  ; owner PSP from the live MCB containing cb_seg
cb_mcb_seg      dw 0                  ; MCB header paragraph for that owner block
cond_left       dw 0
cond_top        dw 0
cond_right      dw VIRT_MAX_X
cond_bottom     dw VIRT_MAX_Y
disp_page       dw 0
dbl_speed       dw 64
sens_x          dw 50
sens_y          dw 50
sens_thr        dw 64
text_screen_mask dw 0x77FF
text_cursor_mask dw 0x7700
saved_cell      dw 0
saved_off       dw 0xFFFF
in_callback     db 0                 ; re-entrancy guard for the user callback
cond_active     db 0                 ; 1 = a conditional-off region is in effect
cb_live_tmp     db 0                 ; scratch result byte for callback validation

; ---- INT 33h dispatcher ----
; A flat compare ladder over the core function set 0x00..0x10. AX > 0x10 falls
; through to x33_high, which Task 3.4 fills in (today it just returns).
; State is accessed CS-relative throughout (the TSR runs on the caller's DS).
int33:
    sti
    cmp ax, 0x0000
    je m_reset
    cmp ax, 0x0001
    je m_show
    cmp ax, 0x0002
    je m_hide
    cmp ax, 0x0003
    je m_getpos
    cmp ax, 0x0004
    je m_setpos
    cmp ax, 0x0005
    je m_press_info
    cmp ax, 0x0006
    je m_release_info
    cmp ax, 0x0007
    je m_set_hrange
    cmp ax, 0x0008
    je m_set_vrange
    cmp ax, 0x0009
    je m_def_gfx_cursor
    cmp ax, 0x000A
    je m_def_txt_cursor
    cmp ax, 0x000B
    je m_read_mickeys
    cmp ax, 0x000C
    je m_set_callback
    cmp ax, 0x000D
    je m_lightpen_on
    cmp ax, 0x000E
    je m_lightpen_off
    cmp ax, 0x000F
    je m_set_ratio
    cmp ax, 0x0010
    je m_cond_off
    jmp x33_high

; Size the vertical virtual range to the active BIOS video mode (BDA 40:49). The
; INT 33h coordinate system is 0..639 x 0..(rows-1): text and 200-line modes use
; 0..199, the 640x350 EGA modes 0..349, and the 640x480 VGA modes 0..479. Without
; this a high-res mode (BGI sets 0x12) leaves the cursor clamped to the top 199
; rows. Sets scr_max_y/max_y/cond_bottom and reclamps the cursor. Preserves ALL.
apply_mode_yrange:
    push ax
    push bx
    push es
    mov ax, 0x40
    mov es, ax
    mov al, [es:0x49]                  ; current video mode
    and al, 0x7F                       ; drop the no-clear flag bit
    mov bx, VIRT_MAX_Y                 ; default 199 (text + 200-line modes)
    cmp al, 0x0F
    je .y349
    cmp al, 0x10
    je .y349
    cmp al, 0x11
    je .y479
    cmp al, 0x12
    je .y479
    jmp .store
.y349:
    mov bx, 349
    jmp .store
.y479:
    mov bx, 479
.store:
    mov [cs:scr_max_y], bx
    mov [cs:max_y], bx
    mov [cs:cond_bottom], bx
    mov ax, [cs:cur_y]                 ; reclamp the cursor into the new range
    cmp ax, bx
    jbe .done
    mov [cs:cur_y], bx
.done:
    pop es
    pop bx
    pop ax
    ret

; 0x00 reset and status. Re-centre, hide, clear edge counters and their saved
; positions, drop the callback and the re-entrancy guard, and report installed
; (AX=0xFFFF) with two buttons. Returns AX,BX; preserves CX,DX,SI,DI.
m_reset:
    call cursor_hide                   ; restore any drawn cell before clearing state
    mov word [cs:cur_x], CENTER_X
    call apply_mode_yrange             ; size the vertical range to the video mode
    mov ax, [cs:scr_max_y]
    shr ax, 1
    mov [cs:cur_y], ax                 ; centre vertically in the active range
    mov word [cs:show_count], 0xFFFF
    mov byte [cs:buttons], 0
    mov word [cs:press_cnt], 0
    mov word [cs:press_cnt + 2], 0
    mov word [cs:press_cnt + 4], 0
    mov word [cs:release_cnt], 0
    mov word [cs:release_cnt + 2], 0
    mov word [cs:release_cnt + 4], 0
    mov word [cs:press_x], 0
    mov word [cs:press_x + 2], 0
    mov word [cs:press_x + 4], 0
    mov word [cs:press_y], 0
    mov word [cs:press_y + 2], 0
    mov word [cs:press_y + 4], 0
    mov word [cs:release_x], 0
    mov word [cs:release_x + 2], 0
    mov word [cs:release_x + 4], 0
    mov word [cs:release_y], 0
    mov word [cs:release_y + 2], 0
    mov word [cs:release_y + 4], 0
    mov word [cs:mickey_x], 0
    mov word [cs:mickey_y], 0
    mov word [cs:accum_x], 0
    mov word [cs:accum_y], 0
    mov word [cs:saved_off], 0xFFFF
    mov byte [cs:in_callback], 0
    mov byte [cs:cond_active], 0       ; no conditional-off region after reset
    mov word [cs:cb_mask], 0
    mov word [cs:cb_seg], 0
    mov word [cs:cb_off], 0
    mov word [cs:cb_owner], 0
    mov word [cs:cb_mcb_seg], 0
    mov ax, 0xFFFF
    mov bx, 2
    iret

; 0x01 show: show_count = min(show_count+1, 0) (signed saturate at 0). Returns
; nothing; preserve ALL (AX is scratch here, so save and restore it).
m_show:
    push ax
    mov ax, [cs:show_count]
    inc ax
    cmp ax, 0
    jle .store                        ; signed: AX <= 0 (hidden or boundary), store as-is
    xor ax, ax                        ; clamp at 0 (visible)
.store:
    mov [cs:show_count], ax
    mov byte [cs:cond_active], 0       ; Show cancels any active conditional-off region
    call cursor_hide                   ; restore any drawn cell so a redundant Show redraws cleanly
    call cursor_show                   ; draw if the count reached 0 (visible)
    pop ax
    iret

; 0x02 hide: show_count -= 1 (signed, no floor). Returns nothing; preserve ALL
; (no general register is touched).
m_hide:
    dec word [cs:show_count]
    call cursor_hide                   ; restore the cell so the cursor disappears
    iret

; 0x03 get position and buttons. Returns BX,CX,DX; preserves AX,SI,DI (none of
; them is written).
m_getpos:
    mov bx, [cs:buttons]
    and bx, 0x0007
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    iret

; 0x04 set position, clamped to the active range. Returns nothing; preserve ALL
; (AX is scratch, save and restore it).
m_setpos:
    push ax
    mov ax, cx
    cmp ax, [cs:min_x]
    jge .x_lo
    mov ax, [cs:min_x]
.x_lo:
    cmp ax, [cs:max_x]
    jle .x_hi
    mov ax, [cs:max_x]
.x_hi:
    mov [cs:cur_x], ax
    mov ax, dx
    cmp ax, [cs:min_y]
    jge .y_lo
    mov ax, [cs:min_y]
.y_lo:
    cmp ax, [cs:max_y]
    jle .y_hi
    mov ax, [cs:max_y]
.y_hi:
    mov [cs:cur_y], ax
    call cursor_hide                   ; move: restore the old cell, redraw at new
    call cursor_show
    pop ax
    iret

; 0x05 button press info. BX selects the button (0 left, 1 right, 2 middle).
; AX=current buttons, BX=press_cnt[i] then zero it, CX=press_x[i], DX=press_y[i].
; BX >= 3 returns count 0 and the current position. Returns AX,BX,CX,DX;
; preserves SI,DI (neither is written).
m_press_info:
    cmp bx, 3
    jae .out_of_range
    shl bx, 1                         ; i*2 into the word arrays
    mov cx, [cs:press_x + bx]
    mov dx, [cs:press_y + bx]
    mov ax, [cs:press_cnt + bx]       ; ax = count to return
    mov word [cs:press_cnt + bx], 0
    mov bx, ax                        ; BX = count
    mov ax, [cs:buttons]              ; AX = current buttons (the return value)
    and ax, 0x0007
    iret
.out_of_range:
    mov ax, [cs:buttons]
    and ax, 0x0007
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    mov bx, 0
    iret

; 0x06 button release info, the release_* mirror of 0x05. Returns AX,BX,CX,DX;
; preserves SI,DI.
m_release_info:
    cmp bx, 3
    jae .out_of_range
    shl bx, 1
    mov cx, [cs:release_x + bx]
    mov dx, [cs:release_y + bx]
    mov ax, [cs:release_cnt + bx]
    mov word [cs:release_cnt + bx], 0
    mov bx, ax
    mov ax, [cs:buttons]              ; AX = current buttons (the return value)
    and ax, 0x0007
    iret
.out_of_range:
    mov ax, [cs:buttons]
    and ax, 0x0007
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    mov bx, 0
    iret

; 0x07 set horizontal range. order(CX,DX) -> min_x,max_x, clamp to 0..VIRT_MAX_X,
; then reclamp the cursor into the new range. Returns nothing; preserve ALL
; (AX,BX are scratch).
m_set_hrange:
    push ax
    push bx
    mov ax, cx                        ; ax = low candidate
    mov bx, dx                        ; bx = high candidate
    cmp ax, bx
    jle .ordered
    xchg ax, bx                       ; swap so ax <= bx
.ordered:
    ; clamp low to >= 0
    cmp ax, 0
    jge .lo_ok
    xor ax, ax
.lo_ok:
    ; clamp high to <= VIRT_MAX_X
    cmp bx, VIRT_MAX_X
    jle .hi_ok
    mov bx, VIRT_MAX_X
.hi_ok:
    mov [cs:min_x], ax
    mov [cs:max_x], bx
    ; reclamp cur_x
    mov ax, [cs:cur_x]
    cmp ax, [cs:min_x]
    jge .cx_lo
    mov ax, [cs:min_x]
.cx_lo:
    cmp ax, [cs:max_x]
    jle .cx_hi
    mov ax, [cs:max_x]
.cx_hi:
    mov [cs:cur_x], ax
    pop bx
    pop ax
    iret

; 0x08 set vertical range, the min_y/max_y mirror of 0x07. Returns nothing;
; preserve ALL (AX,BX are scratch).
m_set_vrange:
    push ax
    push bx
    mov ax, cx
    mov bx, dx
    cmp ax, bx
    jle .ordered
    xchg ax, bx
.ordered:
    cmp ax, 0
    jge .lo_ok
    xor ax, ax
.lo_ok:
    cmp bx, [cs:scr_max_y]
    jle .hi_ok
    mov bx, [cs:scr_max_y]
.hi_ok:
    mov [cs:min_y], ax
    mov [cs:max_y], bx
    mov ax, [cs:cur_y]
    cmp ax, [cs:min_y]
    jge .cy_lo
    mov ax, [cs:min_y]
.cy_lo:
    cmp ax, [cs:max_y]
    jle .cy_hi
    mov ax, [cs:max_y]
.cy_hi:
    mov [cs:cur_y], ax
    pop bx
    pop ax
    iret

; 0x09 define graphics cursor: accept, inert in v1. Returns nothing; preserve ALL.
m_def_gfx_cursor:
    iret

; 0x0A define text cursor. BX==0 selects the software cursor: store the screen
; and cursor masks. Rendering is Task 3.5. Returns nothing; preserve ALL (no
; register is written).
m_def_txt_cursor:
    cmp bx, 0
    jne .done
    mov [cs:text_screen_mask], cx
    mov [cs:text_cursor_mask], dx
.done:
    iret

; 0x0B read and clear the mickey counters. Returns CX,DX; preserves AX,BX,SI,DI
; (none of them is written).
m_read_mickeys:
    mov cx, [cs:mickey_x]
    mov dx, [cs:mickey_y]
    mov word [cs:mickey_x], 0
    mov word [cs:mickey_y], 0
    iret

; 0x0C set the user event handler: mask in CX, far pointer in ES:DX. Returns
; nothing; preserve ALL (no register is written).
m_set_callback:
    mov [cs:cb_mask], cx
    mov [cs:cb_seg], es
    mov [cs:cb_off], dx
    mov word [cs:cb_owner], 0
    mov word [cs:cb_mcb_seg], 0
    push ax
    mov ax, es
    or ax, dx
    or ax, cx
    jz .no_owner
    call find_callback_mcb
.no_owner:
    pop ax
    iret

; 0x0D / 0x0E light-pen emulation on/off: inert. Returns nothing; preserve ALL.
m_lightpen_on:
    iret
m_lightpen_off:
    iret

; 0x0F set the mickey-to-pixel ratio (mickeys per 8 pixels per axis). A zero would
; divide-by-zero in the packet handler's scale, so clamp each axis to at least 1.
; Returns nothing; preserves ALL (ax is saved and restored).
m_set_ratio:
    push ax
    mov ax, cx
    or ax, ax
    jnz .rx
    inc ax                            ; 0 is invalid; keep it non-zero
.rx:
    mov [cs:ratio_x], ax
    mov ax, dx
    or ax, ax
    jnz .ry
    inc ax
.ry:
    mov [cs:ratio_y], ax
    pop ax
    iret

; 0x10 conditional-off region. order(CX,SI) -> cond_left,cond_right and
; order(DX,DI) -> cond_top,cond_bottom. Cursor hide-on-overlap is Task 3.5.
; Returns nothing; preserve ALL (AX,BX are scratch).
m_cond_off:
    push ax
    push bx
    mov ax, cx
    mov bx, si
    cmp ax, bx
    jle .h_ok
    xchg ax, bx
.h_ok:
    mov [cs:cond_left], ax
    mov [cs:cond_right], bx
    mov ax, dx
    mov bx, di
    cmp ax, bx
    jle .v_ok
    xchg ax, bx
.v_ok:
    mov [cs:cond_top], ax
    mov [cs:cond_bottom], bx
    mov byte [cs:cond_active], 1       ; the region is now in effect (one-shot)
    call cursor_hide                   ; re-evaluate: hide if now inside the box
    call cursor_show                   ; redraw if still visible and outside it
    pop bx
    pop ax
    iret

; ---- extended INT 33h dispatcher (AX 0x12..0x24 and aliases) ----
; Each arm preserves every register outside its documented return set.
; State is always accessed CS-relative (the TSR runs on the caller's DS).
x33_high:
    cmp ax, 0x0012
    je m_large_gfx_cursor
    cmp ax, 0x0013
    je m_set_dbl_speed
    cmp ax, 0x0014
    je m_exchange_handler
    cmp ax, 0x0015
    je m_get_buf_size
    cmp ax, 0x0016
    je m_save_state
    cmp ax, 0x0017
    je m_restore_state
    cmp ax, 0x001A
    je m_set_sensitivity
    cmp ax, 0x001B
    je m_get_sensitivity
    cmp ax, 0x001D
    je m_set_disp_page
    cmp ax, 0x001E
    je m_get_disp_page
    cmp ax, 0x0021
    je m_soft_reset
    cmp ax, 0x0022
    je m_set_language
    cmp ax, 0x0023
    je m_get_language
    cmp ax, 0x0024
    je m_get_version
    cmp ax, 0x0042
    je m_get_buf_size_42
    cmp ax, 0x0050
    je m_save_state
    cmp ax, 0x0052
    je m_restore_state
    ; catch-all: leave all registers unchanged
    iret

; 0x12 define large graphics cursor: return AX=0xFFFF. Preserves BX,CX,DX,SI,DI.
m_large_gfx_cursor:
    mov ax, 0xFFFF
    iret

; 0x13 set double-speed threshold: dbl_speed=CX; if CX==0 set dbl_speed=64.
; Returns nothing; preserve ALL (AX,BX,DX,SI,DI - none written).
m_set_dbl_speed:
    push ax
    mov ax, cx
    cmp ax, 0
    jne .store
    mov ax, 64
.store:
    mov [cs:dbl_speed], ax
    pop ax
    iret

; 0x14 exchange user event handler.
; Returns CX=old cb_mask, ES=old cb_seg, DX=old cb_off.
; Installs new handler: cb_mask=(incoming CX), cb_seg=(incoming ES), cb_off=(incoming DX).
; Preserves AX,BX,SI,DI.
; Strategy: read ALL old values into scratch registers first, write new values, then
; set the return registers. Scratch registers used: AX (old mask), SI (old off),
; DI (old seg). Push/pop AX,BX,SI,DI to satisfy the preserve contract.
m_exchange_handler:
    push ax
    push bx
    push si
    push di
    ; Stage old values before any field write.
    mov ax, [cs:cb_mask]        ; ax = old mask
    mov si, [cs:cb_off]         ; si = old off
    mov di, [cs:cb_seg]         ; di = old seg
    ; Write the new values (caller's CX, DX, ES are still intact at this point).
    mov [cs:cb_mask], cx
    mov [cs:cb_off], dx
    mov [cs:cb_seg], es
    ; Build return registers from the staged old values.
    mov cx, ax                  ; CX = old mask
    mov dx, si                  ; DX = old off
    ; ES = old seg: push DI (old seg) and pop into ES.
    push di
    pop es
    pop di
    pop si
    pop bx
    pop ax
    iret

; 0x15 get state buffer size: BX=44. Preserves AX,CX,DX,SI,DI.
m_get_buf_size:
    mov bx, 44
    iret

; 0x42 alias of 0x15 but also returns AX=0xFFFF. Preserves CX,DX,SI,DI.
m_get_buf_size_42:
    mov ax, 0xFFFF
    mov bx, 44
    iret

; 0x16 save driver state to ES:DX (alias 0x50 routes here too).
; Copies the 44-byte state blob. Returns nothing; preserve ALL.
; Save/restore blob layout (22 words, 44 bytes):
;   word  0: magic 0x334D
;   word  1: cur_x        word  2: cur_y       word  3: show_count
;   word  4: buttons (as word, low byte)        word  5: min_x
;   word  6: max_x        word  7: min_y        word  8: max_y
;   word  9: ratio_x      word 10: ratio_y      word 11: cond_left
;   word 12: cond_top     word 13: cond_right   word 14: cond_bottom
;   word 15: disp_page    word 16: sens_x       word 17: sens_y
;   word 18: sens_thr     word 19: cb_mask      word 20: cb_seg
;   word 21: cb_off
m_save_state:
    push ax
    push bx
    ; ES:DX is the caller-supplied buffer; use BX as the ES-relative index.
    mov bx, dx
    mov ax, 0x334D
    mov [es:bx +  0], ax        ; magic
    mov ax, [cs:cur_x]
    mov [es:bx +  2], ax
    mov ax, [cs:cur_y]
    mov [es:bx +  4], ax
    mov ax, [cs:show_count]
    mov [es:bx +  6], ax
    xor ax, ax
    mov al, [cs:buttons]
    mov [es:bx +  8], ax        ; buttons as word
    mov ax, [cs:min_x]
    mov [es:bx + 10], ax
    mov ax, [cs:max_x]
    mov [es:bx + 12], ax
    mov ax, [cs:min_y]
    mov [es:bx + 14], ax
    mov ax, [cs:max_y]
    mov [es:bx + 16], ax
    mov ax, [cs:ratio_x]
    mov [es:bx + 18], ax
    mov ax, [cs:ratio_y]
    mov [es:bx + 20], ax
    mov ax, [cs:cond_left]
    mov [es:bx + 22], ax
    mov ax, [cs:cond_top]
    mov [es:bx + 24], ax
    mov ax, [cs:cond_right]
    mov [es:bx + 26], ax
    mov ax, [cs:cond_bottom]
    mov [es:bx + 28], ax
    mov ax, [cs:disp_page]
    mov [es:bx + 30], ax
    mov ax, [cs:sens_x]
    mov [es:bx + 32], ax
    mov ax, [cs:sens_y]
    mov [es:bx + 34], ax
    mov ax, [cs:sens_thr]
    mov [es:bx + 36], ax
    mov ax, [cs:cb_mask]
    mov [es:bx + 38], ax
    mov ax, [cs:cb_seg]
    mov [es:bx + 40], ax
    mov ax, [cs:cb_off]
    mov [es:bx + 42], ax
    pop bx
    pop ax
    iret

; 0x17 restore driver state from ES:DX (alias 0x52 routes here too).
; Returns nothing; preserve ALL.
m_restore_state:
    push ax
    push bx
    mov bx, dx
    ; word 0 is magic - consume/skip it (read but discard).
    ; word 1 onward maps to fields in the same order as save.
    mov ax, [es:bx +  2]
    mov [cs:cur_x], ax
    mov ax, [es:bx +  4]
    mov [cs:cur_y], ax
    mov ax, [es:bx +  6]
    mov [cs:show_count], ax
    mov ax, [es:bx +  8]
    mov [cs:buttons], al        ; low byte only
    mov ax, [es:bx + 10]
    mov [cs:min_x], ax
    mov ax, [es:bx + 12]
    mov [cs:max_x], ax
    mov ax, [es:bx + 14]
    mov [cs:min_y], ax
    mov ax, [es:bx + 16]
    mov [cs:max_y], ax
    mov ax, [es:bx + 18]
    mov [cs:ratio_x], ax
    mov ax, [es:bx + 20]
    mov [cs:ratio_y], ax
    mov ax, [es:bx + 22]
    mov [cs:cond_left], ax
    mov ax, [es:bx + 24]
    mov [cs:cond_top], ax
    mov ax, [es:bx + 26]
    mov [cs:cond_right], ax
    mov ax, [es:bx + 28]
    mov [cs:cond_bottom], ax
    mov ax, [es:bx + 30]
    mov [cs:disp_page], ax
    mov ax, [es:bx + 32]
    mov [cs:sens_x], ax
    mov ax, [es:bx + 34]
    mov [cs:sens_y], ax
    mov ax, [es:bx + 36]
    mov [cs:sens_thr], ax
    mov ax, [es:bx + 38]
    mov [cs:cb_mask], ax
    mov ax, [es:bx + 40]
    mov [cs:cb_seg], ax
    mov ax, [es:bx + 42]
    mov [cs:cb_off], ax
    pop bx
    pop ax
    iret

; 0x1A set mouse sensitivity: sens_x=BX, sens_y=CX, sens_thr=DX.
; If DX==0 set sens_thr=64. Returns nothing; preserve ALL (AX is scratch).
m_set_sensitivity:
    push ax
    mov [cs:sens_x], bx
    mov [cs:sens_y], cx
    mov ax, dx
    cmp ax, 0
    jne .thr_ok
    mov ax, 64
.thr_ok:
    mov [cs:sens_thr], ax
    pop ax
    iret

; 0x1B get mouse sensitivity: BX=sens_x, CX=sens_y, DX=sens_thr.
; Preserves AX,SI,DI (none written).
m_get_sensitivity:
    mov bx, [cs:sens_x]
    mov cx, [cs:sens_y]
    mov dx, [cs:sens_thr]
    iret

; 0x1D set display page: disp_page=BX. Returns nothing; preserve ALL.
m_set_disp_page:
    mov [cs:disp_page], bx
    iret

; 0x1E get display page: BX=disp_page. Preserves AX,CX,DX,SI,DI.
m_get_disp_page:
    mov bx, [cs:disp_page]
    iret

; 0x21 software reset/detect: AX=0xFFFF, BX=2. No state clear. Preserves CX,DX,SI,DI.
m_soft_reset:
    mov ax, 0xFFFF
    mov bx, 2
    iret

; 0x22 set language: no-op. Returns nothing; preserve ALL.
m_set_language:
    iret

; 0x23 get language number: BX=0 (English). Preserves AX,CX,DX,SI,DI.
m_get_language:
    mov bx, 0
    iret

; 0x24 get driver version/type/IRQ.
; Returns BH=major(8), BL=minor(0x20), CH=mouse-type(4=PS/2), CL=IRQ(0=PS/2).
; Preserves AX,DX,SI,DI. The "BX=0 on entry" in the INT 33h spec is an INPUT
; calling-convention note to callers, not a guard the driver should enforce;
; programs rely on AX=0x24 returning version/type unconditionally.
m_get_version:
    mov bx, 0x0820
    mov cx, 0x0400
    iret

; Return AX = the first conventional MCB header Toka-DOS published. In the full
; boot path the system PSP is at 0200h (MCB 01FFh), while synthetic unit setups may
; use lower roots. Pick the first plausible self-owned block.
find_first_mcb:
    push bx
    push es
    mov ax, MCB_SCAN_START
.scan:
    cmp ax, MCB_SCAN_LIMIT
    jae .not_found
    mov es, ax
    mov bl, [es:0]
    cmp bl, 'M'
    je .sig_ok
    cmp bl, 'Z'
    jne .next
.sig_ok:
    mov bx, ax
    inc bx
    cmp [es:1], bx
    jne .next
    cmp word [es:3], 0
    jne .done
.next:
    inc ax
    jmp .scan
.not_found:
    mov ax, ARENA_TOP
.done:
    pop es
    pop bx
    ret

; Follow Toka-DOS's conventional-memory MCB chain and remember the live block that
; contains the registered callback segment. This runs when a program installs a
; callback, not from IRQ context.
find_callback_mcb:
    push ax
    push bx
    push cx
    push dx
    push es
    call find_first_mcb
.scan:
    cmp ax, ARENA_TOP
    jae .done
    mov es, ax
    mov bl, [es:0]
    cmp bl, 'M'
    je .valid_sig
    cmp bl, 'Z'
    jne .done
.valid_sig:
    mov dx, ax
    inc dx                              ; data segment = MCB + 1
    mov cx, dx
    add cx, [es:3]                      ; first paragraph after this block
    mov bx, [cs:cb_seg]
    cmp bx, dx
    jb .next
    cmp bx, cx
    jae .next
    mov [cs:cb_mcb_seg], ax
    mov bx, [es:1]
    mov [cs:cb_owner], bx
    jmp .done
.next:
    cmp byte [es:0], 'Z'
    je .done
    mov bx, [es:3]
    inc bx                              ; skip data plus the next MCB header
    add ax, bx
    jmp .scan
.done:
    pop es
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; Return ZF=0 when the registered callback still belongs to the same live MCB.
; If no owner was found at registration, allow the callback for compatibility.
callback_still_live:
    push ax
    push bx
    push cx
    push dx
    push es
    mov byte [cs:cb_live_tmp], 1
    mov dx, [cs:cb_owner]
    or dx, dx
    jz .done
    mov bx, [cs:cb_mcb_seg]
    or bx, bx
    jz .dead
    call find_first_mcb
.scan:
    cmp ax, ARENA_TOP
    jae .dead
    mov es, ax
    mov cl, [es:0]
    cmp cl, 'M'
    je .valid_sig
    cmp cl, 'Z'
    jne .dead
.valid_sig:
    cmp ax, bx
    je .candidate
    cmp cl, 'Z'
    je .dead
    mov cx, [es:3]
    inc cx
    add ax, cx
    jmp .scan
.candidate:
    cmp [es:1], dx
    jne .dead
    mov ax, bx
    inc ax
    mov cx, ax
    add cx, [es:3]
    mov dx, [cs:cb_seg]
    cmp dx, ax
    jb .dead
    cmp dx, cx
    jb .done
.dead:
    mov byte [cs:cb_live_tmp], 0
.done:
    pop es
    pop dx
    pop cx
    pop bx
    pop ax
    cmp byte [cs:cb_live_tmp], 0
    ret

; ---- text-mode software cursor (page 0, mode 03h, B800:0) ----
; The cursor cell is col = cur_x >> 3, row = cur_y >> 3 (fixed 200-line Microsoft
; convention: 8 virtual lines per text row). Byte offset in B800 =
; (row*80 + col)*2. Presentation: cell' = (cell AND screen_mask) XOR cursor_mask.
; Both routines work on the resident state via [cs:...] and reach B800 through ES,
; so they are correct regardless of the caller's DS and safe from interrupt
; context. Each saves and restores every register it touches.

; Return ZF=1 when the active BIOS video mode is color text mode 03h. The software
; cursor knows only the B800 80-column text layout; in graphics modes it must not
; touch the video aperture.
cursor_text_mode:
    push ax
    push es
    mov ax, 0x40
    mov es, ax
    mov al, [es:0x49]
    and al, 0x7F
    cmp al, 0x03
    pop es
    pop ax
    ret

; cursor_hide: if a cell is currently drawn (saved_off != 0xFFFF), write the saved
; cell back to B800 and mark none drawn. Safe to call when nothing is drawn.
cursor_hide:
    push ax
    push bx
    push es
    mov bx, [cs:saved_off]
    cmp bx, 0xFFFF
    je .done
    call cursor_text_mode
    jne .drop_saved
    mov ax, 0xB800
    mov es, ax
    mov ax, [cs:saved_cell]
    mov [es:bx], ax
.drop_saved:
    mov word [cs:saved_off], 0xFFFF
.done:
    pop es
    pop bx
    pop ax
    ret

; cursor_show: if visible (show_count == 0) and the cursor's virtual position is
; outside the conditional-off box, save the underlying cell and draw
; (cell AND screen_mask) XOR cursor_mask. No-op otherwise. Assumes nothing is
; currently drawn (call cursor_hide first when moving).
cursor_show:
    push ax
    push bx
    push cx
    push dx
    push es
    call cursor_text_mode
    jne .done
    cmp word [cs:show_count], 0
    jne .done                         ; hidden
    cmp byte [cs:cond_active], 0
    je .visible                       ; no active region: draw everywhere
    ; conditional-off test in virtual space: skip drawing if inside the box
    mov ax, [cs:cur_x]
    cmp ax, [cs:cond_left]
    jl .visible
    cmp ax, [cs:cond_right]
    jg .visible
    mov ax, [cs:cur_y]
    cmp ax, [cs:cond_top]
    jl .visible
    cmp ax, [cs:cond_bottom]
    jg .visible
    jmp .done                         ; inside the hidden box
.visible:
    ; cell offset = (row*80 + col)*2 ; col=cur_x>>3, row=cur_y>>3
    mov ax, [cs:cur_y]
    shr ax, 3
    mov bx, 80
    mul bx                            ; dx:ax = row*80 (row<=24 so ax is enough)
    mov bx, [cs:cur_x]
    shr bx, 3
    add ax, bx
    shl ax, 1                         ; byte offset
    mov bx, ax
    mov ax, 0xB800
    mov es, ax
    mov ax, [es:bx]
    mov [cs:saved_cell], ax
    mov [cs:saved_off], bx
    and ax, [cs:text_screen_mask]
    xor ax, [cs:text_cursor_mask]
    mov [es:bx], ax
.done:
    pop es
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; ---- PS/2 packet handler (far-called by the BIOS INT 74h ISR) ----
; Stack after prologue: [bp+6]=Z, [bp+8]=Y, [bp+10]=X, [bp+12]=status.
packet_handler:
    push bp
    mov bp, sp
    push ax
    push bx
    push cx
    push dx
    push si
    push di
    push ds
    push cs
    pop ds                            ; resident state is in CS

    mov dx, [bp+12]                   ; status
    mov dh, [buttons]                 ; dh = OLD button mask (for edge detect)
    mov al, dl
    and al, 0x07
    mov [buttons], al                 ; new button mask (bit0 L, bit1 R, bit2 M)

    ; signed dx: the status sign bit is authoritative, the packet byte is the low
    ; 8 bits. queue_movement clamps deltas to the 9-bit range -256..255, so a fast
    ; -256..-129 move has a magnitude byte whose own bit7 disagrees with the true
    ; sign. Sign-extend from status bit4, not from the byte, to span -256..255.
    mov al, [bp+10]                   ; X magnitude byte (low 8 bits)
    xor ah, ah
    test dl, 0x10                     ; status bit4: X negative?
    jz .x_done
    mov ah, 0xFF                      ; sign-extend per the status bit
.x_done:
    mov si, ax                        ; si = signed dx (screen sense), -256..255

    ; signed dy: same reconstruction from status bit5; PS/2 is +up so negate to
    ; screen sense (+down) afterwards.
    mov al, [bp+8]                    ; Y magnitude byte (low 8 bits)
    xor ah, ah
    test dl, 0x20                     ; status bit5: Y negative?
    jz .y_done
    mov ah, 0xFF
.y_done:
    neg ax                            ; flip PS/2 +up to screen +down
    mov di, ax                        ; di = signed screen dy, -256..255

    ; Mickeys accumulate in screen sense (positive = down), matching the Microsoft
    ; contract; this is intentional, not a missing negate.
    add [mickey_x], si
    add [mickey_y], di

    ; Scale the raw mickey delta to a pixel delta through the mickey-to-pixel ratio
    ; (pixels = mickeys * 8 / ratio), carrying the sub-pixel remainder per axis so
    ; slow motion is not truncated away. The default ratio is 8 horizontal (1:1) and
    ; 16 vertical (half speed). ratio_x/y are clamped non-zero by 0x0F so the idiv is
    ; safe, and the dividend stays well inside 16 bits for any sane ratio. dh holds
    ; the old button mask the edge code needs, so preserve dx across the divides.
    push dx
    mov ax, si
    sal ax, 3                         ; mickeys * 8 (signed, -2048..2040)
    add ax, [accum_x]                 ; carry the prior remainder
    cwd
    idiv word [ratio_x]               ; ax = pixel delta, dx = remainder
    mov [accum_x], dx
    mov si, ax                        ; si = scaled dx in pixels
    mov ax, di
    sal ax, 3
    add ax, [accum_y]
    cwd
    idiv word [ratio_y]
    mov [accum_y], dx
    mov di, ax                        ; di = scaled dy in pixels
    pop dx

    ; position += scaled delta, clamped to [min,max]
    mov ax, [cur_x]
    add ax, si
    cmp ax, [min_x]
    jge .xl
    mov ax, [min_x]
.xl:
    cmp ax, [max_x]
    jle .xh
    mov ax, [max_x]
.xh:
    mov [cur_x], ax
    mov ax, [cur_y]
    add ax, di
    cmp ax, [min_y]
    jge .yl
    mov ax, [min_y]
.yl:
    cmp ax, [max_y]
    jle .yh
    mov ax, [max_y]
.yh:
    mov [cur_y], ax

    ; the cursor reflects the new position: restore the old cell and redraw.
    ; cursor_hide/show use [cs:] state and save ax,bx,cx,dx,es, so DX (the status
    ; byte) and SI/DI (the signed deltas) survive for the button-edge code below.
    call cursor_hide
    call cursor_show

    ; three-button edge tracking. dh = old mask, bl = new mask.
    ; A 0->1 edge is a press: bump press_cnt[i], record press_x/y[i] = cur pos.
    ; A 1->0 edge is a release: bump release_cnt[i], record release_x/y[i].
    ; The index i*2 selects the word slot in each array.
    mov bl, [buttons]                 ; new mask

    ; ---- left button (bit0, i=0) ----
    test bl, 0x01
    jz .left_clear
    test dh, 0x01
    jnz .left_done                    ; was set: no edge
    inc word [press_cnt + 0]
    mov ax, [cur_x]
    mov [press_x + 0], ax
    mov ax, [cur_y]
    mov [press_y + 0], ax
    jmp .left_done
.left_clear:
    test dh, 0x01
    jz .left_done                     ; was clear: no edge
    inc word [release_cnt + 0]
    mov ax, [cur_x]
    mov [release_x + 0], ax
    mov ax, [cur_y]
    mov [release_y + 0], ax
.left_done:

    ; ---- right button (bit1, i=1) ----
    test bl, 0x02
    jz .right_clear
    test dh, 0x02
    jnz .right_done
    inc word [press_cnt + 2]
    mov ax, [cur_x]
    mov [press_x + 2], ax
    mov ax, [cur_y]
    mov [press_y + 2], ax
    jmp .right_done
.right_clear:
    test dh, 0x02
    jz .right_done
    inc word [release_cnt + 2]
    mov ax, [cur_x]
    mov [release_x + 2], ax
    mov ax, [cur_y]
    mov [release_y + 2], ax
.right_done:

    ; ---- middle button (bit2, i=2) ----
    test bl, 0x04
    jz .mid_clear
    test dh, 0x04
    jnz .mid_done
    inc word [press_cnt + 4]
    mov ax, [cur_x]
    mov [press_x + 4], ax
    mov ax, [cur_y]
    mov [press_y + 4], ax
    jmp .mid_done
.mid_clear:
    test dh, 0x04
    jz .mid_done
    inc word [release_cnt + 4]
    mov ax, [cur_x]
    mov [release_x + 4], ax
    mov ax, [cur_y]
    mov [release_y + 4], ax
.mid_done:

    ; user callback. Build an event-flags mask in cx per the Microsoft INT 33h
    ; AX=000C contract: bit0 motion, bit1 left press, bit2 left release,
    ; bit3 right press, bit4 right release, bit5 middle press, bit6 middle release.
    ; dh = old mask, bl = new mask.
    xor cx, cx
    ; motion (bit0): any non-zero dx or dy this packet.
    mov ax, si
    or ax, di
    jz .no_motion
    or cx, 0x0001
.no_motion:
    ; left press / release
    test bl, 0x01
    jz .l_lo
    test dh, 0x01
    jnz .lbtn_done                    ; still set: no edge
    or cx, 0x0002                     ; left press
    jmp .lbtn_done
.l_lo:
    test dh, 0x01
    jz .lbtn_done
    or cx, 0x0004                     ; left release
.lbtn_done:
    ; right press / release
    test bl, 0x02
    jz .r_lo
    test dh, 0x02
    jnz .rbtn_done
    or cx, 0x0008                     ; right press
    jmp .rbtn_done
.r_lo:
    test dh, 0x02
    jz .rbtn_done
    or cx, 0x0010                     ; right release
.rbtn_done:
    ; middle press / release
    test bl, 0x04
    jz .m_lo
    test dh, 0x04
    jnz .mbtn_done
    or cx, 0x0020                     ; middle press
    jmp .mbtn_done
.m_lo:
    test dh, 0x04
    jz .mbtn_done
    or cx, 0x0040                     ; middle release
.mbtn_done:

    ; fire only if a handler is registered, its mask overlaps the events, and we
    ; are not already inside a callback.
    mov ax, [cb_off]
    or ax, [cb_seg]
    jz .no_callback                   ; null handler
    mov ax, [cb_mask]
    and ax, cx
    jz .no_callback                   ; no event the caller asked for
    cmp byte [in_callback], 0
    jne .no_callback                  ; re-entrant, skip
    call callback_still_live
    jz .no_callback                   ; callback owner exited or block was reused

    mov byte [in_callback], 1
    ; Register block the Microsoft contract hands the callback:
    ;   AX=event flags, BX=buttons, CX=cur_x, DX=cur_y, SI=mickey_x, DI=mickey_y.
    mov ax, cx                        ; AX = event flags
    mov bl, [buttons]
    xor bh, bh                        ; BX = buttons
    mov cx, [cur_x]                   ; CX = cur_x
    mov dx, [cur_y]                   ; DX = cur_y
    mov si, [mickey_x]                ; SI = mickey_x
    mov di, [mickey_y]                ; DI = mickey_y
    ; The callback runs with DS = driver segment. Per common mouse-driver practice
    ; the application's callback establishes its own DS; in this IRQ-driven path
    ; there is no application caller whose DS to restore, so we deliberately do not
    ; restore one here. Revisit only if a corpus program needs it.
    call far [cb_off]                 ; far-call cb_seg:cb_off via the stored pair
    mov byte [in_callback], 0
.no_callback:

    pop ds
    pop di
    pop si
    pop dx
    pop cx
    pop bx
    pop ax
    pop bp
    retf

resident_end:
; ---- install / TSR (transient: discarded by AH=31h KEEP) ----
install:
    push es
    xor ax, ax
    mov es, ax
    mov ax, [es:0x33*4]
    mov [cs:old_int33_off], ax
    mov ax, [es:0x33*4 + 2]
    mov [cs:old_int33_seg], ax
    cli
    mov word [es:0x33*4], int33
    mov [es:0x33*4 + 2], cs
    sti
    pop es
    mov ax, 0xC205
    mov bx, 0x0300
    int 0x15
    mov ax, 0xC207
    push cs
    pop es
    mov bx, packet_handler
    int 0x15
    mov ax, 0xC200
    mov bx, 0x0100
    int 0x15
    mov ah, 0x09
    mov dx, banner
    int 0x21
    mov dx, (resident_end - start + 0x100 + 15) >> 4
    mov ax, 0x3100
    int 0x21

banner          db 'Toka-DOS mouse driver installed.', 13, 10, '$'
