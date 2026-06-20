; izbios-bootbox.inc - STREAM E: the boxed two-pane boot+speed menu. The left
; pane picks the boot device, the right pane picks the CPU speed, and a button bar
; below holds Accept and Cancel. boot_menu (izbios-boot.inc) is a one-line shim
; that jumps to bootbox_menu here.
;
; The keyboard path is the shipped, standalone deliverable. The polled PS/2 AUX
; mouse layer is gated behind BMX_MOUSE: it talks to the 8042 AUX channel the
; machine model implements (crates/izarravm-machine/src/keyboard.rs: the 0xD4
; write-to-aux prefix, enable/disable reporting, the AUX status bit on 0x64, and
; IRQ12). The poll reads 0x60/0x64 directly with IRQ12 left masked, so no IRQ12
; handler is installed.
;
; All drawing uses the gfx_* primitives from izbios-gfx.inc. Frames are four thin
; gfx_fill_rect edges (a picture-frame border), not CP437 line glyphs, so the
; geometry is free of the 8 px glyph grid and lines up with the 16 px row pitch.

; --- Field colors (default mode-13h palette) -------------------------------
%define BX_C_BORDER      0x07      ; light grey pane/button frame
%define BX_C_TITLE       0x0e      ; yellow heading
%define BX_C_LABEL       0x0f      ; white unfocused label
%define BX_C_BAND        0x0b      ; bright cyan focus band
%define BX_C_BANDTXT     0x00      ; black label on the cyan band
%define BX_C_DISABLED    0x08      ; dark grey disabled label / caption
%define BX_C_MARK        0x0b      ; bright cyan marker diamond
%define BX_C_HINT        0x07      ; light grey footer hint

; --- Layout (mode 13h pixels) ----------------------------------------------
%define BX_TITLE_X       8
%define BX_TITLE_Y       4

; Left pane (boot device): frame x 8..155, y 20..139.
%define BX_LP_X          8
%define BX_LP_Y          20
%define BX_LP_W          148
%define BX_LP_H          120
%define BX_LP_BAND_X     16        ; focus band / hit-cell left edge
%define BX_LP_BAND_W     132       ; hit-cell width
%define BX_LP_LABEL_X    24        ; device label column
%define BX_LP_MARK_X     132       ; device marker well
%define BX_DEV0_Y        44        ; HDD row top
%define BX_DEV_DY        20        ; device row pitch
%define BX_ROW_H         16        ; hit-cell / band height
%define BX_ROW_TXT_DY    4         ; label baseline offset inside a 16 px row

; Right pane (speed): frame x 164..311, y 20..139.
%define BX_RP_X          164
%define BX_RP_Y          20
%define BX_RP_W          148
%define BX_RP_H          120
%define BX_RP_BAND_X     172
%define BX_RP_BAND_W     132
%define BX_RP_LABEL_X    180       ; speed label column
%define BX_RP_MARK_X     296       ; speed marker well
%define BX_SPD0_Y        44        ; first speed row top
%define BX_SPD_DY        16        ; speed row pitch
; One cache caption in the free space below the four speed rows (which end at
; y=108). It updates to the focused speed; there is no per-row caption (those
; would crowd the 16 px rows), so no two text cells overlap.
%define BX_CACHE_X       172
%define BX_CACHE_Y       120

; Button bar: frame x 8..311, y 146..167.
%define BX_BB_X          8
%define BX_BB_Y          146
%define BX_BB_W          304
%define BX_BB_H          22
%define BX_ACC_X0        20        ; Accept hit-cell left
%define BX_ACC_W         130       ; Accept hit-cell width (20..150)
%define BX_CAN_X0        170       ; Cancel hit-cell left
%define BX_CAN_W         130       ; Cancel hit-cell width (170..300)
%define BX_BTN_Y0        150
%define BX_BTN_H         16
%define BX_ACC_TXT_X     62
%define BX_CAN_TXT_X     210
%define BX_BTN_TXT_Y     154

; Footer hints.
%define BX_HINT1_Y       172
%define BX_HINT2_Y       182

; Hit-test region ids.
%define BX_RGN_DEV0      0
%define BX_RGN_SPD0      3
%define BX_RGN_ACCEPT    7
%define BX_RGN_CANCEL    8
%define BX_RGN_NONE      0xff

; Scancodes (Set 1).
%define BX_SC_ESC        0x01
%define BX_SC_TAB        0x0f
%define BX_SC_ENTER      0x1c
%define BX_SC_F10        0x44
%define BX_SC_UP         0x48
%define BX_SC_LEFT       0x4b
%define BX_SC_RIGHT      0x4d
%define BX_SC_DOWN       0x50

; CP437 diamond used for the marker glyph.
%define BX_GLYPH_DIAMOND 0x04

; Device row indices (left pane), distinct from the BOOT_DEV_* codes.
%define BMX_RG_DEV_HDD   0
%define BMX_RG_DEV_FLPY  1
%define BMX_RG_DEV_CD    2

; ===========================================================================
; bootbox_menu: the menu entry point (the boot_menu shim jumps here). Enters mode
; 13h, seeds state, draws once, runs the input loop, returns to the caller
; (setup_maybe_enter) on Accept/Cancel. Clobbers all caller-saved registers.
; ===========================================================================
bootbox_menu:
    call bm_enter_mode13h           ; reuse izbios-boot.inc: mode 13h, index 0 black
    mov al, COLOR_BG
    call gfx_clear
    call bx2_seed
%if BMX_MOUSE
    call bx2_aux_init
%endif
    call bx2_draw
.loop:
%if BMX_MOUSE
    ; Interleave a non-blocking key peek with a mouse poll so the cursor moves
    ; even while no key is pressed.
    call bx2_poll_mouse
    call bx2_check_exit             ; a mouse click may have hit Accept/Cancel
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if the ring is empty
    jc .loop                        ; nothing waiting, keep polling the mouse
    xor ah, ah
    call kb_getkey                  ; consume the real key
%else
    xor ah, ah
    call kb_getkey                  ; blocking read: AH=scancode, AL=ascii
%endif
    call bx2_handle_key
    call bx2_check_exit             ; Accept/Cancel set the exit flag
    jmp .loop

; bx2_check_exit: if the exit flag is set, unwind the menu by popping the loop's
; return address and returning to bootbox_menu's caller (setup_maybe_enter). The
; loop calls this; on no-exit it just returns to the loop. DS = 0 on return.
bx2_check_exit:
    xor cx, cx
    mov ds, cx
    cmp byte [BMX_EXIT], 0
    je .stay
    ; Exit: drop this call's return address, then ret pops bootbox_menu's caller.
    add sp, 2
    ret
.stay:
    ret

; ===========================================================================
; State seeding.
; ===========================================================================
; bx2_seed: read 0xE1 (live mode) FIRST so the Accept reset compare is valid, then
; CMOS 0x11 (order) and 0x12 (speed). Leaves DS = 0. Clobbers AX, BX, CX.
bx2_seed:
    xor cx, cx
    mov ds, cx                      ; DS = 0 for the state block
    in al, PORT_LOTURA_MODE          ; live GSW mode, BEFORE any out 0xE1
    mov [BMX_LIVE_MODE], al
    call bx2_compute_avail          ; -> BMX_AVAIL (DS stays 0)
    ; Marked device from CMOS 0x11: 1 (disk-first) -> HDD if available, else Floppy.
    mov al, BOOT_CMOS_ORDER
    out 0x70, al
    in al, 0x71
    cmp al, 1
    jne .dev_floppy
    test byte [BMX_AVAIL], 0x01
    jz .dev_floppy
    mov al, BMX_RG_DEV_HDD
    jmp .dev_store
.dev_floppy:
    mov al, BMX_RG_DEV_FLPY
.dev_store:
    mov [BMX_MARK_DEV], al
    ; Marked speed from CMOS 0x12: 0->VSlow, 1->Slow, 2->Fast, other->Fast. Stored
    ; as a speed ROW index (0 Fast .. 3 SSlow).
    mov al, SU_NVRAM_GSW
    out 0x70, al
    in al, 0x71
    cmp al, 0
    je .spd_vslow
    cmp al, 1
    je .spd_slow
    mov al, BMX_SPD_FAST
    jmp .spd_store
.spd_slow:
    mov al, BMX_SPD_SLOW
    jmp .spd_store
.spd_vslow:
    mov al, BMX_SPD_VSLOW
.spd_store:
    mov [BMX_MARK_SPD], al
    ; Initial focus: device pane, on the marked device. Button-return pane = DEV.
    mov al, [BMX_MARK_DEV]
    mov [BMX_FOCUS_IDX], al
    mov byte [BMX_FOCUS_PANE], BMX_PANE_DEV
    mov byte [BMX_BTN_FROM], BMX_PANE_DEV
    mov byte [BMX_EXIT], 0
%if BMX_MOUSE
    mov word [BMX_CUR_X], 152       ; park the cursor mid-field
    mov word [BMX_CUR_Y], 92
    mov byte [BMX_LMB], 0
    mov byte [BMX_LAST_REGION], BX_RGN_NONE
    mov byte [BMX_PKT_IDX], 0
%endif
    ret

; bx2_compute_avail: set BMX_AVAIL (b0 HDD, b1 CD). No real-mode probe exists for
; C:/HDD or ATAPI presence today, so both are unavailable. This is the one routine
; to update when a host probe lands. DS = 0 on entry. Clobbers AX.
bx2_compute_avail:
    mov byte [BMX_AVAIL], 0
    ret

; bx2_dev_avail: AL = device row index. Returns AL = 1 if that device is markable,
; 0 if not. Floppy (row 1) is always available; HDD (0) and CD (2) follow
; BMX_AVAIL bits 0/1. DS = 0 on entry. Clobbers AX, BX.
bx2_dev_avail:
    cmp al, BMX_RG_DEV_FLPY
    je .yes
    cmp al, BMX_RG_DEV_HDD
    je .hdd
    ; CD-ROM (row 2): bit 1 of BMX_AVAIL.
    mov bl, [BMX_AVAIL]
    and bl, 0x02
    jz .no
    jmp .yes
.hdd:
    mov bl, [BMX_AVAIL]
    and bl, 0x01
    jz .no
.yes:
    mov al, 1
    ret
.no:
    xor al, al
    ret

; bx2_spd_avail: AL = speed row index. Returns AL = 1 if markable, 0 if not. All
; four rows (586/486/386/286) are available. Row 3 (286) maps to GswMode::Gsw286
; (port 0xE1 code 3). The reject path below stays as a guard for any future row
; that has no machine mode yet.
bx2_spd_avail:
    mov al, 1
    ret

; ===========================================================================
; Full repaint.
; ===========================================================================
bx2_draw:
    mov al, COLOR_BG
    call gfx_clear

    ; Title bar.
    push cs
    pop ds
    mov si, bx_str_title
    mov di, BX_TITLE_X
    mov ax, BX_TITLE_Y
    mov bl, BX_C_TITLE
    mov bh, COLOR_BG
    call gfx_text

    ; Left pane frame + title.
    mov di, BX_LP_X
    mov si, BX_LP_Y
    mov cx, BX_LP_W
    mov dx, BX_LP_H
    call bx2_frame
    push cs
    pop ds
    mov si, bx_str_dev_title
    mov di, BX_LP_X + 8
    mov ax, BX_LP_Y + 4
    mov bl, BX_C_TITLE
    mov bh, COLOR_BG
    call gfx_text

    ; Right pane frame + title.
    mov di, BX_RP_X
    mov si, BX_RP_Y
    mov cx, BX_RP_W
    mov dx, BX_RP_H
    call bx2_frame
    push cs
    pop ds
    mov si, bx_str_spd_title
    mov di, BX_RP_X + 8
    mov ax, BX_RP_Y + 4
    mov bl, BX_C_TITLE
    mov bh, COLOR_BG
    call gfx_text

    ; Device rows 0..2.
    xor cx, cx
.dev_rows:
    push cx
    mov al, cl
    call bx2_draw_dev_row
    pop cx
    inc cx
    cmp cx, BMX_DEV_COUNT
    jb .dev_rows

    ; Speed rows 0..3.
    xor cx, cx
.spd_rows:
    push cx
    mov al, cl
    call bx2_draw_spd_row
    pop cx
    inc cx
    cmp cx, BMX_SPD_COUNT
    jb .spd_rows

    ; Cache caption for the focused-or-marked speed.
    call bx2_draw_cache_caption

    ; Button bar.
    call bx2_draw_buttons

    ; Footer hints.
    push cs
    pop ds
    mov si, bx_str_hint1
    mov di, BX_TITLE_X
    mov ax, BX_HINT1_Y
    mov bl, BX_C_HINT
    mov bh, COLOR_BG
    call gfx_text
    mov si, bx_str_hint2
    mov di, BX_TITLE_X
    mov ax, BX_HINT2_Y
    mov bl, BX_C_HINT
    mov bh, COLOR_BG
    call gfx_text

%if BMX_MOUSE
    call bx2_cursor_save            ; save the background under the cursor
    call bx2_cursor_draw            ; then draw the cursor on top of the fresh frame
%endif
    ret

; ---------------------------------------------------------------------------
; bx2_frame: DI=x, SI=y, CX=w, DX=h. Four thin gfx_fill_rect edges in BX_C_BORDER.
; Stashes the geometry in CS scratch words so the four fills are simple. Preserves
; the caller's registers (pusha/popa).
; ---------------------------------------------------------------------------
bx2_frame:
    pusha
    xor ax, ax
    mov ds, ax                      ; DS = 0 for the RAM scratch words
    mov [BMX_DFX], di
    mov [BMX_DFY], si
    mov [BMX_DFW], cx
    mov [BMX_DFH], dx
    ; Top: x, y, w, 1.
    mov di, [BMX_DFX]
    mov si, [BMX_DFY]
    mov cx, [BMX_DFW]
    mov dx, 1
    mov bl, BX_C_BORDER
    call gfx_fill_rect
    ; Bottom: x, y+h-1, w, 1.
    mov di, [BMX_DFX]
    mov ax, [BMX_DFY]
    add ax, [BMX_DFH]
    dec ax
    mov si, ax
    mov cx, [BMX_DFW]
    mov dx, 1
    mov bl, BX_C_BORDER
    call gfx_fill_rect
    ; Left: x, y, 1, h.
    mov di, [BMX_DFX]
    mov si, [BMX_DFY]
    mov cx, 1
    mov dx, [BMX_DFH]
    mov bl, BX_C_BORDER
    call gfx_fill_rect
    ; Right: x+w-1, y, 1, h.
    mov ax, [BMX_DFX]
    add ax, [BMX_DFW]
    dec ax
    mov di, ax
    mov si, [BMX_DFY]
    mov cx, 1
    mov dx, [BMX_DFH]
    mov bl, BX_C_BORDER
    call gfx_fill_rect
    popa
    ret

; bx2_is_focused: AL = pane id, AH = row index. Returns AL = 1 if that pane+row is
; the current focus, else 0. Reads BMX_FOCUS_* from segment 0; leaves DS = 0.
; Clobbers AX, BX.
bx2_is_focused:
    mov bl, al                      ; BL = pane to test
    mov bh, ah                      ; BH = row to test
    xor ax, ax
    mov ds, ax
    mov al, [BMX_FOCUS_PANE]
    cmp al, bl
    jne .no
    mov al, [BMX_FOCUS_IDX]
    cmp al, bh
    jne .no
    mov al, 1
    ret
.no:
    xor al, al
    ret

; ---------------------------------------------------------------------------
; bx2_draw_dev_row: AL = device row index 0..2. Paints the hit cell with focus
; band, label (greyed when unavailable, inverse on the band), and the marker
; diamond when this is the marked device. DS bounces. Preserves caller regs.
; ---------------------------------------------------------------------------
bx2_draw_dev_row:
    pusha
    xor cx, cx
    mov ds, cx                      ; DS = 0 for the RAM scratch
    mov [BMX_DI], al                ; row index in RAM scratch
    mov byte [BMX_DFOCUSED], 0
    ; Row top Y = BX_DEV0_Y + idx*BX_DEV_DY.
    movzx ax, byte [BMX_DI]
    mov cx, BX_DEV_DY
    mul cx
    add ax, BX_DEV0_Y
    mov [BMX_DROWY], ax

    ; Focused row?
    mov al, BMX_PANE_DEV
    mov ah, [BMX_DI]
    call bx2_is_focused             ; AL = 1 if focused (DS = 0)
    test al, al
    jz .label_color
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DFOCUSED], 1
    mov di, BX_LP_BAND_X
    mov si, [BMX_DROWY]
    mov cx, BX_LP_BAND_W
    mov dx, BX_ROW_H
    mov bl, BX_C_BAND
    call gfx_fill_rect
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DCOLOR], BX_C_BANDTXT
    jmp .draw_label
.label_color:
    ; Off-band label color: white if available, dark grey if not.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_DI]
    call bx2_dev_avail              ; reads BMX_AVAIL; AL=1 available, 0 not
    xor cx, cx
    mov ds, cx
    test al, al
    jnz .label_white
    mov byte [BMX_DCOLOR], BX_C_DISABLED
    jmp .draw_label
.label_white:
    mov byte [BMX_DCOLOR], BX_C_LABEL
.draw_label:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_DI]
    call bx2_dev_label_ptr          ; DS:SI -> label (DS = CS)
    mov di, BX_LP_LABEL_X
    push ds                         ; keep DS = CS for gfx_text
    xor cx, cx
    mov ds, cx
    mov ax, [BMX_DROWY]
    mov bl, [BMX_DCOLOR]
    pop ds                          ; DS = CS (the label segment)
    add ax, BX_ROW_TXT_DY
    mov bh, COLOR_BG
    call gfx_text

    ; Marker diamond if this is the marked device.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_DEV]
    cmp al, [BMX_DI]
    jne .done
    mov byte [BMX_DGLYPH], BX_GLYPH_DIAMOND
    mov byte [BMX_DGLYPH + 1], 0
    mov si, BMX_DGLYPH              ; DS:SI -> 1-char ASCIIZ (DS = 0)
    mov di, BX_LP_MARK_X
    mov ax, [BMX_DROWY]
    add ax, BX_ROW_TXT_DY
    mov bl, BX_C_MARK               ; cyan off-band
    cmp byte [BMX_DFOCUSED], 0
    je .mark_draw
    mov bl, BX_C_BANDTXT            ; black on the cyan band
.mark_draw:
    mov bh, COLOR_BG
    call gfx_text                   ; DS = 0, SI -> BMX_DGLYPH
.done:
    popa
    ret

; ---------------------------------------------------------------------------
; bx2_draw_spd_row: AL = speed row index 0..3. Same structure as the device row,
; using the right pane geometry. DS bounces. Preserves caller regs.
; ---------------------------------------------------------------------------
bx2_draw_spd_row:
    pusha
    xor cx, cx
    mov ds, cx
    mov [BMX_DI], al
    mov byte [BMX_DFOCUSED], 0
    ; Row top Y = BX_SPD0_Y + idx*BX_SPD_DY.
    movzx ax, byte [BMX_DI]
    mov cx, BX_SPD_DY
    mul cx
    add ax, BX_SPD0_Y
    mov [BMX_DROWY], ax

    ; Focused row?
    mov al, BMX_PANE_SPD
    mov ah, [BMX_DI]
    call bx2_is_focused
    test al, al
    jz .label_color
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DFOCUSED], 1
    mov di, BX_RP_BAND_X
    mov si, [BMX_DROWY]
    mov cx, BX_RP_BAND_W
    mov dx, BX_ROW_H
    mov bl, BX_C_BAND
    call gfx_fill_rect
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DCOLOR], BX_C_BANDTXT
    jmp .draw_label
.label_color:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_DI]
    call bx2_spd_avail              ; AL=1 available, 0 not
    xor cx, cx
    mov ds, cx
    test al, al
    jnz .label_white
    mov byte [BMX_DCOLOR], BX_C_DISABLED
    jmp .draw_label
.label_white:
    mov byte [BMX_DCOLOR], BX_C_LABEL
.draw_label:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_DI]
    call bx2_spd_label_ptr          ; DS:SI -> label (DS = CS)
    mov di, BX_RP_LABEL_X
    push ds
    xor cx, cx
    mov ds, cx
    mov ax, [BMX_DROWY]
    mov bl, [BMX_DCOLOR]
    pop ds
    add ax, BX_ROW_TXT_DY
    mov bh, COLOR_BG
    call gfx_text

    ; Marker diamond if this is the marked speed.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_SPD]
    cmp al, [BMX_DI]
    jne .done
    mov byte [BMX_DGLYPH], BX_GLYPH_DIAMOND
    mov byte [BMX_DGLYPH + 1], 0
    mov si, BMX_DGLYPH
    mov di, BX_RP_MARK_X
    mov ax, [BMX_DROWY]
    add ax, BX_ROW_TXT_DY
    mov bl, BX_C_MARK
    cmp byte [BMX_DFOCUSED], 0
    je .mark_draw
    mov bl, BX_C_BANDTXT
.mark_draw:
    mov bh, COLOR_BG
    call gfx_text
.done:
    popa
    ret

; ---------------------------------------------------------------------------
; bx2_draw_cache_caption: one dark-grey caption under the speed rows giving the
; focused speed's L1/L2. If the focus is not on the speed pane, it shows the marked
; speed's cache instead. Clears the caption line first. DS bounces.
; ---------------------------------------------------------------------------
bx2_draw_cache_caption:
    pusha
    ; Wipe the caption line to background.
    push cs
    pop ds
    mov di, BX_CACHE_X
    mov si, BX_CACHE_Y
    mov cx, 128
    mov dx, 8
    mov bl, COLOR_BG
    call gfx_fill_rect
    ; Pick the speed row to describe: focused speed if the speed pane is focused,
    ; else the marked speed.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_PANE]
    cmp al, BMX_PANE_SPD
    jne .use_mark
    mov al, [BMX_FOCUS_IDX]
    jmp .have_row
.use_mark:
    mov al, [BMX_MARK_SPD]
.have_row:
    push cs
    pop ds
    call bx2_cache_ptr              ; DS:SI -> caption string for speed row AL
    mov di, BX_CACHE_X
    mov ax, BX_CACHE_Y
    mov bl, BX_C_DISABLED
    mov bh, COLOR_BG
    call gfx_text
    popa
    ret

; ---------------------------------------------------------------------------
; bx2_draw_buttons: draw the button-bar frame and the Accept/Cancel cells. The
; focused button gets a full-cell cyan band with a black label; the other shows a
; white label on the field. DS bounces.
; ---------------------------------------------------------------------------
bx2_draw_buttons:
    pusha
    ; Bar frame.
    mov di, BX_BB_X
    mov si, BX_BB_Y
    mov cx, BX_BB_W
    mov dx, BX_BB_H
    call bx2_frame

    ; Accept cell (button index 0).
    xor al, al
    call bx2_draw_one_button
    ; Cancel cell (button index 1).
    mov al, 1
    call bx2_draw_one_button
    popa
    ret

; bx2_draw_one_button: AL = button index (0 Accept, 1 Cancel). DS bounces.
bx2_draw_one_button:
    pusha
    xor cx, cx
    mov ds, cx
    mov [BMX_DI], al
    ; Is this button focused?
    mov al, [BMX_FOCUS_PANE]
    cmp al, BMX_PANE_BTN
    jne .not_focused
    mov al, [BMX_FOCUS_IDX]
    cmp al, [BMX_DI]
    jne .not_focused
    mov byte [BMX_DCOLOR], 1        ; focused flag
    jmp .paint
.not_focused:
    mov byte [BMX_DCOLOR], 0
.paint:
    ; Cell geometry by index (string pointer is a CS label).
    cmp byte [BMX_DI], 0
    jne .cancel_geom
    mov word [BMX_DFX], BX_ACC_X0
    mov word [BMX_DFW], BX_ACC_W
    mov word [BMX_DTX], BX_ACC_TXT_X
    mov si, bx_str_accept
    jmp .have_geom
.cancel_geom:
    mov word [BMX_DFX], BX_CAN_X0
    mov word [BMX_DFW], BX_CAN_W
    mov word [BMX_DTX], BX_CAN_TXT_X
    mov si, bx_str_cancel
.have_geom:
    mov [BMX_DSTRP], si
    ; Fill the cell: cyan band if focused, background otherwise.
    mov di, [BMX_DFX]
    mov si, BX_BTN_Y0
    mov cx, [BMX_DFW]
    mov dx, BX_BTN_H
    cmp byte [BMX_DCOLOR], 0
    je .fill_bg
    mov bl, BX_C_BAND
    jmp .do_fill
.fill_bg:
    mov bl, COLOR_BG
.do_fill:
    call gfx_fill_rect
    ; Label: DS = CS for the string bytes, geometry from RAM scratch.
    xor cx, cx
    mov ds, cx
    mov si, [BMX_DSTRP]
    mov di, [BMX_DTX]
    mov bl, BX_C_LABEL
    cmp byte [BMX_DCOLOR], 0
    je .label_draw
    mov bl, BX_C_BANDTXT
.label_draw:
    push cs
    pop ds                          ; DS = CS so DS:SI reaches the label
    mov ax, BX_BTN_TXT_Y
    mov bh, COLOR_BG
    call gfx_text
    popa
    ret

; ===========================================================================
; Label / caption pointer helpers. AL = row index on entry; return DS:SI -> the
; ASCIIZ string with DS = CS. Clobber AX, SI.
; ===========================================================================
bx2_dev_label_ptr:
    push cs
    pop ds
    cmp al, BMX_RG_DEV_HDD
    je .hdd
    cmp al, BMX_RG_DEV_FLPY
    je .flpy
    mov si, bx_str_dev_cd
    ret
.hdd:
    mov si, bx_str_dev_hdd
    ret
.flpy:
    mov si, bx_str_dev_flpy
    ret

bx2_spd_label_ptr:
    push cs
    pop ds
    cmp al, BMX_SPD_FAST
    je .fast
    cmp al, BMX_SPD_SLOW
    je .slow
    cmp al, BMX_SPD_VSLOW
    je .vslow
    mov si, bx_str_spd_sslow
    ret
.fast:
    mov si, bx_str_spd_fast
    ret
.slow:
    mov si, bx_str_spd_slow
    ret
.vslow:
    mov si, bx_str_spd_vslow
    ret

bx2_cache_ptr:
    push cs
    pop ds
    cmp al, BMX_SPD_FAST
    je .fast
    cmp al, BMX_SPD_SLOW
    je .slow
    cmp al, BMX_SPD_VSLOW
    je .vslow
    mov si, bx_str_cache_sslow
    ret
.fast:
    mov si, bx_str_cache_fast
    ret
.slow:
    mov si, bx_str_cache_slow
    ret
.vslow:
    mov si, bx_str_cache_vslow
    ret

; ===========================================================================
; Keyboard dispatch.
; ===========================================================================
; bx2_handle_key: AH = scancode (from kb_getkey). Routes to the per-pane handler.
; Repaints on focus/mark changes; Accept/Cancel never return here (they jump out
; of the loop). DS = whatever; helpers set their own. Clobbers caller-saved regs.
bx2_handle_key:
    ; Global keys first (work in any pane).
    cmp ah, BX_SC_ESC
    je bx2_cancel
    cmp ah, BX_SC_F10
    je bx2_accept
    ; Pane-specific.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_PANE]
    cmp al, BMX_PANE_DEV
    je .dev
    cmp al, BMX_PANE_SPD
    je .spd
    jmp bx2_key_btn                 ; BTN pane
.dev:
    jmp bx2_key_dev
.spd:
    jmp bx2_key_spd

; --- device pane keys ---
bx2_key_dev:
    cmp ah, BX_SC_UP
    je .up
    cmp ah, BX_SC_DOWN
    je .down
    cmp ah, BX_SC_RIGHT
    je .right
    cmp ah, BX_SC_TAB
    je .tab
    cmp ah, BX_SC_ENTER
    je .enter
    ret                             ; LEFT and others: no-op
.up:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    test al, al
    jnz .up_dec
    mov al, BMX_DEV_COUNT
.up_dec:
    dec al
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.down:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BMX_DEV_COUNT - 1
    jb .down_inc
    ; off the bottom -> button bar (Accept), remember DEV for Up.
    mov byte [BMX_FOCUS_PANE], BMX_PANE_BTN
    mov byte [BMX_BTN_FROM], BMX_PANE_DEV
    mov byte [BMX_FOCUS_IDX], 0
    jmp bx2_repaint
.down_inc:
    inc al
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.right:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BMX_SPD_COUNT - 1
    jbe .right_ok
    mov al, BMX_SPD_COUNT - 1
.right_ok:
    mov [BMX_FOCUS_IDX], al
    mov byte [BMX_FOCUS_PANE], BMX_PANE_SPD
    jmp bx2_repaint
.tab:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_SPD]
    mov [BMX_FOCUS_IDX], al
    mov byte [BMX_FOCUS_PANE], BMX_PANE_SPD
    jmp bx2_repaint
.enter:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    call bx2_dev_avail
    test al, al
    jz .reject
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    mov [BMX_MARK_DEV], al
    jmp bx2_repaint
.reject:
    ; Unavailable device: show the matching note, do not move the mark.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BMX_RG_DEV_HDD
    je .note_hdd
    ; CD-ROM.
    push cs
    pop ds
    mov si, bm_str_cd_msg
    call bx2_note
    ret
.note_hdd:
    push cs
    pop ds
    mov si, bm_str_disk_msg
    call bx2_note
    ret

; --- speed pane keys ---
bx2_key_spd:
    cmp ah, BX_SC_UP
    je .up
    cmp ah, BX_SC_DOWN
    je .down
    cmp ah, BX_SC_LEFT
    je .left
    cmp ah, BX_SC_TAB
    je .tab
    cmp ah, BX_SC_ENTER
    je .enter
    ret                             ; RIGHT and others: no-op
.up:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    test al, al
    jnz .up_dec
    mov al, BMX_SPD_COUNT
.up_dec:
    dec al
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.down:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BMX_SPD_COUNT - 1
    jb .down_inc
    mov byte [BMX_FOCUS_PANE], BMX_PANE_BTN
    mov byte [BMX_BTN_FROM], BMX_PANE_SPD
    mov byte [BMX_FOCUS_IDX], 0
    jmp bx2_repaint
.down_inc:
    inc al
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.left:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BMX_DEV_COUNT - 1
    jbe .left_ok
    mov al, BMX_DEV_COUNT - 1
.left_ok:
    mov [BMX_FOCUS_IDX], al
    mov byte [BMX_FOCUS_PANE], BMX_PANE_DEV
    jmp bx2_repaint
.tab:
    xor cx, cx
    mov ds, cx
    mov byte [BMX_FOCUS_PANE], BMX_PANE_BTN
    mov byte [BMX_BTN_FROM], BMX_PANE_SPD
    mov byte [BMX_FOCUS_IDX], 0
    jmp bx2_repaint
.enter:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    call bx2_spd_avail
    test al, al
    jz .reject
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    mov [BMX_MARK_SPD], al
    jmp bx2_repaint
.reject:
    push cs
    pop ds
    mov si, bx_str_286_msg
    call bx2_note
    ret

; --- button pane keys ---
bx2_key_btn:
    cmp ah, BX_SC_LEFT
    je .left
    cmp ah, BX_SC_RIGHT
    je .right
    cmp ah, BX_SC_UP
    je .up
    cmp ah, BX_SC_TAB
    je .tab
    cmp ah, BX_SC_ENTER
    je .enter
    ret                             ; DOWN and others: no-op
.left:
    xor cx, cx
    mov ds, cx
    mov byte [BMX_FOCUS_IDX], 0     ; Accept
    jmp bx2_repaint
.right:
    xor cx, cx
    mov ds, cx
    mov byte [BMX_FOCUS_IDX], 1     ; Cancel
    jmp bx2_repaint
.up:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_BTN_FROM]
    mov [BMX_FOCUS_PANE], al
    cmp al, BMX_PANE_DEV
    jne .up_spd
    mov al, [BMX_MARK_DEV]
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.up_spd:
    mov al, [BMX_MARK_SPD]
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.tab:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    test al, al
    jnz .tab_to_dev
    mov byte [BMX_FOCUS_IDX], 1     ; Accept -> Cancel
    jmp bx2_repaint
.tab_to_dev:
    mov byte [BMX_FOCUS_PANE], BMX_PANE_DEV
    mov al, [BMX_MARK_DEV]
    mov [BMX_FOCUS_IDX], al
    jmp bx2_repaint
.enter:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    test al, al
    jz bx2_accept
    jmp bx2_cancel

; bx2_repaint: tail-called by the per-key handlers. Repaints and returns to the
; loop (so it returns to bx2_handle_key's caller).
bx2_repaint:
    call bx2_draw
    ret

; bx2_note: like bm_note but at the bootbox footer row. Wipe both hint lines, draw
; the ASCIIZ at CS:SI on hint line 1. The next repaint restores the hints.
bx2_note:
    pusha
    push si
    push cs
    pop ds
    mov di, BX_TITLE_X
    mov si, BX_HINT1_Y
    mov cx, 304
    mov dx, 16                      ; cover both hint lines (y 172..187)
    mov bl, COLOR_BG
    call gfx_fill_rect
    pop si
    push cs
    pop ds
    mov di, BX_TITLE_X
    mov ax, BX_HINT1_Y
    mov bl, BX_C_HINT
    mov bh, COLOR_BG
    call gfx_text
    popa
    ret

; ===========================================================================
; Accept / Cancel.
; ===========================================================================
; bx2_accept: commit the marked device and speed, then exit. Mirrors su_commit for
; the CMOS/checksum/0xE1 writes. The exit either falls through into boot_entry (no
; speed change) or cold-resets (speed changed). Reached as a tail target from the
; key handlers, so it never returns to bx2_handle_key.
bx2_accept:
    ; Map the marked speed row to the 0xE1/CMOS code (real hardware encoding:
    ; 586=2, 486=1, 386=0, 286=3). Stash the code in RAM scratch so su_cmos_checksum
    ; (which clobbers AX/BX/CX/DX) cannot lose it.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_SPD]
    call bx2_spd_row_to_code        ; AL row -> AL code
    xor cx, cx
    mov ds, cx
    mov [BMX_DCODE], al
%if BMX_MOUSE
    call bx2_aux_disable            ; quiet the AUX channel before leaving
%endif
    ; Apply the mode live (pending_mode; identical to su_commit's out 0xE1).
    xor cx, cx
    mov ds, cx
    mov al, [BMX_DCODE]
    out PORT_LOTURA_MODE, al
    ; Persist speed to CMOS 0x12.
    mov al, [BMX_DCODE]
    mov ah, SU_NVRAM_GSW
    call su_cmos_write
    ; Persist device order to CMOS 0x11: HDD -> 1 (disk-first), else 0.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_DEV]
    cmp al, BMX_RG_DEV_HDD
    jne .order_floppy
    mov al, 1
    jmp .order_store
.order_floppy:
    xor al, al
.order_store:
    mov ah, BOOT_CMOS_ORDER
    call su_cmos_write
    ; Recompute the NVRAM checksum (covers 0x11 and 0x12).
    call su_cmos_checksum
    ; Session boot device into BOOT_CHOICE.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_DEV]
    cmp al, BMX_RG_DEV_HDD
    je .choice_disk
    cmp al, BMX_RG_DEV_CD
    je .choice_cd
    mov byte [BOOT_CHOICE], BOOT_DEV_FLOPPY
    jmp .decide
.choice_disk:
    mov byte [BOOT_CHOICE], BOOT_DEV_DISK
    jmp .decide
.choice_cd:
    mov byte [BOOT_CHOICE], BOOT_DEV_CDROM
.decide:
    ; Reset only if the speed actually changed; otherwise fall through to boot.
    xor cx, cx
    mov ds, cx
    mov al, [BMX_LIVE_MODE]
    cmp al, [BMX_DCODE]
    jne .reset
    ; No change: flag the menu loop to exit, returning into boot_entry.
    mov byte [BMX_EXIT], 1
    ret
.reset:
    cli
    jmp ROM_SEG:reset

; bx2_cancel: discard all edits. Write nothing to 0xE1 or CMOS. Set BOOT_CHOICE to
; floppy so boot_entry runs the default order floppy -> HDD -> CD-ROM, then return.
bx2_cancel:
%if BMX_MOUSE
    call bx2_aux_disable
%endif
    xor cx, cx
    mov ds, cx
    mov byte [BOOT_CHOICE], BOOT_DEV_FLOPPY
    mov byte [BMX_EXIT], 1
    ret

; bx2_spd_row_to_code: AL = speed row index -> AL = the 0xE1/CMOS GSW code. Real
; hardware encoding (gsw_mode_from_code): Fast(586)=2, Slow(486)=1, VSlow(386)=0,
; SSlow(286)=3. Any out-of-range row falls back to VSlow (0).
bx2_spd_row_to_code:
    cmp al, BMX_SPD_FAST
    je .fast
    cmp al, BMX_SPD_SLOW
    je .slow
    cmp al, BMX_SPD_VSLOW
    je .vslow
    cmp al, BMX_SPD_SSLOW
    je .sslow
    ; Out of range -> fall back to VSlow (386).
    xor al, al
    ret
.fast:
    mov al, 2
    ret
.slow:
    mov al, 1
    ret
.vslow:
    xor al, al
    ret
.sslow:
    mov al, 3
    ret

%if BMX_MOUSE
; ===========================================================================
; Mouse layer (item 8). Polled PS/2 AUX cursor and click hit-testing. The 8042
; AUX channel and IRQ12 live in the machine model; this code matches its exact
; semantics (keyboard.rs). IRQ12 is left masked at the PIC, so this is poll-only
; and installs no IRQ12 handler.
; ===========================================================================

; bx2_aux_init: enable the mouse once on entry. The "read command byte" reply is
; delivered through the keyboard output buffer and arms IRQ1, so the keyboard ISR
; would otherwise steal that byte (and worse, interpret it as a scancode) before
; we read it. Mask IRQ1 at the PIC and run with interrupts off across the whole
; handshake, then restore. Set command-byte bit 1 (IRQ12) alongside bit 0, enable
; AUX data reporting (0xD4, 0xF4), and drain the ACK. Clobbers AX.
bx2_aux_init:
    pushf
    cli
    ; Mask IRQ1 at the master PIC so the CCB reply does not reach INT 09h.
    in al, PORT_PIC_DATA
    push ax                         ; save the live mask
    or al, 0x02                     ; set bit 1 (mask IRQ1)
    out PORT_PIC_DATA, al
    ; Read the current controller command byte.
    mov al, KBC_READ_CCB
    out PORT_KBD_CMD, al
    call bx2_aux_wait_obf
    in al, PORT_KBD_DATA            ; AL = current command byte
    ; Set the IRQ1 and IRQ12 enable bits; leave the rest as the controller had it.
    or al, KBC_CMD_IRQ1 | KBC_CMD_IRQ12
    mov ah, al                      ; AH = new command byte
    mov al, KBC_WRITE_CCB
    out PORT_KBD_CMD, al
    mov al, ah
    out PORT_KBD_DATA, al           ; write the new command byte
    ; Enable AUX data reporting: route 0xF4 to the mouse.
    mov al, KBC_WRITE_AUX
    out PORT_KBD_CMD, al
    mov al, PS2_ENABLE_REPORT
    out PORT_KBD_DATA, al
    ; Drain the AUX ACK (0xFA) so it does not sit in the output buffer.
    call bx2_aux_drain
    ; Restore the live IRQ1 mask and the caller's interrupt flag.
    pop ax                          ; AL = saved mask
    out PORT_PIC_DATA, al
    popf
    ret

; bx2_aux_disable: disable AUX reporting on the way out so the controller is quiet
; (the keyboard ISR reads 0x60 without an AUX check). Mask IRQ1 and run with
; interrupts off so the ACK cannot be raced by INT 09h, then restore. Clobbers AX.
bx2_aux_disable:
    pushf
    cli
    in al, PORT_PIC_DATA
    push ax
    or al, 0x02
    out PORT_PIC_DATA, al
    mov al, KBC_WRITE_AUX
    out PORT_KBD_CMD, al
    mov al, PS2_DISABLE_REPORT
    out PORT_KBD_DATA, al
    call bx2_aux_drain
    pop ax
    out PORT_PIC_DATA, al
    popf
    ret

; bx2_aux_wait_obf: spin (bounded) until the controller output buffer is full.
; Clobbers AX, CX.
bx2_aux_wait_obf:
    mov cx, 0xffff
.spin:
    in al, PORT_KBD_CMD
    test al, KBC_STATUS_OBF
    jnz .ready
    loop .spin
.ready:
    ret

; bx2_aux_drain: while an AUX byte is waiting (OBF set and AUX bit set), read and
; discard it. Bounded. Clobbers AX, CX.
bx2_aux_drain:
    mov cx, 16
.spin:
    in al, PORT_KBD_CMD
    mov ah, al
    test al, KBC_STATUS_OBF
    jz .done
    test ah, KBC_STATUS_AUX
    jz .done                        ; a keyboard byte: leave it for INT 09h
    in al, PORT_KBD_DATA            ; discard the AUX byte
    loop .spin
.done:
    ret

; bx2_poll_mouse: read at most one waiting AUX byte and, when a full 3-byte packet
; is assembled, apply the motion and the left-button edge. Reads 0x64; if no AUX
; byte is waiting, returns at once. DS bounces. Clobbers caller-saved regs.
bx2_poll_mouse:
    in al, PORT_KBD_CMD
    mov ah, al
    test al, KBC_STATUS_OBF
    jz .ret                         ; nothing waiting
    test ah, KBC_STATUS_AUX
    jz .ret                         ; a keyboard byte: leave it for the key path
    in al, PORT_KBD_DATA            ; AL = the AUX byte
    ; Assemble into BMX_PKT[BMX_PKT_IDX].
    xor cx, cx
    mov ds, cx
    mov bl, [BMX_PKT_IDX]
    test bl, bl
    jnz .store                      ; not the sync byte: store wherever we are
    ; Byte 0 must have bit 3 set (sync); if not, drop and resync.
    test al, 0x08
    jz .ret
.store:
    movzx bx, bl
    mov [BMX_PKT + bx], al
    inc bl
    mov [BMX_PKT_IDX], bl
    cmp bl, 3
    jb .ret
    ; Full packet: reset the index and act on it.
    mov byte [BMX_PKT_IDX], 0
    call bx2_apply_packet
.ret:
    ret

; bx2_apply_packet: consume BMX_PKT (flags, dx, dy). Update the cursor with motion
; (clamped), hot-track focus by hit-test on the new position, and fire a click on
; the left-button rising edge. DS = 0 on entry. Clobbers caller-saved regs.
bx2_apply_packet:
    xor cx, cx
    mov ds, cx
    ; --- X delta (signed) ---
    mov al, [BMX_PKT + 1]           ; dx byte
    movsx bx, al                    ; BX = sign-extended dx
    mov al, [BMX_PKT]               ; flags
    test al, 0x40                   ; X overflow -> ignore this axis delta
    jz .x_ok
    xor bx, bx
.x_ok:
    mov ax, [BMX_CUR_X]
    add ax, bx
    ; clamp 0..BMX_CUR_X_MAX
    cmp ax, 0
    jge .x_lo_ok
    xor ax, ax
.x_lo_ok:
    cmp ax, BMX_CUR_X_MAX
    jle .x_hi_ok
    mov ax, BMX_CUR_X_MAX
.x_hi_ok:
    mov [BMX_CUR_X], ax
    ; --- Y delta (PS/2 up-positive; screen y is down-positive, so subtract) ---
    mov al, [BMX_PKT + 2]           ; dy byte
    movsx bx, al
    mov al, [BMX_PKT]
    test al, 0x80                   ; Y overflow -> ignore
    jz .y_ok
    xor bx, bx
.y_ok:
    mov ax, [BMX_CUR_Y]
    sub ax, bx
    cmp ax, 0
    jge .y_lo_ok
    xor ax, ax
.y_lo_ok:
    cmp ax, BMX_CUR_Y_MAX
    jle .y_hi_ok
    mov ax, BMX_CUR_Y_MAX
.y_hi_ok:
    mov [BMX_CUR_Y], ax

    ; Move the cursor sprite: restore the old background, save the new background,
    ; redraw. Pure motion does NOT repaint the panes.
    call bx2_cursor_restore
    call bx2_cursor_save
    call bx2_cursor_draw

    ; Hot-track focus by hit-test.
    call bx2_hit_test               ; AL = region id (or NONE)
    mov bl, al
    xor cx, cx
    mov ds, cx
    cmp bl, BX_RGN_NONE
    je .no_focus
    cmp bl, [BMX_LAST_REGION]
    je .no_focus                    ; same region as last frame: skip the repaint
    mov [BMX_LAST_REGION], bl
    mov al, bl
    call bx2_focus_from_region      ; set focus_pane/idx from the region id
    call bx2_draw
    jmp .check_click
.no_focus:
    mov bl, al
    xor cx, cx
    mov ds, cx
    mov [BMX_LAST_REGION], bl
.check_click:
    ; Left-button rising edge?
    xor cx, cx
    mov ds, cx
    mov al, [BMX_PKT]
    and al, 0x01                    ; current LMB
    mov ah, [BMX_LMB]
    mov [BMX_LMB], al
    test al, al
    jz .ret                         ; button up now: no click
    test ah, ah
    jnz .ret                        ; was already down: not a new edge
    ; Rising edge: act on the region under the cursor.
    call bx2_hit_test
    call bx2_click_region
.ret:
    ret

; bx2_focus_from_region: AL = region id (DEV0..2, SPD0..3, ACCEPT, CANCEL). Sets
; BMX_FOCUS_PANE/IDX accordingly (hover focus). DS = 0 on entry. Clobbers AX.
bx2_focus_from_region:
    xor cx, cx
    mov ds, cx
    cmp al, BX_RGN_SPD0
    jb .dev                         ; 0..2 device
    cmp al, BX_RGN_ACCEPT
    jb .spd                         ; 3..6 speed
    je .accept                      ; 7 accept
    ; 8 cancel
    mov byte [BMX_FOCUS_PANE], BMX_PANE_BTN
    mov byte [BMX_FOCUS_IDX], 1
    ret
.accept:
    mov byte [BMX_FOCUS_PANE], BMX_PANE_BTN
    mov byte [BMX_FOCUS_IDX], 0
    ret
.dev:
    mov byte [BMX_FOCUS_PANE], BMX_PANE_DEV
    mov [BMX_FOCUS_IDX], al
    ret
.spd:
    sub al, BX_RGN_SPD0
    mov byte [BMX_FOCUS_PANE], BMX_PANE_SPD
    mov [BMX_FOCUS_IDX], al
    ret

; bx2_click_region: AL = region id under the cursor on a left click. A click is
; "focus then act": mark a device/speed (if available) or commit on a button.
; DS = 0 on entry. Tail-jumps to bx2_accept/bx2_cancel for the buttons.
bx2_click_region:
    cmp al, BX_RGN_NONE
    je .ret
    cmp al, BX_RGN_CANCEL
    je bx2_cancel
    cmp al, BX_RGN_ACCEPT
    je bx2_accept
    cmp al, BX_RGN_SPD0
    jb .dev                         ; 0..2 device
    ; speed 3..6
    sub al, BX_RGN_SPD0
    push ax
    call bx2_spd_avail
    test al, al
    pop bx                          ; BL = speed row
    jz .spd_reject
    xor cx, cx
    mov ds, cx
    mov [BMX_MARK_SPD], bl
    call bx2_draw
.ret:
    ret
.spd_reject:
    push cs
    pop ds
    mov si, bx_str_286_msg
    call bx2_note
    ret
.dev:
    push ax                         ; AL = device row
    call bx2_dev_avail
    test al, al
    pop bx                          ; BL = device row
    jz .dev_reject
    xor cx, cx
    mov ds, cx
    mov [BMX_MARK_DEV], bl
    call bx2_draw
    ret
.dev_reject:
    cmp bl, BMX_RG_DEV_HDD
    je .note_hdd
    push cs
    pop ds
    mov si, bm_str_cd_msg
    call bx2_note
    ret
.note_hdd:
    push cs
    pop ds
    mov si, bm_str_disk_msg
    call bx2_note
    ret

; bx2_hit_test: read BMX_CUR_X/Y and return AL = the region id whose rect contains
; the point, or BX_RGN_NONE. Scans the flat CS rect table (read with a cs: prefix,
; so DS does not matter during the scan). Clobbers AX, BX, CX, DX, SI; sets DS = 0.
bx2_hit_test:
    xor cx, cx
    mov ds, cx
    mov bx, [BMX_CUR_X]             ; BX = x
    mov dx, [BMX_CUR_Y]             ; DX = y
    mov si, bx2_hit_table
.scan:
    mov ax, [cs:si]                 ; x0
    cmp ax, 0xffff
    je .none                        ; sentinel: end of table
    cmp bx, ax
    jl .next
    mov ax, [cs:si + 2]             ; x1
    cmp bx, ax
    jg .next
    mov ax, [cs:si + 4]             ; y0
    cmp dx, ax
    jl .next
    mov ax, [cs:si + 6]             ; y1
    cmp dx, ax
    jg .next
    mov al, [cs:si + 8]             ; region id
    ret
.next:
    add si, 10                      ; 5 words per entry
    jmp .scan
.none:
    mov al, BX_RGN_NONE
    ret

; ---------------------------------------------------------------------------
; Cursor save/restore-under and draw (16x16). Save buffer at BMX_SAVE (segment 0).
; The X/Y clamps keep the 16x16 block fully on screen, so a fixed copy is safe.
; ---------------------------------------------------------------------------
; bx2_cursor_save: copy the 16x16 framebuffer block at (BMX_CUR_X,BMX_CUR_Y) into
; BMX_SAVE. DS = framebuffer, ES = segment 0. Clobbers regs.
bx2_cursor_save:
    pusha
    push es
    push ds
    xor ax, ax
    mov es, ax                      ; ES = 0 (save buffer)
    mov ax, VGA13_SEG
    mov ds, ax                      ; DS = framebuffer
    push ds
    xor ax, ax
    mov ds, ax
    mov bx, [BMX_CUR_X]
    mov bp, [BMX_CUR_Y]
    pop ds                          ; DS = framebuffer again
    mov di, BMX_SAVE                ; ES:DI = save buffer
    mov cx, BMX_CUR_H               ; rows
.row:
    push cx
    ; source offset = y*320 + x
    mov ax, bp
    mov dx, 320
    mul dx
    add ax, bx
    mov si, ax                      ; DS:SI = framebuffer row start
    mov cx, BMX_CUR_W
    rep movsb                       ; copy 16 bytes to ES:DI (DI advances)
    inc bp                          ; next scanline
    pop cx
    loop .row
    pop ds
    pop es
    popa
    ret

; bx2_cursor_restore: copy BMX_SAVE back to the 16x16 framebuffer block. ES =
; framebuffer, DS = segment 0. Clobbers regs.
bx2_cursor_restore:
    pusha
    push es
    push ds
    mov ax, VGA13_SEG
    mov es, ax                      ; ES = framebuffer
    xor ax, ax
    mov ds, ax                      ; DS = 0 (save buffer)
    mov bx, [BMX_CUR_X]
    mov bp, [BMX_CUR_Y]
    mov si, BMX_SAVE                ; DS:SI = save buffer
    mov cx, BMX_CUR_H
.row:
    push cx
    mov ax, bp
    mov dx, 320
    mul dx
    add ax, bx
    mov di, ax                      ; ES:DI = framebuffer row start
    mov cx, BMX_CUR_W
    rep movsb
    inc bp
    pop cx
    loop .row
    pop ds
    pop es
    popa
    ret

; bx2_cursor_draw: blit the 16x16 1-bpp arrow sprite at the cursor position in
; white, set bits only (transparent clear bits). Uses gfx_blit_1bpp. Clobbers regs.
bx2_cursor_draw:
    pusha
    xor cx, cx
    mov ds, cx
    mov di, [BMX_CUR_X]
    mov bp, [BMX_CUR_Y]
    push cs
    pop ds                          ; DS = CS for the sprite bytes
    mov si, bx2_cursor_sprite
    mov ax, bp                      ; AX = y
    mov cx, BMX_CUR_W               ; width in pixels
    mov dx, BMX_CUR_H               ; height in rows
    mov bl, BX_C_LABEL              ; white
    mov bh, BMX_CUR_STRIDE          ; bytes per source row
    call gfx_blit_1bpp
    popa
    ret

; A 16x16 top-left arrow, 1 bpp, 2 bytes per source row, MSB of byte 0 = leftmost
; pixel (gfx_blit_1bpp reads byte = col>>3, MSB first). The hot tip is the
; top-left pixel, matching the (X,Y) test point in bx2_hit_test.
bx2_cursor_sprite:
    db 0x80, 0x00   ; row 0:  X
    db 0xc0, 0x00   ; row 1:  XX
    db 0xe0, 0x00   ; row 2:  XXX
    db 0xf0, 0x00   ; row 3:  XXXX
    db 0xf8, 0x00   ; row 4:  XXXXX
    db 0xfc, 0x00   ; row 5:  XXXXXX
    db 0xfe, 0x00   ; row 6:  XXXXXXX
    db 0xff, 0x00   ; row 7:  XXXXXXXX
    db 0xf8, 0x00   ; row 8:  XXXXX
    db 0xdc, 0x00   ; row 9:  XX XXX
    db 0x8e, 0x00   ; row 10: X   XXX
    db 0x0e, 0x00   ; row 11:     XXX
    db 0x07, 0x00   ; row 12:      XXX
    db 0x07, 0x00   ; row 13:      XXX
    db 0x03, 0x80   ; row 14:       XXX
    db 0x00, 0x00   ; row 15: (blank)

; bx2_hit_table: the flat hit-test rectangle table, one {x0, x1, y0, y1, id}
; entry (5 words) per clickable region, terminated by an x0 = 0xFFFF sentinel.
; The rects are the exact Section 3 geometry, so the visual and hit areas cannot
; drift apart. Right pane is x 172..303, device pane x 16..147.
bx2_hit_table:
    ; Device rows (left pane hit cells x 16..147).
    dw 16, 147, 44,  59,  BX_RGN_DEV0      ; Hard Disk
    dw 16, 147, 64,  79,  BX_RGN_DEV0 + 1  ; Floppy A:
    dw 16, 147, 84,  99,  BX_RGN_DEV0 + 2  ; CD-ROM
    ; Speed rows (right pane hit cells x 172..303).
    dw 172, 303, 44,  59,  BX_RGN_SPD0      ; Fast 586
    dw 172, 303, 60,  75,  BX_RGN_SPD0 + 1  ; Slow 486
    dw 172, 303, 76,  91,  BX_RGN_SPD0 + 2  ; VSlow 386
    dw 172, 303, 92,  107, BX_RGN_SPD0 + 3  ; SSlow 286
    ; Buttons.
    dw BX_ACC_X0, BX_ACC_X0 + BX_ACC_W - 1, BX_BTN_Y0, BX_BTN_Y0 + BX_BTN_H - 1, BX_RGN_ACCEPT
    dw BX_CAN_X0, BX_CAN_X0 + BX_CAN_W - 1, BX_BTN_Y0, BX_BTN_Y0 + BX_BTN_H - 1, BX_RGN_CANCEL
    dw 0xffff      ; end-of-table sentinel
%endif

; The per-draw scratch (row index, colors, frame geometry, marker buffer, button
; geometry) lives in segment-0 RAM (BMX_D* in izbios-defs.inc), not the ROM code
; segment: a mov into a CS label is dropped by the read-only ROM bus, so CS-local
; scratch silently never updates. RAM scratch is the only correct place for it.

; ===========================================================================
; Strings (ASCIIZ).
; ===========================================================================
bx_str_title:      db 'IZARRA 3000 - BOOT & SPEED', 0
bx_str_dev_title:  db 'Boot Device', 0
bx_str_spd_title:  db 'Speed', 0
bx_str_dev_hdd:    db 'Hard Disk', 0
bx_str_dev_flpy:   db 'Floppy A:', 0
bx_str_dev_cd:     db 'CD-ROM', 0
bx_str_spd_fast:   db 'Fast   586', 0
bx_str_spd_slow:   db 'Slow   486', 0
bx_str_spd_vslow:  db 'VSlow  386', 0
bx_str_spd_sslow:  db 'SSlow  286', 0
bx_str_cache_fast: db 'Cache L1 32K L2 512K', 0
bx_str_cache_slow: db 'Cache L1 16K L2 128K', 0
bx_str_cache_vslow:db 'Cache L1 0   L2 64K', 0
bx_str_cache_sslow:db 'Cache L1 0   L2 0', 0
bx_str_accept:     db 'Accept', 0
bx_str_cancel:     db 'Cancel', 0
bx_str_286_msg:    db '286 mode not available', 0
%if BMX_MOUSE
bx_str_hint1:      db 'Click/arrows move  Enter mark  Tab pane', 0
%else
bx_str_hint1:      db 'Arrows move  Enter mark  Tab pane', 0
%endif
bx_str_hint2:      db 'Accept boots marked  Cancel default order', 0
