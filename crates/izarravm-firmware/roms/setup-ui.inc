; setup-ui.inc - STREAM D: the mode-13h setup page. Owns this file and the
; SETUP_SCRATCH region (segment 0, 256 bytes). Core calls setup_maybe_enter after
; run_post and before enter_setup_or_boot.
;
; Flow: peek the keyboard for a brief window. If the setup hotkey (Del, scancode
; SETUP_HOTKEY) is not waiting, return and let the BIOS boot. If it is, switch to
; mode 13h and draw a menu of rows: a heading, static info rows, one editable
; CHOICE row for the GSW compatibility mode, and Save / Discard action rows.
; Up/Down move the highlight over the selectable rows only (the heading and info
; rows are skipped). Left/Right or Enter edit the CHOICE in a working copy held in
; SETUP_SCRATCH; active_mode does not change until Save. Save (F10 or Enter on the
; Save row) writes the chosen code to PORT_LOTURA_MODE: that is the live Lotura
; switch, and it takes effect on the next instruction. Discard (Esc or Enter on the
; Discard row) leaves the active mode untouched. Both exits restore boot.
;
; The GSW mode shown and edited here is read live from PORT_LOTURA_MODE, which
; returns the active mode code (0=386, 1=486, 2=586). The other info rows are
; static labels: parsing the VDTS result block by name in 16-bit code would couple
; this page to other streams' record formats and grow the shared ROM, and the one
; value the page actually changes (the mode) is available straight from hardware.

; --- SETUP_SCRATCH layout (segment 0) --------------------------------------
; +0  working_mode : the edited GSW mode code (0/1/2), seeded from the live mode.
; +1  cursor       : index of the highlighted selectable row (0..SETUP_SEL_COUNT-1).
%define SU_WORKING_MODE  SETUP_SCRATCH + 0
%define SU_CURSOR        SETUP_SCRATCH + 1

; Selectable rows, in highlight order.
%define SU_ROW_CHOICE    0
%define SU_ROW_SAVE      1
%define SU_ROW_DISCARD   2
%define SU_SEL_COUNT     3

; Set 1 make codes for the navigation and action keys. The scan2ascii table only
; covers the main typing block, so these are matched by scancode (AH), not ASCII.
%define SC_ESC           0x01
%define SC_ENTER         0x1c
%define SC_F10           0x44
%define SC_UP            0x48
%define SC_DOWN          0x50
%define SC_LEFT          0x4b
%define SC_RIGHT         0x4d

; Hotkey poll budget: how many peek iterations to wait for Del before booting. The
; window is short on purpose; a headless run with no key falls straight through. An
; injected key is already queued when the window opens, so a few hundred iterations
; is ample for IRQ1 to deliver it.
%define SU_HOTKEY_SPINS  0x0200

; Menu layout (mode 13h pixels). Rows start below the title and step by one line.
%define SU_MENU_X        16
%define SU_MENU_Y        40
%define SU_ROW_DY        12
%define SU_VALUE_X       176
%define SU_COLOR_HEAD    0x0e        ; yellow
%define SU_COLOR_INFO    0x07        ; light grey
%define SU_COLOR_ROW     0x0f        ; white (unselected selectable row)
%define SU_COLOR_HILITE  0x0b        ; bright cyan (highlighted row)

; setup_maybe_enter: the core hook. Returns with no side effects unless the hotkey
; is pressed during the poll window.
setup_maybe_enter:
    call su_wait_hotkey
    jc .enter                       ; CF=1 -> Del was waiting
    ret
.enter:
    call su_run_menu
    ret

; su_wait_hotkey: peek the keyboard ring for the setup hotkey for a bounded spin.
; Returns CF=1 if Del (SETUP_HOTKEY) was seen (and consumed), CF=0 otherwise. Any
; other key seen during the window is consumed and ignored so it does not leak into
; the boot path. Clobbers AX, CX.
su_wait_hotkey:
    mov cx, SU_HOTKEY_SPINS
.spin:
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if empty
    jc .next
    cmp ah, SETUP_HOTKEY
    je .hit
    ; some other key: drain it and keep watching for Del.
    push cx
    xor ah, ah
    call kb_getkey
    pop cx
    jmp .next
.next:
    loop .spin
    clc                             ; window elapsed, no hotkey
    ret
.hit:
    xor ah, ah
    call kb_getkey                  ; drain the Del make from the ring
    stc
    ret

; su_run_menu: draw the page, seed the working copy from the live mode, then loop
; reading keys until Save or Discard. On Save, write the chosen mode to the Lotura
; live register. Clobbers all caller-saved registers (the sequencer saves none here
; because core calls this directly, but nothing after it depends on our registers).
su_run_menu:
    ; Seed the working mode from the live Lotura register and reset the cursor.
    in al, PORT_LOTURA_MODE
    cmp al, 2
    jbe .seed_ok
    xor al, al                      ; clamp an unexpected readback to 386
.seed_ok:
    xor cx, cx
    mov ds, cx                      ; DS = 0 for the scratch region
    mov bx, SU_WORKING_MODE
    mov [bx], al                    ; register-indexed store (the CPU has no AL moffs form)
    mov byte [SU_CURSOR], SU_ROW_CHOICE
    ; Do not flush here: su_wait_hotkey already drained the Del, and any key the
    ; user has queued for the menu may already sit in the ring. Flushing would
    ; race those legitimate presses.
    call su_draw
.loop:
    xor ah, ah
    call kb_getkey                  ; blocking read: AH=scancode, AL=ascii
    cmp ah, SC_UP
    je .up
    cmp ah, SC_DOWN
    je .down
    cmp ah, SC_LEFT
    je .dec
    cmp ah, SC_RIGHT
    je .inc
    cmp ah, SC_F10
    je .save
    cmp ah, SC_ESC
    je .discard
    cmp ah, SC_ENTER
    je .enter
    jmp .loop
.up:
    call su_cursor_dec
    call su_draw
    jmp .loop
.down:
    call su_cursor_inc
    call su_draw
    jmp .loop
.dec:
    call su_choice_on_cursor
    jne .loop                       ; Left edits only when the CHOICE row is active
    call su_mode_dec
    call su_draw
    jmp .loop
.inc:
    call su_choice_on_cursor
    jne .loop
    call su_mode_inc
    call su_draw
    jmp .loop
.enter:
    ; Enter acts on the highlighted row: edit the CHOICE, else Save / Discard.
    xor cx, cx
    mov ds, cx
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, SU_ROW_CHOICE
    je .enter_choice
    cmp al, SU_ROW_SAVE
    je .save
    jmp .discard                    ; SU_ROW_DISCARD
.enter_choice:
    call su_mode_inc                ; Enter on the CHOICE cycles it forward
    call su_draw
    jmp .loop
.save:
    xor cx, cx
    mov ds, cx
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    out PORT_LOTURA_MODE, al        ; live Lotura switch; applies on the next instruction
    call su_writeback               ; persistence hook (currently a stub)
    ret
.discard:
    ret                             ; active_mode untouched

; su_cursor_dec / su_cursor_inc: move the highlight, wrapping across the selectable
; rows. DS is set to 0 here. Clobbers AX.
su_cursor_dec:
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    test al, al
    jnz .ok
    mov al, SU_SEL_COUNT
.ok:
    dec al
    mov [bx], al
    ret
su_cursor_inc:
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    inc al
    cmp al, SU_SEL_COUNT
    jb .ok
    xor al, al
.ok:
    mov [bx], al
    ret

; su_choice_on_cursor: ZF=1 iff the CHOICE row is highlighted. Sets DS=0. Clobbers AX, BX.
su_choice_on_cursor:
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, SU_ROW_CHOICE
    ret

; su_mode_dec / su_mode_inc: cycle the working mode through 0..2. DS=0. Clobbers
; AX, BX.
su_mode_dec:
    xor ax, ax
    mov ds, ax
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    test al, al
    jnz .ok
    mov al, 3
.ok:
    dec al
    mov [bx], al
    ret
su_mode_inc:
    xor ax, ax
    mov ds, ax
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    inc al
    cmp al, 3
    jb .ok
    xor al, al
.ok:
    mov [bx], al
    ret

; su_writeback: persistence hook. The Izarra 3000 has no battery-backed CMOS, so a
; saved mode survives only until the next power cycle (the live Lotura write above
; is the whole effect). A future config-store backend would persist the choice
; here. Stub for now: no-op.
su_writeback:
    ret

; su_draw: paint the whole page from scratch. Clears to background, draws the
; title, the info rows, the CHOICE row with its current value, and the action rows,
; highlighting whichever selectable row the cursor sits on. Sets DS=CS to reach the
; ROM strings; reads SU_CURSOR / SU_WORKING_MODE from segment 0 as needed.
su_draw:
    call set_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds                          ; DS=CS for the label strings

    ; Title.
    mov si, su_str_title
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY * 2
    mov bl, SU_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text

    ; Static info rows.
    mov si, su_str_info_mem
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    mov si, su_str_info_video
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    mov si, su_str_info_audio
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text

    ; CHOICE row label, highlighted when the cursor selects it.
    mov bl, SU_ROW_CHOICE
    call su_row_color               ; -> BL = label color for this selectable row
    mov si, su_str_cpu
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 3
    mov bh, COLOR_BG
    call gfx_text
    ; CHOICE value, drawn next to the label.
    call su_draw_mode_value

    ; Action rows.
    mov bl, SU_ROW_SAVE
    call su_row_color
    mov si, su_str_save
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 5
    mov bh, COLOR_BG
    call gfx_text
    mov bl, SU_ROW_DISCARD
    call su_row_color
    mov si, su_str_discard
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 6
    mov bh, COLOR_BG
    call gfx_text
    ret

; su_row_color: BL = selectable-row index on entry; returns BL = the highlight color
; if that row is the cursor, else the plain row color. Restores DS=CS for the
; caller's gfx_text. Clobbers AX, CX.
su_row_color:
    push cx
    mov cl, bl                      ; save the input row index across the DS:BX load
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, cl
    mov bl, SU_COLOR_ROW
    jne .done
    mov bl, SU_COLOR_HILITE
.done:
    pop cx
    push cs
    pop ds                          ; restore DS=CS for the caller's gfx_text string
    ret

; su_draw_mode_value: draw the working mode's name (386/486/586) at the CHOICE
; value column. Reads SU_WORKING_MODE from segment 0; uses DS=CS for the string.
su_draw_mode_value:
    push cx
    xor cx, cx
    mov ds, cx
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    push cs
    pop ds
    cmp al, 0
    je .m386
    cmp al, 1
    je .m486
    mov si, su_str_mode586
    jmp .put
.m386:
    mov si, su_str_mode386
    jmp .put
.m486:
    mov si, su_str_mode486
.put:
    mov di, SU_VALUE_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 3
    mov bl, SU_COLOR_HILITE
    mov bh, COLOR_BG
    call gfx_text
    pop cx
    ret

su_str_title:      db 'IZARRA 3000 SETUP', 0
su_str_info_mem:   db 'MEMORY: SEE POST', 0
su_str_info_video: db 'VIDEO: MARGO VGA', 0
su_str_info_audio: db 'AUDIO: RESONIQUE', 0
su_str_cpu:        db 'CPU MODE:', 0
su_str_save:       db 'SAVE (F10)', 0
su_str_discard:    db 'DISCARD (ESC)', 0
su_str_mode386:    db '386', 0
su_str_mode486:    db '486', 0
su_str_mode586:    db '586', 0
