; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; TOKAMOUS.COM - Toka-DOS PS/2 mouse driver (INT 33h), a TSR.
; Installs an INT 33h dispatcher and registers a PS/2 packet handler with the
; BIOS (INT 15h AX=C207). The BIOS INT 74h ISR far-calls the handler per packet.
; Hooks INT 10h so a mode set hides the cursor and re-sizes the virtual range,
; the way the Microsoft driver does. Draws the software cursor in text mode 03h
; and in VGA mode 13h (the fn 09h mask over a saved 16x16 background).
; Assemble: nasm -f bin tools/tokamous.asm -o TOKAMOUS.COM
    cpu 386
    org 0x100

VIRT_MAX_X      equ 639
VIRT_MAX_Y      equ 199
CENTER_X        equ VIRT_MAX_X / 2
CENTER_Y        equ VIRT_MAX_Y / 2
MCB_SCAN_START  equ 0x0050
MCB_SCAN_LIMIT  equ 0x0300
ARENA_TOP       equ 0xA000
CURSOR_W        equ 16                ; graphics cursor is a 16x16 mask pair
CURSOR_H        equ 16
MODE13_W        equ 320
MODE13_H        equ 200

start:
    jmp install

; ---- resident state (lives in the COM image, kept after TSR) ----
old_int33_off   dw 0
old_int33_seg   dw 0
; Saved INT 10h vector, laid out offset-then-segment for `jmp far [old_int10]`.
old_int10       dw 0
old_int10_seg   dw 0
cur_x           dw CENTER_X
cur_y           dw CENTER_Y
buttons         db 0
wheel           db 0                  ; signed wheel counter since last fn 0x03
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

; ---- graphics cursor (fn 09h) ----
; The Microsoft contract: 16 words of screen mask (AND) then 16 words of cursor
; mask (XOR), bit 15 = leftmost pixel, plus a hot spot. The defaults are the
; standard arrow. Mode 13h presents a pixel as (screen bit ? background : 0)
; XOR (cursor bit ? 0Fh : 0), the DOSBox-X rendering of the same masks.
hot_x           dw 0
hot_y           dw 0
gfx_screen_mask dw 0x3FFF, 0x1FFF, 0x0FFF, 0x07FF, 0x03FF, 0x01FF, 0x00FF, 0x007F
                dw 0x003F, 0x001F, 0x01FF, 0x00FF, 0x30FF, 0xF87F, 0xF87F, 0xFCFF
gfx_cursor_mask dw 0x0000, 0x4000, 0x6000, 0x7000, 0x7800, 0x7C00, 0x7E00, 0x7F00
                dw 0x7F80, 0x7C00, 0x6C00, 0x4600, 0x0600, 0x0300, 0x0300, 0x0000
gfx_drawn       db 0                 ; 1 = a graphics cursor is on screen (restore needed)
drawn_kind      db 0                 ; the vid_kind it was drawn in (a mode change drops it)
gfx_back_x      dw 0                 ; top-left pixel of the drawn cursor (signed, may clip)
gfx_back_y      dw 0
row_scr         dw 0                 ; per-row mask scratch shifted one bit per column
row_cur         dw 0
px_val          db 0                 ; colour handed to px_put
gfx_back        times CURSOR_W * CURSOR_H db 0   ; background under the drawn cursor

; ---- active video mode, classified from BDA 40:49 (see apply_mode) ----
; The kinds are the DOSBox-X INT10 PutPixel families the cursor can draw in.
VID_NONE        equ 0                ; unknown or SVGA: ranges only, no drawing
VID_TEXT        equ 1                ; colour text 03h (B800 cells)
VID_MODE13      equ 2                ; 320x200x256, one byte per pixel at A000
VID_PLANAR      equ 3                ; EGA/VGA 16-colour planar (0Dh-12h)
VID_CGA4        equ 4                ; 320x200x4 (04h/05h), two bits per pixel at B800
VID_CGA2        equ 5                ; 640x200x2 (06h), one bit per pixel at B800
vid_kind        db VID_TEXT
vid_w           dw 640               ; pixels across
vid_h           dw 200               ; pixels down
vid_shift       db 0                 ; virtual x >> vid_shift = pixel x (1 in 320-wide modes)
vid_bpr         dw 80                ; bytes per pixel row (planar/CGA: of one plane / bank)
scr_max_x       dw VIRT_MAX_X        ; horizontal virtual max for the active video mode
vesa_active     db 0                 ; 1 = the last mode set went through VBE 4F02h
vesa_w          dw 640               ; that VBE mode's size, from 4F01h
vesa_h          dw 480
vga_save        times 8 db 0         ; GC 0,1,3,4,5,8 and SEQ 2 around a planar draw
vbe_info        times 256 db 0       ; VBE 4F01h mode-info buffer

; BDA mode byte -> kind, virtual-x shift, width, height, bytes per row.
MODE_ENTRY      equ 9
mode_table:
    db 0x03, VID_TEXT,   0
    dw 640, 200, 160
    db 0x04, VID_CGA4,   1
    dw 320, 200, 80
    db 0x05, VID_CGA4,   1
    dw 320, 200, 80
    db 0x06, VID_CGA2,   0
    dw 640, 200, 80
    db 0x0D, VID_PLANAR, 1
    dw 320, 200, 40
    db 0x0E, VID_PLANAR, 0
    dw 640, 200, 80
    db 0x0F, VID_PLANAR, 0
    dw 640, 350, 80
    db 0x10, VID_PLANAR, 0
    dw 640, 350, 80
    db 0x11, VID_PLANAR, 0
    dw 640, 480, 80
    db 0x12, VID_PLANAR, 0
    dw 640, 480, 80
    db 0x13, VID_MODE13, 1
    dw 320, 200, 320
    db 0xFF

; ---- INT 33h dispatcher ----
; A flat compare ladder over the core function set 0x00..0x10. AX > 0x10 falls
; through to x33_high, which currently just returns.
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

; Classify the active BIOS video mode (BDA 40:49) and size the virtual space to
; it. The INT 33h coordinate system is 640 wide in every standard mode (so a
; 320-pixel mode sees even x only) and (rows-1) tall: 0..199 for text and the
; 200-line modes, 0..349 for the EGA modes, 0..479 for the VGA modes. A mode the
; table does not know draws nothing; if it was set through VBE 4F02h the range is
; that mode's pixel size (DOSBox-X takes CurMode's width/height there), else the
; 640x200 default. Sets vid_*, scr_max_x/y, max_x/max_y, cond_right/bottom and
; reclamps the cursor. Preserves ALL.
apply_mode:
    push ax
    push bx
    push cx
    push dx
    push si
    push es
    mov ax, 0x40
    mov es, ax
    mov al, [es:0x49]                  ; current video mode
    and al, 0x7F                       ; drop the no-clear flag bit
    ; A VBE mode set leaves the BDA byte stale (this BIOS does not rewrite
    ; 40:49 on 4F02h), so the recorded VBE state wins over the table: otherwise
    ; an SVGA mode would classify as the text mode it replaced and the cursor
    ; would be drawn into B800.
    cmp byte [cs:vesa_active], 0
    jne .unknown
    mov si, mode_table
.scan:
    mov ah, [cs:si]
    cmp ah, 0xFF
    je .unknown
    cmp ah, al
    je .found
    add si, MODE_ENTRY
    jmp .scan
.found:
    mov al, [cs:si + 1]
    mov [cs:vid_kind], al
    mov al, [cs:si + 2]
    mov [cs:vid_shift], al
    mov cx, [cs:si + 3]
    mov dx, [cs:si + 5]
    mov ax, [cs:si + 7]
    mov [cs:vid_bpr], ax
    jmp .size
.unknown:
    mov byte [cs:vid_kind], VID_NONE
    mov byte [cs:vid_shift], 0
    mov cx, 640
    mov dx, 200
    cmp byte [cs:vesa_active], 0
    je .size
    mov cx, [cs:vesa_w]
    mov dx, [cs:vesa_h]
.size:
    mov [cs:vid_w], cx
    mov [cs:vid_h], dx
    ; virtual extent: width << shift (320-wide modes span 0..639), height as is
    push cx
    mov cl, [cs:vid_shift]
    mov ax, [cs:vid_w]
    shl ax, cl
    pop cx
    dec ax
    mov [cs:scr_max_x], ax
    mov [cs:max_x], ax
    mov [cs:cond_right], ax
    mov bx, dx
    dec bx
    mov [cs:scr_max_y], bx
    mov [cs:max_y], bx
    mov [cs:cond_bottom], bx
    cmp [cs:cur_x], ax                 ; reclamp the cursor into the new range
    jbe .y_clamp
    mov [cs:cur_x], ax
.y_clamp:
    cmp [cs:cur_y], bx
    jbe .done
    mov [cs:cur_y], bx
.done:
    pop es
    pop si
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; 0x00 reset and status. Re-centre, hide, clear edge counters and their saved
; positions, drop the callback and the re-entrancy guard, and report installed
; (AX=0xFFFF) with two buttons. Returns AX,BX; preserves CX,DX,SI,DI.
m_reset:
    call cursor_hide                   ; restore any drawn cell before clearing state
    call apply_mode                    ; size the virtual space to the video mode
    mov ax, [cs:scr_max_x]
    shr ax, 1
    mov [cs:cur_x], ax                 ; centre in the active range
    mov ax, [cs:scr_max_y]
    shr ax, 1
    mov [cs:cur_y], ax
    mov word [cs:show_count], 0xFFFF
    mov byte [cs:buttons], 0
    mov byte [cs:wheel], 0             ; clear the accumulated wheel counter
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
    mov word [cs:min_x], 0             ; the range is the whole screen again
    mov word [cs:min_y], 0             ; (apply_mode set max_x/max_y)
    call gfx_default_cursor            ; back to the arrow, hot spot (0,0)
    mov ax, 0xFFFF
    mov bx, 2
    iret

; Restore the default arrow masks and a (0,0) hot spot. Preserves ALL.
gfx_default_cursor:
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
    mov word [hot_x], 0
    mov word [hot_y], 0
    mov si, default_screen_mask
    mov di, gfx_screen_mask
    mov cx, CURSOR_H * 2               ; both masks are contiguous, 32 words
    cld
    rep movsw
    pop es
    pop ds
    pop di
    pop si
    pop cx
    pop ax
    ret

default_screen_mask dw 0x3FFF, 0x1FFF, 0x0FFF, 0x07FF, 0x03FF, 0x01FF, 0x00FF, 0x007F
                    dw 0x003F, 0x001F, 0x01FF, 0x00FF, 0x30FF, 0xF87F, 0xF87F, 0xFCFF
default_cursor_mask dw 0x0000, 0x4000, 0x6000, 0x7000, 0x7800, 0x7C00, 0x7E00, 0x7F00
                    dw 0x7F80, 0x7C00, 0x6C00, 0x4600, 0x0600, 0x0300, 0x0300, 0x0000

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
    mov bl, [cs:buttons]
    and bl, 0x07
    mov bh, [cs:wheel]                 ; BH = signed wheel counter (CuteMouse wheel API)
    mov byte [cs:wheel], 0             ; consume the accumulated detents
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

; 0x07 set horizontal range. order(CX,DX) -> min_x,max_x as the program asked,
; then reclamp the cursor into the new range. The values are NOT clamped to the
; driver's idea of the screen: a program in an SVGA mode (which the BIOS mode byte
; does not describe) or one that wants a larger virtual space sets whatever range
; it draws its own cursor in, and the Microsoft driver and DOSBox-X both take it
; as given. Returns nothing; preserve ALL (AX,BX are scratch).
m_set_hrange:
    push ax
    push bx
    mov ax, cx                        ; ax = low candidate
    mov bx, dx                        ; bx = high candidate
    cmp ax, bx
    jle .ordered
    xchg ax, bx                       ; swap so ax <= bx
.ordered:
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
    call cursor_hide                  ; the cursor may have moved: redraw
    call cursor_show
    pop bx
    pop ax
    iret

; 0x08 set vertical range, the min_y/max_y mirror of 0x07 (same no-clamp rule).
; Returns nothing; preserve ALL (AX,BX are scratch).
m_set_vrange:
    push ax
    push bx
    mov ax, cx
    mov bx, dx
    cmp ax, bx
    jle .ordered
    xchg ax, bx
.ordered:
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
    call cursor_hide
    call cursor_show
    pop bx
    pop ax
    iret

; 0x09 define graphics cursor. BX = hot spot column, CX = hot spot row (signed,
; -16..16), ES:DX -> 16 words of screen mask then 16 words of cursor mask. Copies
; the masks into the resident state and redraws so a visible cursor changes shape
; at once (Microsoft Word calls this in text mode too; there it is harmless).
; Returns nothing; preserve ALL.
m_def_gfx_cursor:
    push ax
    push cx
    push si
    push di
    push ds
    push es
    mov [cs:hot_x], bx
    mov [cs:hot_y], cx
    call cursor_hide                  ; the old shape must come off the screen first
    push es
    pop ds
    mov si, dx                        ; DS:SI = caller's mask block
    push cs
    pop es
    mov di, gfx_screen_mask           ; ES:DI = resident masks (screen then cursor)
    mov cx, CURSOR_H * 2
    cld
    rep movsw
    call cursor_show
    pop es
    pop ds
    pop di
    pop si
    pop cx
    pop ax
    iret

; 0x0A define text cursor. BX==0 selects the software cursor: store the screen
; and cursor masks. Rendering is not implemented. Returns nothing; preserve ALL (no
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
; order(DX,DI) -> cond_top,cond_bottom. Cursor hide-on-overlap is not implemented.
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
    cmp ax, 0x0011
    je m_wheel_api
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

; 0x11 get wheel capabilities (CuteMouse wheel API). AX=0x574D ("WM") signals
; support; CX bit0 = wheel present; BX = button count. This driver tracks the
; signed wheel detents (fn 0x03 BH) on a 3-button PS/2 IntelliMouse, so report
; BX=3 / CX=1. Preserves DX,SI,DI.
m_wheel_api:
    mov ax, 0x574D
    mov bx, 3                         ; button count (this driver reports 3)
    mov cx, 1                         ; bit0 = wheel present
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

; ---- software cursor ----
; Text mode 03h: the cursor cell is col = cur_x >> 3, row = cur_y >> 3 (fixed
; 200-line Microsoft convention: 8 virtual lines per text row). Byte offset in
; B800 = (row*80 + col)*2. Presentation: cell' = (cell AND screen_mask) XOR
; cursor_mask.
; Graphics modes (vid_kind MODE13, PLANAR, CGA4, CGA2): the 16x16 fn 09h masks at
; pixel (cur_x >> vid_shift - hot_x, cur_y - hot_y). A pixel becomes
; (screen bit ? background : 0) XOR (cursor bit ? 0Fh : 0), the DOSBox-X
; rendering of the Microsoft masks; the CGA kinds keep the low bits of that. The
; background under the cursor is saved so it can be put back before the next
; draw. Pixel access goes through px_get/px_put, one routine per kind, the way
; DOSBox-X's INT10 GetPixel/PutPixel families do it: a byte at A000 for mode 13h,
; set/reset plus bit mask through the graphics controller for the planar modes,
; and the interleaved B800 banks for CGA.
; SVGA (VBE) modes draw nothing: DOSBox-X inhibits drawing there too, and the
; programs that use them draw their own cursor.
; All routines work on the resident state via [cs:...] and reach the video
; aperture through ES, so they are correct regardless of the caller's DS and safe
; from interrupt context. Each saves and restores every register it touches.

; Return ZF=1 when the active mode is the colour text mode.
cursor_text_mode:
    cmp byte [cs:vid_kind], VID_TEXT
    ret

; Return ZF=1 when the active mode is one the graphics cursor draws in.
cursor_gfx_mode:
    push ax
    mov al, [cs:vid_kind]
    cmp al, VID_MODE13
    je .yes
    cmp al, VID_PLANAR
    je .yes
    cmp al, VID_CGA4
    je .yes
    cmp al, VID_CGA2
    je .yes
    cmp al, 0xFF                      ; never equal: ZF=0
    pop ax
    ret
.yes:
    cmp al, al                        ; ZF=1
    pop ax
    ret

; cursor_hide: put back whatever the cursor covers (a text cell, or the saved
; graphics block) and mark nothing drawn. Safe to call when nothing is drawn.
; If the mode changed under a drawn cursor, the saved image is dropped rather
; than written into a different layout.
cursor_hide:
    push ax
    push bx
    push es
    cmp byte [cs:gfx_drawn], 0
    je .text
    mov byte [cs:gfx_drawn], 0
    mov al, [cs:drawn_kind]
    cmp al, [cs:vid_kind]
    jne .text
    call gfx_restore
.text:
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
; outside the conditional-off box, draw the cursor for the active mode. No-op
; otherwise. Assumes nothing is currently drawn (call cursor_hide first when
; moving).
cursor_show:
    push ax
    push bx
    push cx
    push dx
    push es
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
    call cursor_gfx_mode
    jne .not_gfx
    call gfx_draw
    jmp .done
.not_gfx:
    call cursor_text_mode
    jne .done
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

; ---- graphics cursor drawing ----
; Both routines walk the 16x16 block row by row; a pixel outside the screen is
; skipped (the masks still shift so the shape stays aligned). DS = CS inside.

; gfx_draw: save the background under the cursor into gfx_back and draw the
; masks. Records the top-left pixel and the mode kind so gfx_restore can find
; the block again.
gfx_draw:
    push ax
    push bx
    push cx
    push dx
    push si
    push di
    push ds
    push es
    push cs
    pop ds
    mov al, [vid_kind]
    mov [drawn_kind], al
    cmp al, VID_PLANAR
    jne .no_vga
    call vga_enter
.no_vga:
    mov ax, [cur_x]
    mov cl, [vid_shift]
    sar ax, cl                        ; virtual x -> pixel x
    sub ax, [hot_x]
    mov [gfx_back_x], ax
    mov ax, [cur_y]
    sub ax, [hot_y]
    mov [gfx_back_y], ax
    mov byte [gfx_drawn], 1
    xor si, si                        ; si = row 0..15
.row:
    mov bx, si
    shl bx, 1                         ; word index into the masks
    mov ax, [gfx_screen_mask + bx]
    mov [row_scr], ax
    mov ax, [gfx_cursor_mask + bx]
    mov [row_cur], ax
    mov di, si
    shl di, 4                         ; di = row*16 into gfx_back
    xor cx, cx                        ; cx = column 0..15
.col:
    shl word [row_scr], 1             ; CF = screen-mask bit for this column
    setc dl                           ; dl = 1 keep background, 0 black
    shl word [row_cur], 1
    setc dh                           ; dh = 1 invert (XOR 0Fh)
    mov bx, [gfx_back_y]
    add bx, si                        ; bx = screen y
    cmp bx, 0
    jl .next_col
    cmp bx, [vid_h]
    jge .next_col
    mov ax, [gfx_back_x]
    add ax, cx                        ; ax = screen x
    cmp ax, 0
    jl .next_col
    cmp ax, [vid_w]
    jge .next_col
    push ax                           ; px_get answers in AL; keep x for px_put
    call px_get                       ; al = background pixel at (ax, bx)
    mov [gfx_back + di], al           ; save it
    test dl, dl
    jnz .keep
    xor al, al                        ; screen bit clear: black
.keep:
    test dh, dh
    jz .store
    xor al, 0x0F                      ; cursor bit set: invert
.store:
    mov [px_val], al
    pop ax
    call px_put
.next_col:
    inc di
    inc cx
    cmp cx, CURSOR_W
    jb .col
    inc si
    cmp si, CURSOR_H
    jb .row
    cmp byte [drawn_kind], VID_PLANAR
    jne .done
    call vga_leave
.done:
    pop es
    pop ds
    pop di
    pop si
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; gfx_restore: write the saved background back at the recorded block position.
; Clips exactly as gfx_draw did, so every byte that was saved is the one put back.
gfx_restore:
    push ax
    push bx
    push cx
    push dx
    push si
    push di
    push ds
    push es
    push cs
    pop ds
    cmp byte [drawn_kind], VID_PLANAR
    jne .no_vga
    call vga_enter
.no_vga:
    xor si, si
.row:
    mov di, si
    shl di, 4
    xor cx, cx
.col:
    mov bx, [gfx_back_y]
    add bx, si
    cmp bx, 0
    jl .next_col
    cmp bx, [vid_h]
    jge .next_col
    mov ax, [gfx_back_x]
    add ax, cx
    cmp ax, 0
    jl .next_col
    cmp ax, [vid_w]
    jge .next_col
    mov dl, [gfx_back + di]
    mov [px_val], dl
    call px_put
.next_col:
    inc di
    inc cx
    cmp cx, CURSOR_W
    jb .col
    inc si
    cmp si, CURSOR_H
    jb .row
    cmp byte [drawn_kind], VID_PLANAR
    jne .done
    call vga_leave
.done:
    pop es
    pop ds
    pop di
    pop si
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; ---- per-kind pixel access ----
; px_get: ax = x, bx = y (inside the screen) -> al = colour. Preserves all else.
; px_put: ax = x, bx = y, [px_val] = colour. Preserves ALL.

px_get:
    push bx
    push cx
    push dx
    push di
    push es
    mov dl, [cs:vid_kind]
    cmp dl, VID_MODE13
    je .m13
    cmp dl, VID_PLANAR
    je .planar
    cmp dl, VID_CGA4
    je .cga4
    cmp dl, VID_CGA2
    je .cga2
    xor al, al
    jmp .done
.m13:
    call off_mode13
    mov al, [es:di]
    jmp .done
.planar:
    call off_planar                   ; di = byte, cl = bit shift
    xor ah, ah                        ; ah collects one bit per plane
    xor ch, ch                        ; ch = plane 0..3
    mov dx, 0x3CE
.plane:
    mov al, 4                         ; GC4 read map select
    out dx, al
    inc dx
    mov al, ch
    out dx, al
    dec dx
    mov al, [es:di]
    shr al, cl
    and al, 1
    push cx
    mov cl, ch
    shl al, cl                        ; into the plane's colour bit
    pop cx
    or ah, al
    inc ch
    cmp ch, 4
    jb .plane
    mov al, ah
    jmp .done
.cga4:
    call off_cga                      ; di = row base, ax = x
    mov cx, ax
    shr ax, 2
    add di, ax
    and cl, 3
    xor cl, 3
    shl cl, 1                         ; shift = (3 - (x & 3)) * 2
    mov al, [es:di]
    shr al, cl
    and al, 3
    jmp .done
.cga2:
    call off_cga
    mov cx, ax
    shr ax, 3
    add di, ax
    and cl, 7
    xor cl, 7                         ; shift = 7 - (x & 7)
    mov al, [es:di]
    shr al, cl
    and al, 1
.done:
    pop es
    pop di
    pop dx
    pop cx
    pop bx
    ret

px_put:
    push ax
    push bx
    push cx
    push dx
    push di
    push es
    mov dl, [cs:vid_kind]
    cmp dl, VID_MODE13
    je .m13
    cmp dl, VID_PLANAR
    je .planar
    cmp dl, VID_CGA4
    je .cga4
    cmp dl, VID_CGA2
    je .cga2
    jmp .done
.m13:
    call off_mode13
    mov al, [cs:px_val]
    mov [es:di], al
    jmp .done
.planar:
    ; Write mode 0 (vga_enter set it): the bit mask picks the pixel, set/reset
    ; supplies the colour to every plane, and the write data is irrelevant.
    call off_planar
    mov al, 1
    shl al, cl                        ; bit mask for this pixel (bit 7 = leftmost)
    mov ah, al
    mov dx, 0x3CE
    mov al, 8                         ; GC8 bit mask
    out dx, al
    inc dx
    mov al, ah
    out dx, al
    dec dx
    mov al, 0                         ; GC0 set/reset = colour
    out dx, al
    inc dx
    mov al, [cs:px_val]
    out dx, al
    dec dx
    mov al, 1                         ; GC1 enable set/reset on all planes
    out dx, al
    inc dx
    mov al, 0x0F
    out dx, al
    mov al, [es:di]                   ; load the latches
    mov byte [es:di], 0xFF
    jmp .done
.cga4:
    call off_cga
    mov cx, ax
    shr ax, 2
    add di, ax
    and cl, 3
    xor cl, 3
    shl cl, 1
    mov ah, 3
    shl ah, cl
    not ah                            ; ah = keep mask
    mov al, [cs:px_val]
    and al, 3
    shl al, cl
    mov dl, [es:di]
    and dl, ah
    or dl, al
    mov [es:di], dl
    jmp .done
.cga2:
    call off_cga
    mov cx, ax
    shr ax, 3
    add di, ax
    and cl, 7
    xor cl, 7
    mov ah, 1
    shl ah, cl
    not ah
    mov al, [cs:px_val]
    and al, 1
    shl al, cl
    mov dl, [es:di]
    and dl, ah
    or dl, al
    mov [es:di], dl
.done:
    pop es
    pop di
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; off_mode13: ax = x, bx = y -> es = A000, di = y*320 + x. Preserves ax, bx, cx.
; y*320 = y*256 + y*64, which fits a word for y < 200.
off_mode13:
    push ax
    push dx
    mov dx, 0xA000
    mov es, dx
    mov di, bx
    shl di, 6
    mov dx, bx
    shl dx, 8
    add di, dx
    pop dx
    pop ax
    add di, ax
    ret

; off_planar: ax = x, bx = y -> es = A000, di = y*bpr + x/8, cl = 7 - (x & 7).
; Preserves ax, bx.
off_planar:
    push ax
    push dx
    mov dx, 0xA000
    mov es, dx
    mov ax, bx
    mul word [cs:vid_bpr]             ; dx:ax = y*bpr (fits a word for any VGA mode)
    mov di, ax
    pop dx
    pop ax
    mov cx, ax
    shr cx, 3
    add di, cx
    mov cl, al
    and cl, 7
    xor cl, 7
    ret

; off_cga: bx = y -> es = B800, di = (y/2)*80 + (y & 1)*2000h. Preserves ax, bx.
off_cga:
    push ax
    push dx
    mov dx, 0xB800
    mov es, dx
    mov ax, bx
    shr ax, 1
    mov di, ax
    shl di, 4                         ; *16
    shl ax, 6                         ; *64
    add di, ax                        ; *80
    test bl, 1
    jz .even
    add di, 0x2000
.even:
    pop dx
    pop ax
    ret

; vga_enter / vga_leave: around a planar draw, save the graphics-controller and
; sequencer registers the pixel routines touch (GC 0,1,3,4,5,8 and SEQ 2), put
; the card into write mode 0 / read mode 0 with all planes enabled, and restore
; them afterwards (DOSBox-X: SaveVgaRegisters / RestoreVgaRegisters). A game
; interrupted mid-blit gets its register state back untouched. Preserve ALL.
vga_enter:
    push ax
    push bx
    push dx
    push si
    ; The index ports first: the interrupted code may sit between selecting a
    ; register and touching its data port, so the selection must come back too.
    mov dx, 0x3CE
    in al, dx
    mov [cs:vga_gc_index], al
    mov dx, 0x3C4
    in al, dx
    mov [cs:vga_seq_index], al
    mov si, vga_save
    mov dx, 0x3CE
    mov bx, gc_saved_regs
.save_gc:
    mov al, [cs:bx]
    cmp al, 0xFF
    je .save_seq
    out dx, al
    inc dx
    in al, dx
    dec dx
    mov [cs:si], al
    inc si
    inc bx
    jmp .save_gc
.save_seq:
    mov dx, 0x3C4
    mov al, 2
    out dx, al
    inc dx
    in al, dx
    mov [cs:si], al
    ; SEQ2 map mask: all planes
    mov al, 0x0F
    out dx, al
    mov dx, 0x3CE
    mov al, 3                         ; GC3 data rotate / function: replace
    out dx, al
    inc dx
    xor al, al
    out dx, al
    dec dx
    mov al, 5                         ; GC5 mode: write mode 0, read mode 0
    out dx, al
    inc dx
    xor al, al
    out dx, al
    pop si
    pop dx
    pop bx
    pop ax
    ret

vga_leave:
    push ax
    push bx
    push dx
    push si
    mov si, vga_save
    mov dx, 0x3CE
    mov bx, gc_saved_regs
.restore_gc:
    mov al, [cs:bx]
    cmp al, 0xFF
    je .restore_seq
    out dx, al
    inc dx
    mov al, [cs:si]
    out dx, al
    dec dx
    inc si
    inc bx
    jmp .restore_gc
.restore_seq:
    mov dx, 0x3C4
    mov al, 2
    out dx, al
    inc dx
    mov al, [cs:si]
    out dx, al
    ; The index selections last, so the interrupted code finds them as left.
    mov dx, 0x3C4
    mov al, [cs:vga_seq_index]
    out dx, al
    mov dx, 0x3CE
    mov al, [cs:vga_gc_index]
    out dx, al
    pop si
    pop dx
    pop bx
    pop ax
    ret

gc_saved_regs   db 0, 1, 3, 4, 5, 8, 0xFF
vga_gc_index    db 0                 ; 3CEh index as the interrupted code left it
vga_seq_index   db 0                 ; 3C4h index likewise

; ---- INT 10h hook ----
; A mode set (AH=00h, or VBE AX=4F02h) takes the cursor off the screen first
; (the saved background belongs to the old layout), runs the BIOS, then
; re-classifies the mode and leaves the cursor hidden, as the Microsoft driver
; does after a mode change (DOSBox-X: Mouse_Before/AfterNewVideoMode). A BIOS
; mode set also resets the range to the whole screen and drops any
; conditional-off region. A VBE mode set keeps the range the program set
; (DOSBox-X does not reset min/max on an SVGA set-mode: some programs set their
; range first and switch modes after) and records the mode's pixel size for the
; next fn 00h reset. Every other INT 10h function goes straight through.
int10_hook:
    cmp ah, 0x00
    je .set_mode
    cmp ax, 0x4F02
    je .vesa_set
    jmp far [cs:old_int10]
.set_mode:
    call cursor_hide
    mov word [cs:show_count], 0xFFFF
    pushf
    call far [cs:old_int10]           ; run the BIOS mode set as an interrupt call
    mov byte [cs:vesa_active], 0
    call apply_mode                   ; new mode: new kind and virtual space
    mov word [cs:min_x], 0
    mov word [cs:min_y], 0
    mov byte [cs:cond_active], 0
    iret
.vesa_set:
    call cursor_hide
    mov word [cs:show_count], 0xFFFF
    pushf
    call far [cs:old_int10]
    cmp ax, 0x004F
    jne .vesa_done                    ; the set failed: nothing changed
    call vesa_record
    push word [cs:max_x]              ; apply_mode resets these; the program's
    push word [cs:max_y]              ; range survives a VBE set (see above)
    call apply_mode
    pop word [cs:max_y]
    pop word [cs:max_x]
.vesa_done:
    iret

; vesa_record: BX = the mode just set. Ask VBE 4F01h for its size and remember
; it as the unknown-mode virtual space. Preserves ALL (AX carries the 4F02h
; status back to the caller).
vesa_record:
    pusha
    push es
    push cs
    pop es
    mov di, vbe_info
    mov cx, bx
    and cx, 0x3FFF                    ; drop the LFB / no-clear request bits
    mov ax, 0x4F01
    int 0x10
    cmp ax, 0x004F
    jne .done
    mov ax, [cs:vbe_info + 0x12]      ; XResolution
    mov [cs:vesa_w], ax
    mov ax, [cs:vbe_info + 0x14]      ; YResolution
    mov [cs:vesa_h], ax
    mov byte [cs:vesa_active], 1
.done:
    pop es
    popa
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
    push es
    push cs
    pop ds                            ; resident state is in CS

    mov dx, [bp+12]                   ; status
    mov dh, [buttons]                 ; dh = OLD button mask (for edge detect)
    mov al, dl
    and al, 0x07
    mov [buttons], al                 ; new button mask (bit0 L, bit1 R, bit2 M)

    ; wheel: signed Z from the 4-word frame (0 for a 3-byte mouse) into the counter.
    ; DS=CS here so [wheel] is the resident byte; dx still carries status, leave it.
    mov al, [bp+6]
    add [wheel], al

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
    mov byte [cs:in_callback], 0
.no_callback:

    pop es
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
    ; Command-tail scan for /T or -T: TOKAMOUS takes no other arguments and
    ; the only effect is a banner prefix, so this is a raw BYTE SWEEP over the
    ; whole tail rather than the token-anchored parse sndctrl.asm's
    ; parse_tail does. It does not check that the lead-in starts a token, so
    ; an argument like X-TREME would also set tree_mode (the "-T" inside it
    ; matches) -- deliberate, since there is nothing else on the tail for it
    ; to misfire against.
    ;
    ; DOS tools took either lead-in (see sndctrl.asm's parse_tail). The
    ; lead-in byte below is tested RAW, before any upcase: '/' (0x2F) and '-'
    ; (0x2D) both fold to control characters under 'and 0xDF', so upcasing
    ; first would kill both lead-ins. Only the switch letter ('T') is upcased,
    ; once a lead-in has already matched.
    mov si, 0x81
    mov cl, [0x80]
    xor ch, ch
.t_scan:
    jcxz .t_done
    lodsb
    dec cx
    cmp al, '/'
    je .t_lead
    cmp al, '-'
    jne .t_scan
.t_lead:
    jcxz .t_done
    lodsb
    dec cx
    and al, 0xDF
    cmp al, 'T'
    jne .t_scan
    mov byte [tree_mode], 1
.t_done:
    push es
    xor ax, ax
    mov es, ax
    mov ax, [es:0x33*4]
    mov [cs:old_int33_off], ax
    mov ax, [es:0x33*4 + 2]
    mov [cs:old_int33_seg], ax
    mov ax, [es:0x10*4]
    mov [cs:old_int10], ax
    mov ax, [es:0x10*4 + 2]
    mov [cs:old_int10_seg], ax
    cli
    mov word [es:0x33*4], int33
    mov [es:0x33*4 + 2], cs
    mov word [es:0x10*4], int10_hook
    mov [es:0x10*4 + 2], cs
    sti
    pop es
    mov ax, 0xC205
    mov bx, 0x0300
    int 0x15
    mov ax, 0xC202
    mov bx, 0x0600                    ; sample-rate code 6 = 200 Hz
    int 0x15
    mov ax, 0xC207
    push cs
    pop es
    mov bx, packet_handler
    int 0x15
    mov ax, 0xC200
    mov bx, 0x0100
    int 0x15
    cmp byte [tree_mode], 0
    je .t_plain
    mov ah, 0x09
    mov dx, banner_tree
    int 0x21
.t_plain:
    mov ah, 0x09
    mov dx, banner
    int 0x21
    mov dx, (resident_end - start + 0x100 + 15) >> 4
    mov ax, 0x3100
    int 0x21

banner          db 'Toka-DOS mouse driver installed.', 13, 10, '$'
; banner_tree and tree_mode MUST stay on this transient side of
; resident_end: the KEEP paragraph count above is
; (resident_end - start + 0x100 + 15) >> 4, so grouping either of these with
; the resident state block near the top of the file would silently grow the
; TSR by however many bytes they add -- nothing tests for that regression.
banner_tree     db 0xC3, 0xC4, '>', ' ', '$'
tree_mode       db 0
