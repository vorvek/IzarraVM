; MOUSE.COM - Toka-DOS PS/2 mouse driver (INT 33h), a TSR.
; Installs an INT 33h dispatcher and registers a PS/2 packet handler with the
; BIOS (INT 15h AX=C207). The BIOS INT 74h ISR far-calls the handler per packet.
; Assemble: nasm -f bin tools/mouse.asm -o MOUSE.COM
    cpu 386
    org 0x100

VIRT_MAX_X      equ 639
VIRT_MAX_Y      equ 199
CENTER_X        equ VIRT_MAX_X / 2
CENTER_Y        equ VIRT_MAX_Y / 2

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
cb_mask         dw 0
; The callback far pointer is laid out offset-then-segment so `call far [cb_off]`
; reads a valid 32-bit far pointer straight from this pair (Intel memory order:
; low word offset, high word segment). Keep cb_off immediately before cb_seg.
cb_off          dw 0
cb_seg          dw 0
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

; 0x00 reset and status. Re-centre, hide, clear edge counters, drop the callback
; and the re-entrancy guard, and report installed (AX=0xFFFF) with two buttons.
m_reset:
    mov word [cs:cur_x], CENTER_X
    mov word [cs:cur_y], CENTER_Y
    mov word [cs:show_count], 0xFFFF
    mov byte [cs:buttons], 0
    mov word [cs:press_cnt], 0
    mov word [cs:press_cnt + 2], 0
    mov word [cs:press_cnt + 4], 0
    mov word [cs:release_cnt], 0
    mov word [cs:release_cnt + 2], 0
    mov word [cs:release_cnt + 4], 0
    mov word [cs:mickey_x], 0
    mov word [cs:mickey_y], 0
    mov word [cs:saved_off], 0xFFFF
    mov byte [cs:in_callback], 0
    mov word [cs:cb_mask], 0
    mov word [cs:cb_seg], 0
    mov word [cs:cb_off], 0
    mov ax, 0xFFFF
    mov bx, 2
    iret

; 0x01 show: show_count = min(show_count+1, 0) (signed saturate at 0).
m_show:
    mov ax, [cs:show_count]
    inc ax
    cmp ax, 0
    jle .store                        ; signed: still <= 0, keep it
    xor ax, ax                        ; clamp at 0 (visible)
.store:
    mov [cs:show_count], ax
    ; cursor draw wired in Task 3.5
    iret

; 0x02 hide: show_count -= 1 (signed, no floor).
m_hide:
    ; cursor restore wired in Task 3.5
    dec word [cs:show_count]
    iret

; 0x03 get position and buttons.
m_getpos:
    mov bx, [cs:buttons]
    and bx, 0x0007
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    iret

; 0x04 set position, clamped to the active range.
m_setpos:
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
    iret

; 0x05 button press info. BX selects the button (0 left, 1 right, 2 middle).
; AX=current buttons, BX=press_cnt[i] then zero it, CX=press_x[i], DX=press_y[i].
; BX >= 3 returns count 0 and the current position.
m_press_info:
    mov ax, [cs:buttons]
    and ax, 0x0007
    cmp bx, 3
    jae .out_of_range
    shl bx, 1                         ; i*2 into the word arrays
    mov cx, [cs:press_x + bx]
    mov dx, [cs:press_y + bx]
    mov ax, [cs:press_cnt + bx]       ; ax = count to return
    mov word [cs:press_cnt + bx], 0
    mov bx, ax                        ; BX = count
    mov ax, [cs:buttons]
    and ax, 0x0007
    iret
.out_of_range:
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    mov bx, 0
    iret

; 0x06 button release info, the release_* mirror of 0x05.
m_release_info:
    mov ax, [cs:buttons]
    and ax, 0x0007
    cmp bx, 3
    jae .out_of_range
    shl bx, 1
    mov cx, [cs:release_x + bx]
    mov dx, [cs:release_y + bx]
    mov ax, [cs:release_cnt + bx]
    mov word [cs:release_cnt + bx], 0
    mov bx, ax
    mov ax, [cs:buttons]
    and ax, 0x0007
    iret
.out_of_range:
    mov cx, [cs:cur_x]
    mov dx, [cs:cur_y]
    mov bx, 0
    iret

; 0x07 set horizontal range. order(CX,DX) -> min_x,max_x, clamp to 0..VIRT_MAX_X,
; then reclamp the cursor into the new range.
m_set_hrange:
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
    iret

; 0x08 set vertical range, the min_y/max_y mirror of 0x07.
m_set_vrange:
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
    cmp bx, VIRT_MAX_Y
    jle .hi_ok
    mov bx, VIRT_MAX_Y
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
    iret

; 0x09 define graphics cursor: accept, inert in v1.
m_def_gfx_cursor:
    iret

; 0x0A define text cursor. BX==0 selects the software cursor: store the screen
; and cursor masks. Rendering is Task 3.5.
m_def_txt_cursor:
    cmp bx, 0
    jne .done
    mov [cs:text_screen_mask], cx
    mov [cs:text_cursor_mask], dx
.done:
    iret

; 0x0B read and clear the mickey counters.
m_read_mickeys:
    mov cx, [cs:mickey_x]
    mov dx, [cs:mickey_y]
    mov word [cs:mickey_x], 0
    mov word [cs:mickey_y], 0
    iret

; 0x0C set the user event handler: mask in CX, far pointer in ES:DX.
m_set_callback:
    mov [cs:cb_mask], cx
    mov [cs:cb_seg], es
    mov [cs:cb_off], dx
    iret

; 0x0D / 0x0E light-pen emulation on/off: inert.
m_lightpen_on:
    iret
m_lightpen_off:
    iret

; 0x0F set the mickey-to-pixel ratio.
m_set_ratio:
    mov [cs:ratio_x], cx
    mov [cs:ratio_y], dx
    iret

; 0x10 conditional-off region. order(CX,SI) -> cond_left,cond_right and
; order(DX,DI) -> cond_top,cond_bottom. Cursor hide-on-overlap is Task 3.5.
m_cond_off:
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
    iret

x33_high:
    ; Extended functions 0x12..0x24 are implemented in Task 3.4.
    iret

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

    ; signed dx: X byte, sign from status bit4
    mov al, [bp+10]
    cbw
    test dl, 0x10
    jnz .x_signed
    mov ah, 0
.x_signed:
    mov si, ax                        ; si = signed dx (screen sense)

    ; signed dy: Y byte, sign from status bit5; PS/2 is +up so negate for screen
    mov al, [bp+8]
    cbw
    test dl, 0x20
    jnz .y_signed
    mov ah, 0
.y_signed:
    neg ax
    mov di, ax                        ; di = signed screen dy

    add [mickey_x], si
    add [mickey_y], di

    ; position += delta, clamped to [min,max]
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

; ---- install / TSR ----
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
resident_end:
