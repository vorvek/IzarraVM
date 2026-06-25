; izbios-bootbox.inc - STREAM E (Phase 4 section 8): the boot-device + CPU-speed
; menu, rendered on the proprietary Margo LFB (mode 0x150) inside the boot-box
; sprite from izbios-art.inc. boot_menu (izbios-boot.inc) is a one-line shim that
; jumps to bootbox_menu here.
;
; The box (art_bootbox_rle, BOOTBOX_X/Y/W/H = 19,57,181,177) is too narrow for the
; old side-by-side panes, so the menu is a single stacked column: three device
; rows, four CPU-speed rows, and an Accept row, walked by a flat focus index 0..7
; (0..2 device, 3..6 speed, 7 Accept). The menu is modal -- there is no Cancel
; button -- but Esc bails to the default boot order so an accidental Tab is not a
; trap. F10 or Enter-on-Accept commits.
;
; Drawing uses the lfb_* primitives, which share the mode-13h gfx_* register
; contract, so the menu wears the POST screen's art palette: ART_FIELD_INDEX field,
; ART_INK_INDEX ink, ART_RED_INDEX title, ART_GREY_INDEX disabled rows. The LFB
; mode is already active from the POST screen, so the menu just blits the box over
; it (the wordmark and mascot on the right stay visible, matching the boot art).
; Accept applies the speed live (port 0xE1) and persists order/speed to CMOS, then
; falls through to boot_entry at the chosen speed with no cold reset.

; --- Colours (art palette indices) -----------------------------------------
%define BX_C_FIELD     ART_FIELD_INDEX
%define BX_C_INK       ART_INK_INDEX
%define BX_C_TITLE     ART_RED_INDEX
%define BX_C_DISABLED  ART_GREY_INDEX

; --- Stacked layout inside the box (LFB pixels) ----------------------------
%define BX_TITLE_X     28
%define BX_TITLE_Y     64
%define BX_DEVLBL_Y    80
%define BX_DEV0_Y      92
%define BX_DEV_DY      12
%define BX_SPDLBL_Y    132
%define BX_SPD0_Y      144
%define BX_SPD_DY      12
%define BX_ACC_Y       202
%define BX_HINT_Y      218
%define BX_BAND_X      26
%define BX_BAND_W      158
%define BX_BAND_H      11
%define BX_LABEL_X     34
%define BX_MARK_X      172
%define BX_TXT_DY      2          ; glyph offset inside the band

; Flat focus indices.
%define BX_FOCUS_SPD0    3
%define BX_FOCUS_ACCEPT  7
%define BX_FOCUS_MAX     7

; Scancodes (Set 1).
%define BX_SC_ESC      0x01
%define BX_SC_ENTER    0x1c
%define BX_SC_F10      0x44
%define BX_SC_UP       0x48
%define BX_SC_DOWN     0x50

%define BX_GLYPH_DIAMOND 0x04

; Device row indices.
%define BMX_RG_DEV_HDD   0
%define BMX_RG_DEV_FLPY  1
%define BMX_RG_DEV_CD    2

; ===========================================================================
; bootbox_menu: entry point (the boot_menu shim jumps here). Seeds from CMOS,
; draws the box, runs a blocking key loop, and returns to setup_maybe_enter on
; Accept or Esc (both set BMX_EXIT). Clobbers all caller-saved registers.
; ===========================================================================
bootbox_menu:
    call bx_seed
    call bx_draw_full               ; box + static chrome + rows (once)
.loop:
    xor ah, ah
    call kb_getkey                  ; blocking read: AH=scancode, AL=ascii
    call bx_handle_key              ; updates state; sets BMX_EXIT on Accept/Esc
    xor cx, cx
    mov ds, cx
    cmp byte [BMX_EXIT], 0
    jne .done
    call bx_redraw_rows             ; only the rows change per key; the box stays
    jmp .loop
.done:
    ret

; ===========================================================================
; State seeding: read CMOS 0x11 (order) and 0x12 (speed) into the marks, and
; place initial focus on the marked device. Leaves DS = 0. Clobbers AX, BX, CX.
; ===========================================================================
bx_seed:
    xor cx, cx
    mov ds, cx
    ; No boot-menu probe for HDD/CD presence, so only the floppy is selectable.
    mov byte [BMX_AVAIL], 0
    ; Marked device from CMOS 0x11: 1 (disk-first) -> HDD if available, else floppy.
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
    ; Marked speed from CMOS 0x12 (GSW code) mapped to a speed row.
    mov al, SU_NVRAM_GSW
    out 0x70, al
    in al, 0x71
    call bx_code_to_row
    mov [BMX_MARK_SPD], al
    ; Initial focus: the marked device row.
    mov al, [BMX_MARK_DEV]
    mov [BMX_FOCUS_IDX], al
    mov byte [BMX_EXIT], 0
    ret

; ===========================================================================
; Keyboard dispatch. AH = scancode. Up/Down move the flat focus (wrapping);
; Enter acts on the focused row (mark a device/speed, or Accept); F10 accepts;
; Esc bails. Mark/move handlers just return so the loop repaints. Accept/Esc set
; BMX_EXIT. DS bounces; clobbers caller-saved regs.
; ===========================================================================
bx_handle_key:
    cmp ah, BX_SC_ESC
    je bx_cancel
    cmp ah, BX_SC_F10
    je bx_accept
    cmp ah, BX_SC_UP
    je .up
    cmp ah, BX_SC_DOWN
    je .down
    cmp ah, BX_SC_ENTER
    je .enter
    ret
.up:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    test al, al
    jnz .up_dec
    mov al, BX_FOCUS_MAX + 1
.up_dec:
    dec al
    mov [BMX_FOCUS_IDX], al
    ret
.down:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BX_FOCUS_MAX
    jb .down_inc
    xor al, al
    jmp .down_store
.down_inc:
    inc al
.down_store:
    mov [BMX_FOCUS_IDX], al
    ret
.enter:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    cmp al, BX_FOCUS_ACCEPT
    je bx_accept
    cmp al, BX_FOCUS_SPD0
    jae .mark_spd
    ; device row (focus 0..2 == device row): mark it if available.
    call bx_dev_avail               ; AL row -> AL 1/0 (clobbers AX, BX)
    test al, al
    jz .ret                         ; unavailable: leave the mark unchanged
    xor cx, cx
    mov ds, cx
    mov al, [BMX_FOCUS_IDX]
    mov [BMX_MARK_DEV], al
.ret:
    ret
.mark_spd:
    ; speed row: focus 3..6 -> speed row 0..3.
    sub al, BX_FOCUS_SPD0
    mov [BMX_MARK_SPD], al
    ret

; ===========================================================================
; Accept: apply the marked speed live (port 0xE1), persist order/speed to CMOS
; with a fresh checksum, set BOOT_CHOICE, and flag the loop to exit. Mirrors the
; old bx2_accept / su_commit. Reached as a tail target; sets BMX_EXIT and returns.
; ===========================================================================
bx_accept:
    xor cx, cx
    mov ds, cx
    mov al, [BMX_MARK_SPD]
    call bx_spd_row_to_code         ; AL row -> AL GSW code
    xor cx, cx
    mov ds, cx
    mov [BMX_DCODE], al             ; stash (su_cmos_* clobber AX/BX/CX/DX)
    ; Live mode switch.
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
    jmp .exit
.choice_disk:
    mov byte [BOOT_CHOICE], BOOT_DEV_DISK
    jmp .exit
.choice_cd:
    mov byte [BOOT_CHOICE], BOOT_DEV_CDROM
.exit:
    mov byte [BMX_EXIT], 1
    ret

; bx_cancel: Esc bail. Write nothing to 0xE1/CMOS; set BOOT_CHOICE to floppy so
; boot_entry runs the default order, then flag exit.
bx_cancel:
    xor cx, cx
    mov ds, cx
    mov byte [BOOT_CHOICE], BOOT_DEV_FLOPPY
    mov byte [BMX_EXIT], 1
    ret

; ===========================================================================
; Full repaint: blit the box over the POST screen, then draw the title, the two
; section labels, the device/speed rows, the Accept row, and the footer hint.
; ===========================================================================
; bx_draw_full: blit the box and the static chrome (title, section labels, hint),
; then draw the rows. Called once on entry. The per-key loop calls bx_redraw_rows
; instead, leaving the costly box blit alone (it never changes).
bx_draw_full:
    ; Box sprite (frame + cream interior) over the live POST screen.
    push cs
    pop ds
    mov si, art_bootbox_rle
    mov di, BOOTBOX_X
    mov ax, BOOTBOX_Y
    mov cx, BOOTBOX_W
    mov dx, BOOTBOX_H
    call lfb_blit_rle
    ; Clear the POST-screen sliver that sticks out left of the box (x 0..18) so the
    ; menu reads as a clean boot screen. The box itself (now x 19..230, through the
    ; shadow) covers the rest of the POST text and the icon row; the gap before the
    ; mascot stays cream. Stays below the wordmark (y>=64).
    mov di, 0
    mov si, 64
    mov cx, BOOTBOX_X
    mov dx, 170
    mov bl, BX_C_FIELD
    call lfb_fill_rect
    ; Title (red).
    push cs
    pop ds
    mov si, bx_str_title
    mov di, BX_TITLE_X
    mov ax, BX_TITLE_Y
    mov bl, BX_C_TITLE
    mov bh, BX_C_FIELD
    call lfb_text
    ; "Boot device:" section label (ink).
    push cs
    pop ds
    mov si, bx_str_dev_title
    mov di, BX_TITLE_X
    mov ax, BX_DEVLBL_Y
    mov bl, BX_C_INK
    mov bh, BX_C_FIELD
    call lfb_text
    ; "CPU speed:" section label (ink).
    push cs
    pop ds
    mov si, bx_str_spd_title
    mov di, BX_TITLE_X
    mov ax, BX_SPDLBL_Y
    mov bl, BX_C_INK
    mov bh, BX_C_FIELD
    call lfb_text
    ; Footer hint.
    push cs
    pop ds
    mov si, bx_str_hint
    mov di, BX_TITLE_X
    mov ax, BX_HINT_Y
    mov bl, BX_C_DISABLED
    mov bh, BX_C_FIELD
    call lfb_text
    ; The selectable rows (these change with focus and marks).
    call bx_redraw_rows
    ret

; bx_redraw_rows: redraw the three device rows, the four speed rows, and Accept.
; Called by bx_draw_full and after every key. Each row clears its own band, so the
; per-key path skips the costly box re-blit and only touches the rows.
bx_redraw_rows:
    xor cx, cx
.dev_rows:
    push cx
    mov al, cl
    call bx_draw_dev_row
    pop cx
    inc cx
    cmp cx, BMX_DEV_COUNT
    jb .dev_rows
    xor cx, cx
.spd_rows:
    push cx
    mov al, cl
    call bx_draw_spd_row
    pop cx
    inc cx
    cmp cx, BMX_SPD_COUNT
    jb .spd_rows
    call bx_draw_accept
    ret

; ---------------------------------------------------------------------------
; bx_draw_item: draw one selectable row from segment-0 scratch the caller fills:
;   BMX_DI    = this row's flat focus index (0..7)
;   BMX_DROWY = row top Y
;   BMX_DCOLOR= 1 if this row is the marked one (draw the diamond), else 0
;   BMX_DBG   = 1 if available (ink label), 0 if disabled (grey label)
;   BMX_DSTRP = CS-relative label pointer
; Focused rows get an ink band with inverse (field) text; others sit on the field.
; DS bounces; preserves caller registers.
; ---------------------------------------------------------------------------
bx_draw_item:
    pusha
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DFOCUSED], 0
    mov al, [BMX_FOCUS_IDX]
    cmp al, [BMX_DI]
    jne .bg_field
    ; Focused: ink band.
    mov byte [BMX_DFOCUSED], 1
    mov di, BX_BAND_X
    mov si, [BMX_DROWY]
    mov cx, BX_BAND_W
    mov dx, BX_BAND_H
    mov bl, BX_C_INK
    call lfb_fill_rect
    jmp .label
.bg_field:
    ; Unfocused: clear the band to the field (erases an old focus bar/marker).
    xor cx, cx
    mov ds, cx
    mov di, BX_BAND_X
    mov si, [BMX_DROWY]
    mov cx, BX_BAND_W
    mov dx, BX_BAND_H
    mov bl, BX_C_FIELD
    call lfb_fill_rect
.label:
    ; Pick label fg/bg: focused -> field on ink; disabled -> grey on field;
    ; available -> ink on field.
    xor cx, cx
    mov ds, cx
    cmp byte [BMX_DFOCUSED], 0
    jne .focused_colors
    mov bh, BX_C_FIELD              ; bg = field
    mov bl, BX_C_INK               ; fg = ink (available)
    cmp byte [BMX_DBG], 0
    jne .have_label_colors
    mov bl, BX_C_DISABLED          ; disabled -> grey
    jmp .have_label_colors
.focused_colors:
    mov bh, BX_C_INK               ; bg = ink band
    mov bl, BX_C_FIELD             ; fg = field (inverse)
.have_label_colors:
    mov ax, [BMX_DROWY]            ; read y while DS = 0
    add ax, BX_TXT_DY
    mov si, [BMX_DSTRP]           ; CS-relative label pointer
    push cs
    pop ds                         ; DS = CS for the label bytes
    mov di, BX_LABEL_X
    call lfb_text                  ; DI=x, AX=y, BL=fg, BH=bg, DS:SI=label
    ; Marker diamond for the marked row.
    xor cx, cx
    mov ds, cx
    cmp byte [BMX_DCOLOR], 0
    je .done
    mov byte [BMX_DGLYPH], BX_GLYPH_DIAMOND
    mov byte [BMX_DGLYPH + 1], 0
    mov bh, BX_C_FIELD
    mov bl, BX_C_INK
    cmp byte [BMX_DFOCUSED], 0
    je .mark_draw
    mov bh, BX_C_INK               ; inverse: field diamond on the ink band
    mov bl, BX_C_FIELD
.mark_draw:
    mov ax, [BMX_DROWY]
    add ax, BX_TXT_DY
    mov si, BMX_DGLYPH             ; DS:SI = 0:glyph
    mov di, BX_MARK_X
    call lfb_text
.done:
    popa
    ret

; bx_draw_dev_row: AL = device row 0..2 (its flat focus index is the same).
bx_draw_dev_row:
    pusha
    xor cx, cx
    mov ds, cx
    mov [BMX_DI], al
    movzx ax, byte [BMX_DI]
    mov cx, BX_DEV_DY
    mul cx
    add ax, BX_DEV0_Y
    mov [BMX_DROWY], ax
    ; Marked?
    mov al, [BMX_DI]
    cmp al, [BMX_MARK_DEV]
    jne .not_marked
    mov byte [BMX_DCOLOR], 1
    jmp .avail
.not_marked:
    mov byte [BMX_DCOLOR], 0
.avail:
    mov al, [BMX_DI]
    call bx_dev_avail
    xor cx, cx
    mov ds, cx
    mov [BMX_DBG], al
    mov al, [BMX_DI]
    call bx_dev_label_ptr           ; DS:SI = CS:label
    xor cx, cx
    mov ds, cx
    mov [BMX_DSTRP], si
    call bx_draw_item
    popa
    ret

; bx_draw_spd_row: AL = speed row 0..3 (flat focus index = row + 3).
bx_draw_spd_row:
    pusha
    xor cx, cx
    mov ds, cx
    mov [BMX_DGLYPH], al            ; stash the speed row (free until bx_draw_item)
    add al, BX_FOCUS_SPD0
    mov [BMX_DI], al
    movzx ax, byte [BMX_DGLYPH]
    mov cx, BX_SPD_DY
    mul cx
    add ax, BX_SPD0_Y
    mov [BMX_DROWY], ax
    mov al, [BMX_DGLYPH]
    cmp al, [BMX_MARK_SPD]
    jne .not_marked
    mov byte [BMX_DCOLOR], 1
    jmp .avail
.not_marked:
    mov byte [BMX_DCOLOR], 0
.avail:
    mov byte [BMX_DBG], 1           ; every speed is available
    mov al, [BMX_DGLYPH]
    call bx_spd_label_ptr
    xor cx, cx
    mov ds, cx
    mov [BMX_DSTRP], si
    call bx_draw_item
    popa
    ret

; bx_draw_accept: the Accept row (focus index 7), no marker, always available.
bx_draw_accept:
    pusha
    xor cx, cx
    mov ds, cx
    mov byte [BMX_DI], BX_FOCUS_ACCEPT
    mov word [BMX_DROWY], BX_ACC_Y
    mov byte [BMX_DCOLOR], 0
    mov byte [BMX_DBG], 1
    mov word [BMX_DSTRP], bx_str_accept
    call bx_draw_item
    popa
    ret

; ===========================================================================
; Helpers: availability, label pointers, and the speed-row <-> GSW-code maps.
; ===========================================================================
; bx_dev_avail: AL = device row -> AL = 1 if markable, 0 if not. Floppy (1) is
; always available; HDD (0) and CD (2) follow BMX_AVAIL bits 0/1. DS = 0 on entry.
bx_dev_avail:
    cmp al, BMX_RG_DEV_FLPY
    je .yes
    cmp al, BMX_RG_DEV_HDD
    je .hdd
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

; bx_dev_label_ptr: AL = device row -> DS:SI = ASCIIZ (DS = CS). Clobbers AX, SI.
bx_dev_label_ptr:
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

; bx_spd_label_ptr: AL = speed row -> DS:SI = ASCIIZ (DS = CS). Clobbers AX, SI.
bx_spd_label_ptr:
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

; bx_spd_row_to_code: AL = speed row -> AL = 0xE1/CMOS GSW code (Fast/586=2,
; Slow/486=1, VSlow/386=0, SSlow/286=3). Out-of-range falls back to VSlow (0).
bx_spd_row_to_code:
    cmp al, BMX_SPD_FAST
    je .fast
    cmp al, BMX_SPD_SLOW
    je .slow
    cmp al, BMX_SPD_SSLOW
    je .sslow
    xor al, al                      ; VSlow / default
    ret
.fast:
    mov al, 2
    ret
.slow:
    mov al, 1
    ret
.sslow:
    mov al, 3
    ret

; bx_code_to_row: AL = GSW code -> AL = speed row. Inverse of bx_spd_row_to_code.
bx_code_to_row:
    cmp al, 2
    je .fast
    cmp al, 1
    je .slow
    cmp al, 3
    je .sslow
    mov al, BMX_SPD_VSLOW           ; code 0 or other -> 386
    ret
.fast:
    mov al, BMX_SPD_FAST
    ret
.slow:
    mov al, BMX_SPD_SLOW
    ret
.sslow:
    mov al, BMX_SPD_SSLOW
    ret

; ===========================================================================
; Strings.
; ===========================================================================
bx_str_title:     db 'Boot & Speed', 0
bx_str_dev_title: db 'Boot device:', 0
bx_str_spd_title: db 'CPU speed:', 0
bx_str_dev_hdd:   db 'Hard Disk', 0
bx_str_dev_flpy:  db 'Floppy A:', 0
bx_str_dev_cd:    db 'CD-ROM', 0
bx_str_spd_fast:  db 'Fast   586', 0
bx_str_spd_slow:  db 'Slow   486', 0
bx_str_spd_vslow: db 'VSlow  386', 0
bx_str_spd_sslow: db 'SSlow  286', 0
bx_str_accept:    db 'Accept', 0
bx_str_hint:      db 'Up/Dn  Enter  Esc', 0
