; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Margo-LFB setup page. Uses the
; SETUP_SCRATCH region (segment 0, 256 bytes). Core calls setup_maybe_enter after
; run_post and before enter_setup_or_boot.
;
; Flow: peek the keyboard for a brief window. If the setup hotkey (Del, scancode
; SETUP_HOTKEY) is not waiting, return and let the BIOS boot. If it is, switch to
; the Margo LFB mode (0x150, 320x240x8 - the graphical POST's own mode) and draw
; the main menu inside a bordered box, matching the POST screen's palette. The
; menu is navigated by Up/Down (highlight), Left/Right (edit the inline rows:
; keyboard layout and GSW mode), Enter (open a sub-page or fire a Save/Discard
; action), and Esc (back out, treated as Discard on the main menu). A one-line
; context-sensitive help string under the box changes with the highlighted row.
;
; The page edits a working copy held in SETUP_SCRATCH and seeded at entry from the
; live hardware and CMOS NVRAM. Nothing is committed until Save: Save writes the
; CMOS NVRAM area (layout 0x10, GSW default 0x12), refreshes the 0x2E/0x2F
; checksum, sets the RTC registers when the time was edited, and writes the GSW
; default to the live Lotura mode register (PORT_LOTURA_MODE). Discard leaves all
; of that untouched. Both exits return to the boot path.
;
; The peripherals page re-runs the POST probe routines live (they are
; non-destructive) and shows each as PASS/FAIL in a boxed table with an aligned
; status column.

; --- SETUP_SCRATCH layout (segment 0) --------------------------------------
%define SU_WORKING_MODE   SETUP_SCRATCH + 0    ; edited GSW mode (0/1/2/3)
%define SU_WORKING_LAYOUT SETUP_SCRATCH + 1    ; edited keyboard layout (0..16)
%define SU_CURSOR         SETUP_SCRATCH + 2    ; highlighted main-menu row
%define SU_TIME_FIELD     SETUP_SCRATCH + 3    ; highlighted time field (0..5)
%define SU_TIME_EDITED    SETUP_SCRATCH + 4    ; 1 once the time was changed
%define SU_TIME_HOUR      SETUP_SCRATCH + 5    ; working clock, binary
%define SU_TIME_MIN       SETUP_SCRATCH + 6
%define SU_TIME_SEC       SETUP_SCRATCH + 7
%define SU_TIME_DAY       SETUP_SCRATCH + 8
%define SU_TIME_MONTH     SETUP_SCRATCH + 9
%define SU_TIME_YEAR      SETUP_SCRATCH + 10   ; two-digit year (0..99)

; Main-menu selectable rows, in highlight order.
%define SU_ROW_TIME       0
%define SU_ROW_LAYOUT     1
%define SU_ROW_GSW        2
%define SU_ROW_PERIPH     3
%define SU_ROW_REPAIR     4
%define SU_ROW_SAVE       5
%define SU_ROW_DISCARD    6
%define SU_SEL_COUNT      7

; Time sub-page fields, in Left/Right edit order.
%define SU_TF_HOUR        0
%define SU_TF_MIN         1
%define SU_TF_SEC         2
%define SU_TF_DAY         3
%define SU_TF_MONTH       4
%define SU_TF_YEAR        5
%define SU_TF_COUNT       6

; CMOS register indices (MC146818 ports 0x70/0x71). The clock is binary 24-hour.
%define SU_RTC_INDEX      0x70
%define SU_RTC_DATA       0x71
%define SU_RTC_SEC        0x00
%define SU_RTC_MIN        0x02
%define SU_RTC_HOUR       0x04
%define SU_RTC_DAY        0x07
%define SU_RTC_MONTH      0x08
%define SU_RTC_YEAR       0x09

; CMOS NVRAM bytes (the Izarra general area; see the RTC device map).
; SU_NVRAM_GSW (0x12) lives in izbios-defs.inc: the POST bring-up applies it too.
%define SU_NVRAM_LAYOUT   0x10

; Number of selectable keyboard layouts (US/UK/SP/FR/GR/IT/BE/CF/DK/NL/NO/PO/SF/SG/SU/SV/LA).
; The active table selection lives in CMOS 0x10; the layout-switching ISR is a separate
; stream, so this page only persists the index and names the choice.
%define SU_LAYOUT_COUNT   17

; Set 1 make codes for the navigation and action keys.
%define SC_ESC            0x01
%define SC_ENTER          0x1c
%define SC_F10            0x44
%define SC_UP             0x48
%define SC_DOWN           0x50
%define SC_LEFT           0x4b
%define SC_RIGHT          0x4d

; Hotkey poll budget: how many peek iterations to wait for Del before booting.
%define SU_HOTKEY_SPINS   0x0200

; --- Colours (art palette indices; the setup page shares the POST screen's
; palette so it reads as one continuous system, like the Tab boot menu does).
%define SU_C_FIELD        ART_FIELD_INDEX   ; cream field / interior
%define SU_C_INK           ART_INK_INDEX     ; near-black body text / border
%define SU_C_TITLE         ART_RED_INDEX     ; red title bar text
%define SU_C_DISABLED      ART_GREY_INDEX    ; mid-grey info / help text
%define SU_C_PASS          10                ; sage-green PASS (art_palette[10])
%define SU_C_FAIL          ART_RED_INDEX     ; red FAIL

; --- Menu layout (LFB pixels, 320x240 field) --------------------------------
%define SU_BOX_X          8
%define SU_BOX_Y          16
%define SU_BOX_W          304
%define SU_BOX_H          192
%define SU_TITLE_X        8
%define SU_TITLE_Y         4
%define SU_MENU_X         18
%define SU_MENU_Y         26
%define SU_ROW_DY         12
%define SU_VALUE_X        180
%define SU_HELP1_Y        212
%define SU_HELP2_Y         226
%define SU_HELP_X          8

; su_enter_setup_lfb: install the Margo LFB mode (0x150) and load the art
; palette. The POST screen already left the LFB mode active and the palette
; loaded in the normal boot path, so this is a belt-and-braces call: it makes
; setup_maybe_enter safe to call from any screen state, not just fresh POST.
; No DAC hack is needed here (unlike the old mode-13h path) because the art
; palette is exactly what this page wants.
su_enter_setup_lfb:
    call lfb_set_mode
    call lfb_load_palette
    ret

; setup_maybe_enter: the core hook. During the POST poll window it watches for two
; hotkeys: Del opens the setup page, Tab opens the boot-device menu. Returns with no
; side effects if neither key is pressed during the window.
setup_maybe_enter:
    call su_wait_hotkey
    jnc .done                       ; CF=0 -> no hotkey, just boot
    test al, al
    jnz .boot_menu                  ; AL=1 -> Tab opened the boot menu
    call su_run_menu                ; AL=0 -> Del opened the setup page
    ret
.boot_menu:
    call boot_menu                  ; Tab boot-device menu
.done:
    ret

; su_wait_hotkey: peek the keyboard ring for the setup or boot-menu hotkey for a
; bounded spin. Returns CF=1 with AL=0 if Del (SETUP_HOTKEY) was seen, or CF=1 with
; AL=1 if Tab (BOOTMENU_HOTKEY) was seen; CF=0 if the window elapsed with neither.
; The first hotkey make is latched by su_poll_post_hotkeys; any later Del/Tab is
; consumed but ignored. Other keys are consumed so they do not leak into the boot
; path. Clobbers AX, CX.
su_wait_hotkey:
    mov cx, SU_HOTKEY_SPINS
.spin:
    call su_take_latched_hotkey
    jc .done
    call su_poll_post_hotkeys
    call su_take_latched_hotkey
    jc .done
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if empty
    jc .next
    xor ah, ah
    call kb_getkey                  ; drain one unrelated key
.next:
    loop .spin
    clc                             ; window elapsed, no hotkey
    ret
.done:
    ret

; su_poll_post_hotkeys: scan the keyboard ring and latch the first Del/Tab POST
; action. It does not consume keys; Esc must stay queued for the RAM-test skip.
; Safe to call from long POST steps: preserves registers and DS.
su_poll_post_hotkeys:
    pusha
    push ds
    xor ax, ax
    mov ds, ax
    cmp byte [POST_HOTKEY], 0
    jne .done
    mov ax, BDA_SEG
    mov ds, ax
    mov bx, [KB_BUF_HEAD]
.scan:
    cmp bx, [KB_BUF_TAIL]
    je .done
    mov ax, [bx]                    ; AH = scancode, AL = ASCII
    cmp ah, SETUP_HOTKEY
    je .setup
    cmp ah, BOOTMENU_HOTKEY
    je .boot
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .no_wrap
    mov bx, KB_BUF_START
.no_wrap:
    jmp .scan
.setup:
    call su_latch_setup_hotkey
    jmp .done
.boot:
    call su_latch_boot_hotkey
    jmp .done
.done:
    pop ds
    popa
    ret

su_latch_setup_hotkey:
    pusha
    push ds
    xor ax, ax
    mov ds, ax
    cmp byte [POST_HOTKEY], 0
    jne .done
    mov byte [POST_HOTKEY], POST_HOTKEY_SETUP
    mov si, su_str_enter_setup
    call su_post_note
.done:
    pop ds
    popa
    ret

su_latch_boot_hotkey:
    pusha
    push ds
    xor ax, ax
    mov ds, ax
    cmp byte [POST_HOTKEY], 0
    jne .done
    mov byte [POST_HOTKEY], POST_HOTKEY_BOOT
    mov si, su_str_enter_boot
    call su_post_note
.done:
    pop ds
    popa
    ret

; su_take_latched_hotkey: CF=1 with AL=0 for setup, AL=1 for boot; CF=0 if none.
su_take_latched_hotkey:
    push bx
    push ds
    xor bx, bx
    mov ds, bx
    mov bl, [POST_HOTKEY]
    pop ds
    cmp bl, POST_HOTKEY_SETUP
    je .setup
    cmp bl, POST_HOTKEY_BOOT
    je .boot
    pop bx
    clc
    ret
.setup:
    pop bx
    xor al, al
    stc
    ret
.boot:
    pop bx
    mov al, 1
    stc
    ret

; su_post_note: draw a short status line on the still-visible graphical POST
; screen (the Margo LFB) when a hotkey is caught, so the press gets immediate
; feedback before the setup page or boot menu redraws over it. SI = CS-relative
; ASCIIZ on entry. It overwrites the two key-hint rows with the note, ink on the
; field. Preserves the caller's registers (the hotkey path re-sets the CF/AL it
; returns after this call). The BIOS is in the LFB mode here, so this must use
; lfb_* primitives -- mode-13h gfx_* writes would be invisible.
su_post_note:
    pusha
    mov bp, si                      ; BP = note string pointer (survives the wipe)
    ; Wipe the two hint rows on the LFB to the field colour.
    mov di, ART_HDR_X
    mov si, ART_HINT1_Y
    mov cx, 225
    mov dx, 20
    mov bl, ART_FIELD_INDEX
    call lfb_fill_rect
    ; Draw the note where the first hint row was.
    push cs
    pop ds                          ; DS = CS so DS:SI reaches the ROM string
    mov si, bp
    mov di, ART_HDR_X
    mov ax, ART_HINT1_Y
    mov bl, ART_INK_INDEX
    mov bh, ART_FIELD_INDEX
    call lfb_text
    popa
    ret

; su_run_menu: seed the working copy from live hardware and CMOS, draw the main
; menu, then loop reading keys until Save or Discard. Clobbers all caller-saved
; registers (core calls this directly and nothing after it depends on them).
su_run_menu:
    sti                             ; the keyboard ring fills on IRQ1
    call su_seed
    mov byte [SU_CURSOR], SU_ROW_TIME   ; DS = 0 left set by su_seed
    call su_draw_fresh
.loop:
    ; Non-blocking, modeled on the bootbox loop: peek the ring each pass, fall
    ; through and re-loop when it is empty, consume and handle a waiting key. The
    ; menu has no live readout to animate, so the empty pass just spins.
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if the ring is empty
    jc .loop
    xor ah, ah
    call kb_getkey                  ; consume the waiting key: AH=scancode, AL=ascii
    cmp ah, SC_UP
    je .up
    cmp ah, SC_DOWN
    je .down
    cmp ah, SC_LEFT
    je .left
    cmp ah, SC_RIGHT
    je .right
    cmp ah, SC_F10
    je .save
    cmp ah, SC_ESC
    je .discard
    cmp ah, SC_ENTER
    je .enter
    jmp .loop
.up:
    xor ax, ax
    mov ds, ax
    mov cl, [SU_CURSOR]             ; CL = the row losing the highlight
    call su_cursor_dec              ; clobbers AX/BX only, CL survives
    jmp .cursor_moved
.down:
    xor ax, ax
    mov ds, ax
    mov cl, [SU_CURSOR]
    call su_cursor_inc
.cursor_moved:
    ; Repaint only the two affected rows and the help footer.
    mov bl, cl
    call su_draw_row                ; old row, back to plain
    xor ax, ax
    mov ds, ax
    mov bl, [SU_CURSOR]
    call su_draw_row                ; new row, highlighted
    call su_draw_help
    jmp .loop
.left:
    call su_edit_left
    call su_redraw_cursor_row
    jmp .loop
.right:
    call su_edit_right
    call su_redraw_cursor_row
    jmp .loop
.enter:
    xor cx, cx
    mov ds, cx
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, SU_ROW_TIME
    je .open_time
    cmp al, SU_ROW_PERIPH
    je .open_periph
    cmp al, SU_ROW_REPAIR
    je .repair
    cmp al, SU_ROW_SAVE
    je .save
    cmp al, SU_ROW_DISCARD
    je .discard
    ; LAYOUT and GSW are inline: Enter cycles them forward, like Right.
    call su_edit_right
    call su_redraw_cursor_row
    jmp .loop
.repair:
    mov al, 1                       ; Lotura service: Repair Toka-DOS
    mov si, su_str_repaired
    jmp .service
.service:
    ; Ask the machine to install or reinstall Toka-DOS, then read the status. The
    ; run loop performs the host-side work between this OUT and the next IN.
    out 0xE3, al
    in  al, 0xE3
    push ax                         ; save status across the repaint
    push si                         ; save the success message
    call su_draw                    ; repaint the menu under the note
    pop si
    pop ax
    test al, al
    jz .service_note                ; status 0 -> success message in SI
    mov si, su_str_svc_fail
.service_note:
    call su_service_note
    jmp .loop
.open_time:
    call su_time_page
    call su_draw_fresh              ; repaint the main menu on return; clear the sub-page first
    jmp .loop
.open_periph:
    call su_periph_page
    call su_draw_fresh
    jmp .loop
.save:
    call su_commit
    ; Fall through: leaving setup resets the machine, the way a real BIOS reboots
    ; on exit, so changes take effect from a clean POST.
.discard:
    cli
    jmp ROM_SEG:reset               ; cold-start the BIOS (replays POST)

; su_seed: load the working copy from the live GSW mode, CMOS NVRAM layout, and
; the RTC clock. Sets DS = 0 and leaves it there. Clobbers AX, BX, CX.
su_seed:
    xor cx, cx
    mov ds, cx                      ; DS = 0 for the scratch region
    ; GSW mode from the live Lotura register, clamped to 0..3.
    in al, PORT_LOTURA_MODE
    cmp al, 3
    jbe .mode_ok
    xor al, al
.mode_ok:
    mov bx, SU_WORKING_MODE
    mov [bx], al
    ; Keyboard layout from CMOS 0x10, clamped to a valid index.
    mov al, SU_NVRAM_LAYOUT
    call su_cmos_read
    cmp al, SU_LAYOUT_COUNT
    jb .layout_ok
    xor al, al
.layout_ok:
    mov bx, SU_WORKING_LAYOUT
    mov [bx], al
    ; Time fields from the RTC (binary, 24-hour).
    mov al, SU_RTC_HOUR
    call su_cmos_read
    mov bx, SU_TIME_HOUR
    mov [bx], al
    mov al, SU_RTC_MIN
    call su_cmos_read
    mov bx, SU_TIME_MIN
    mov [bx], al
    mov al, SU_RTC_SEC
    call su_cmos_read
    mov bx, SU_TIME_SEC
    mov [bx], al
    mov al, SU_RTC_DAY
    call su_cmos_read
    mov bx, SU_TIME_DAY
    mov [bx], al
    mov al, SU_RTC_MONTH
    call su_cmos_read
    mov bx, SU_TIME_MONTH
    mov [bx], al
    mov al, SU_RTC_YEAR
    call su_cmos_read
    mov bx, SU_TIME_YEAR
    mov [bx], al
    mov byte [SU_TIME_FIELD], SU_TF_HOUR
    mov byte [SU_TIME_EDITED], 0
    ret

; su_cmos_read: AL = CMOS register index on entry -> AL = the byte. Selects the
; index on port 0x70, reads port 0x71. Preserves nothing but AL. DS unchanged.
su_cmos_read:
    out SU_RTC_INDEX, al
    in al, SU_RTC_DATA
    ret

; su_cmos_write: AH = CMOS register index, AL = byte to write. Selects then writes.
; Preserves nothing of note.
su_cmos_write:
    push ax
    mov al, ah
    out SU_RTC_INDEX, al            ; select the register
    pop ax
    out SU_RTC_DATA, al             ; write the data byte
    ret

; su_cmos_checksum: sum NVRAM bytes 0x10..0x2D as a 16-bit value and store it high
; byte first at 0x2E and low byte at 0x2F, matching the RTC device's checksum. Read
; each byte through the index/data ports. Clobbers AX, BX, CX, DX.
su_cmos_checksum:
    xor dx, dx                      ; DX = running sum
    mov cl, SU_NVRAM_LAYOUT         ; CL = 0x10, the first covered byte
.sum:
    mov al, cl
    call su_cmos_read               ; AL = byte at index CL
    xor ah, ah
    add dx, ax
    inc cl
    cmp cl, 0x2e                    ; stop after 0x2D (0x2E is the checksum slot)
    jb .sum
    ; Store the 16-bit sum: high byte at 0x2E, low byte at 0x2F (AT order).
    mov al, dh
    mov ah, 0x2e
    call su_cmos_write
    mov al, dl
    mov ah, 0x2f
    call su_cmos_write
    ret

; su_cursor_dec / su_cursor_inc: move the main-menu highlight, wrapping. DS = 0.
; Clobbers AX, BX.
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

; su_edit_left / su_edit_right: Left/Right on the main menu edit the inline rows.
; LAYOUT cycles the keyboard layout 0..16. GSW cycles the four modes
; (386/486/586/386-slow), wrapping after the bootbox 386-slow row.
; Other rows are not inline-editable, so the keys are ignored there. DS = 0. Clobbers
; AX, BX.
su_edit_left:
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, SU_ROW_LAYOUT
    je .layout_dec
    cmp al, SU_ROW_GSW
    je .gsw_dec
    ret
.layout_dec:
    mov bx, SU_WORKING_LAYOUT
    mov al, [bx]
    test al, al
    jnz .layout_store
    mov al, SU_LAYOUT_COUNT
.layout_store:
    dec al
    mov [bx], al
    ret
.gsw_dec:
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    test al, al
    jnz .gsw_store
    mov al, 4
.gsw_store:
    dec al
    mov [bx], al
    ret

su_edit_right:
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, SU_ROW_LAYOUT
    je .layout_inc
    cmp al, SU_ROW_GSW
    je .gsw_inc
    ret
.layout_inc:
    mov bx, SU_WORKING_LAYOUT
    mov al, [bx]
    inc al
    cmp al, SU_LAYOUT_COUNT
    jb .layout_store
    xor al, al
.layout_store:
    mov [bx], al
    ret
.gsw_inc:
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    inc al
    cmp al, 4
    jb .gsw_store
    xor al, al
.gsw_store:
    mov [bx], al
    ret

; su_commit: write the working copy to hardware and CMOS, the Save action.
; Layout -> CMOS 0x10, GSW default -> CMOS 0x12, recompute the 0x2E/0x2F checksum
; over 0x10..0x2D, set the RTC registers if the time was edited, and write the GSW
; default to the live Lotura register. DS = 0. Clobbers AX, BX, CX.
su_commit:
    xor cx, cx
    mov ds, cx
    ; CMOS 0x10 = layout index.
    mov bx, SU_WORKING_LAYOUT
    mov al, [bx]
    mov ah, SU_NVRAM_LAYOUT
    call su_cmos_write
    ; Refresh the live INT 09h layout cache so the new table applies at once,
    ; without waiting for a reboot to re-read CMOS. DS bounces to the BDA and back.
    mov bx, SU_WORKING_LAYOUT
    mov al, [bx]                    ; DS is still 0 here
    push ds
    mov bx, BDA_SEG
    mov ds, bx
    mov [KB_LAYOUT], al
    pop ds
    ; Look up the code-page index for the new layout and write CMOS 0x11 so the
    ; font follows the layout. kbd_layout_codepage is a CS-relative ROM table.
    mov bx, SU_WORKING_LAYOUT
    mov bl, [bx]                   ; BL = layout index (0..16)
    xor bh, bh
    mov al, [cs:kbd_layout_codepage + bx]  ; AL = code-page index (0..4)
    mov ah, CP_CMOS_INDEX          ; 0x13
    call su_cmos_write
    ; Load the matching font live so the change is visible without a reboot.
    mov bx, SU_WORKING_LAYOUT
    mov bl, [bx]
    xor bh, bh
    mov al, [cs:kbd_layout_codepage + bx]
    call load_codepage_font
    ; CMOS 0x12 = GSW default.
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    mov ah, SU_NVRAM_GSW
    call su_cmos_write
    ; Refresh the NVRAM checksum now that the general-area bytes changed.
    call su_cmos_checksum
    ; Write the RTC clock only when the user changed it, so a Save that never
    ; touched Time leaves the running clock alone.
    mov bx, SU_TIME_EDITED
    mov al, [bx]
    test al, al
    jz .skip_time
    mov bx, SU_TIME_HOUR
    mov al, [bx]
    mov ah, SU_RTC_HOUR
    call su_cmos_write
    mov bx, SU_TIME_MIN
    mov al, [bx]
    mov ah, SU_RTC_MIN
    call su_cmos_write
    mov bx, SU_TIME_SEC
    mov al, [bx]
    mov ah, SU_RTC_SEC
    call su_cmos_write
    mov bx, SU_TIME_DAY
    mov al, [bx]
    mov ah, SU_RTC_DAY
    call su_cmos_write
    mov bx, SU_TIME_MONTH
    mov al, [bx]
    mov ah, SU_RTC_MONTH
    call su_cmos_write
    mov bx, SU_TIME_YEAR
    mov al, [bx]
    mov ah, SU_RTC_YEAR
    call su_cmos_write
.skip_time:
    ; Apply the GSW default live on the Lotura mode register.
    xor cx, cx
    mov ds, cx
    mov bx, SU_WORKING_MODE
    mov al, [bx]
    out PORT_LOTURA_MODE, al
    ret

; ============================ Main-menu drawing ============================
; su_draw: paint the main menu from scratch. Title bar, a bordered box holding
; the eight selectable rows (Time/Layout/GSW with their values, Peripherals/
; Health, Repair/Save/Discard), and a two-line context-sensitive help footer
; below the box. Sets DS = CS for the ROM strings; reads SU_CURSOR and the
; working values from segment 0.
; su_draw_fresh clears the whole framebuffer (and re-asserts the LFB mode +
; palette) before painting. That reset is only needed when arriving from POST or a
; sub-page, where pixels sit outside the regions the in-place repaint covers.
; Cursor moves and inline value edits stay on this page, so their hot paths skip
; su_draw entirely and repaint only the affected rows via su_draw_row (plus the
; help footer on cursor moves). Repainting the whole box (57K px of interior
; fill plus every label) on every keystroke was the slow-redraw cause.
su_draw_fresh:
    call su_enter_setup_lfb
    mov al, COLOR_BG                ; clear the whole screen so no stale POST
    call lfb_clear                  ; content (wordmark/mascot/text) bleeds through
su_draw:
    ; Title bar (red on field).
    push cs
    pop ds                          ; DS = CS for the label strings
    mov si, su_str_title
    mov di, SU_TITLE_X
    mov ax, SU_TITLE_Y
    mov bl, SU_C_TITLE
    mov bh, SU_C_FIELD
    call lfb_text

    ; Main box: ink border, cream interior.
    mov di, SU_BOX_X
    mov si, SU_BOX_Y
    mov cx, SU_BOX_W
    mov dx, SU_BOX_H
    mov bl, SU_C_FIELD
    mov bh, SU_C_INK
    call lfb_box

    ; All seven rows.
    xor bl, bl
.rows:
    push bx
    call su_draw_row
    pop bx
    inc bl
    cmp bl, SU_SEL_COUNT
    jb .rows

    ; Two-line context-sensitive help below the box.
    call su_draw_help
    ret

; ROM table: main-menu row label strings, indexed by row.
su_row_labels:  dw su_str_time, su_str_layout, su_str_gsw, su_str_periph
                dw su_str_repair, su_str_save, su_str_discard

; su_draw_row: repaint one main-menu row in place: wipe its band back to the
; field colour (erasing a stale highlight), then the label (su_row_colors
; re-paints the ink band when this is the focused row) and, for the Time/
; Keyboard/CPU rows, the value column. BL = row index. This is the keystroke
; hot path: a cursor move repaints two of these instead of the whole box.
; Clobbers AX, BX, CX, DX, SI, DI and DS.
su_draw_row:
    push bx
    ; Band wipe (same geometry as su_row_colors' highlight band).
    movzx ax, bl
    mov si, SU_ROW_DY
    mul si                          ; ax = row * SU_ROW_DY
    add ax, SU_MENU_Y - 2
    mov si, ax                      ; si = band y
    mov di, SU_MENU_X - 6
    mov cx, SU_BOX_W - (SU_MENU_X - 6 - SU_BOX_X) * 2
    mov dx, SU_ROW_DY
    mov bl, SU_C_FIELD
    call lfb_fill_rect
    pop bx
    ; Label: string from the ROM table, y from the row index.
    push bx
    movzx ax, bl
    mov si, SU_ROW_DY
    mul si
    add ax, SU_MENU_Y               ; ax = label y
    movzx si, bl
    shl si, 1
    mov si, [cs:su_row_labels + si]
    mov di, SU_MENU_X
    call su_row_colors              ; BL = row in -> BL/BH colours, DS = CS
    call lfb_text
    pop bx
    ; Value column for the three inline rows (tail calls; each self-contained).
    cmp bl, SU_ROW_TIME
    je su_draw_time_value
    cmp bl, SU_ROW_LAYOUT
    je su_draw_layout_value
    cmp bl, SU_ROW_GSW
    je su_draw_mode_value
    ret

; su_redraw_cursor_row: repaint just the highlighted row (an inline value edit
; changes nothing else on screen).
su_redraw_cursor_row:
    xor ax, ax
    mov ds, ax
    mov bl, [SU_CURSOR]
    jmp su_draw_row

; su_draw_help: wipe and redraw the two-line help footer below the box. Both
; lines change with the highlighted row: su_help_str_for_row returns line 1,
; and line 2 sits right after its NUL. DS bounces; leaves DS = CS.
su_draw_help:
    push cs
    pop ds
    mov di, SU_HELP_X
    mov si, SU_HELP1_Y
    mov cx, SU_BOX_W
    mov dx, 22
    mov bl, COLOR_BG                ; the screen field outside the box (black)
    call lfb_fill_rect
    xor cx, cx
    mov ds, cx
    mov bl, [SU_CURSOR]
    call su_help_str_for_row        ; DS:SI = CS:help string for this row
    mov di, SU_HELP_X
    mov ax, SU_HELP1_Y
    mov bl, SU_C_DISABLED
    mov bh, COLOR_BG
    call lfb_text                   ; preserves SI (pusha/popa)
.find_line2:
    inc si
    cmp byte [si - 1], 0
    jne .find_line2                 ; SI -> line 2, past line 1's NUL
    mov di, SU_HELP_X
    mov ax, SU_HELP2_Y
    call lfb_text
    ret

; su_help_str_for_row: BL = main-menu row index -> DS:SI = CS:help string.
; Sets DS = CS. Clobbers AX.
su_help_str_for_row:
    push cs
    pop ds
    cmp bl, SU_ROW_TIME
    je .time
    cmp bl, SU_ROW_LAYOUT
    je .layout
    cmp bl, SU_ROW_GSW
    je .gsw
    cmp bl, SU_ROW_PERIPH
    je .periph
    cmp bl, SU_ROW_REPAIR
    je .repair
    cmp bl, SU_ROW_SAVE
    je .save
    mov si, su_str_help_discard
    ret
.time:
    mov si, su_str_help_time
    ret
.layout:
    mov si, su_str_help_layout
    ret
.gsw:
    mov si, su_str_help_gsw
    ret
.periph:
    mov si, su_str_help_periph
    ret
.repair:
    mov si, su_str_help_repair
    ret
.save:
    mov si, su_str_help_save
    ret

; su_service_note: draw a one-line Repair result in the help band (line 1),
; bright ink, overwriting the context help until the next key repaints. DS = CS.
; Clobbers AX, DI.
su_service_note:
    push cs
    pop ds
    push si
    mov di, SU_HELP_X
    mov si, SU_HELP1_Y
    mov cx, SU_BOX_W
    mov dx, 11
    mov bl, COLOR_BG
    call lfb_fill_rect
    pop si
    push cs
    pop ds
    mov di, SU_HELP_X
    mov ax, SU_HELP1_Y
    mov bl, SU_C_TITLE
    mov bh, COLOR_BG
    call lfb_text
    ret

; su_row_colors: BL = selectable-row index on entry; returns BL = foreground,
; BH = background for that row's label (ink-on-field normally, field-on-ink
; inverse when it is the highlighted row -- the same focused-band look the Tab
; boot menu uses). When focused, also paints the ink highlight band under the
; row before returning, so the caller's lfb_text lands on top of it. The
; caller sets DI/AX (x/y) *before* calling this, for its own following
; lfb_text call, so DI/AX/SI/CX/DX are all fully preserved here (saved on the
; stack around the highlight-band math and the lfb_fill_rect call, which needs
; its own DI/SI/CX/DX). Restores DS = CS for the caller's lfb_text.
su_row_colors:
    push ax
    push cx
    push dx
    push si
    push di
    mov cl, bl                      ; save the input row index across the DS:BX load
    xor ax, ax
    mov ds, ax
    mov bx, SU_CURSOR
    mov al, [bx]
    cmp al, cl
    jne .plain
    ; Highlighted: paint the ink band across the row.
    movzx ax, cl
    mov si, SU_ROW_DY
    mul si                          ; ax = row * SU_ROW_DY (unsigned mul16, 8086-safe)
    add ax, SU_MENU_Y - 2
    mov si, ax                      ; si = band y
    mov di, SU_MENU_X - 6
    mov cx, SU_BOX_W - (SU_MENU_X - 6 - SU_BOX_X) * 2
    mov dx, SU_ROW_DY
    mov bl, SU_C_INK
    call lfb_fill_rect
    mov bl, SU_C_FIELD               ; fg = field (inverse)
    mov bh, SU_C_INK                 ; bg = ink band
    jmp .done
.plain:
    mov bl, SU_C_INK                 ; fg = ink
    mov bh, SU_C_FIELD                ; bg = field
.done:
    push cs
    pop ds                           ; restore DS = CS for the caller's lfb_text
    pop di
    pop si
    pop dx
    pop cx
    pop ax
    ret

; su_cs_row_text_colors: BL = selectable-row index on entry; returns BL = fg,
; BH = bg for text already sitting on a row whose highlight band was painted by
; su_row_colors. Unlike su_row_colors, this does not repaint the band, so inline
; value redraws cannot erase the label. Restores DS = CS for lfb_text.
su_cs_row_text_colors:
    push ax
    push cx
    mov cl, bl
    xor ax, ax
    mov ds, ax
    mov al, [SU_CURSOR]
    push cs
    pop ds
    cmp al, cl
    jne .plain
    mov bl, SU_C_FIELD
    mov bh, SU_C_INK
    jmp .done
.plain:
    mov bl, SU_C_INK
    mov bh, SU_C_FIELD
.done:
    pop cx
    pop ax
    ret

; su_draw_mode_value: draw the working GSW mode name at the
; value column on the GSW row. Reads SU_WORKING_MODE; uses DS = CS for the string.
; Re-derives the row text colors without repainting the highlight band.
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
    cmp al, 2
    je .m586
    mov si, su_str_mode386slow
    jmp .put
.m386:
    mov si, su_str_mode386
    jmp .put
.m486:
    mov si, su_str_mode486
    jmp .put
.m586:
    mov si, su_str_mode586
.put:
    mov di, SU_VALUE_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 2
    mov bl, SU_ROW_GSW
    call su_cs_row_text_colors
    call lfb_text
    pop cx
    ret

; su_draw_layout_value: draw the working keyboard layout's name at the value
; column on the Layout row. Reads SU_WORKING_LAYOUT; selects the name pointer from
; su_layout_names. DS = CS for the string.
su_draw_layout_value:
    push cx
    xor cx, cx
    mov ds, cx
    mov bx, SU_WORKING_LAYOUT
    mov al, [bx]
    push cs
    pop ds
    cmp al, SU_LAYOUT_COUNT
    jb .ok
    xor al, al
.ok:
    movzx bx, al
    shl bx, 1
    mov si, [su_layout_names + bx]
    mov di, SU_VALUE_X
    mov ax, SU_MENU_Y + SU_ROW_DY
    mov bl, SU_ROW_LAYOUT
    call su_cs_row_text_colors
    call lfb_text
    pop cx
    ret

; su_draw_time_value: draw HH:MM:SS for the working time at the value column on the
; Time row. Reads the working clock from segment 0. DS = CS for the colon string.
su_draw_time_value:
    push cx
    mov di, SU_VALUE_X
    mov bx, SU_TIME_HOUR
    call su_put2                    ; HH
    call su_put_colon
    mov bx, SU_TIME_MIN
    call su_put2                    ; MM
    call su_put_colon
    mov bx, SU_TIME_SEC
    call su_put2                    ; SS
    pop cx
    ret

; su_time_row_colors: return BL=fg, BH=bg for the Time row's value glyphs,
; matching su_row_colors' highlighted-vs-plain look, but WITHOUT touching DS --
; su_row_colors always hands back DS=CS for a CS-relative label, which is wrong
; for these callers' segment-0 BIOS_VALBUF/colon-buffer strings. The highlight
; band itself is already painted by the Time row's own su_row_colors call in
; su_draw/su_draw_time_value, so this only needs to pick the two colors, not
; paint anything. Preserves all registers except BL/BH.
su_time_row_colors:
    push ax
    push ds
    xor ax, ax
    mov ds, ax
    mov al, [SU_CURSOR]
    cmp al, SU_ROW_TIME
    jne .plain
    mov bl, SU_C_FIELD               ; fg = field (inverse, on the ink band)
    mov bh, SU_C_INK
    jmp .done
.plain:
    mov bl, SU_C_INK                 ; fg = ink
    mov bh, SU_C_FIELD
.done:
    pop ds
    pop ax
    ret

; su_put2: draw the two-digit decimal of the byte at segment-0 [BX] at DI on the
; Time row, advancing DI by 16 px. Uses BIOS_VALBUF to format. Clobbers AX.
su_put2:
    push cx
    push dx
    push si
    xor cx, cx
    mov ds, cx                      ; DS = 0 to read the value and write the buffer
    mov al, [bx]
    xor ah, ah
    mov cl, 10
    div cl                          ; AL = tens, AH = units
    add al, '0'
    add ah, '0'
    mov [BIOS_VALBUF], al
    mov [BIOS_VALBUF + 1], ah
    mov byte [BIOS_VALBUF + 2], 0
    mov si, BIOS_VALBUF             ; DS:SI = the two-digit string (DS still 0)
    push di
    mov ax, SU_MENU_Y
    call su_time_row_colors         ; BL/BH only; DS stays 0 for lfb_text's DS:SI
    call lfb_text
    pop di
    add di, 16
    pop si
    pop dx
    pop cx
    ret

; su_put_colon: draw a ':' separator at DI on the Time row, advancing DI by 8 px.
; DS = CS for the ROM ':' string, so this picks its own fg/bg without disturbing
; DS the way su_row_colors/su_time_row_colors would (both assume a specific DS
; for the caller's following lfb_text, and this caller's is CS, not 0).
su_put_colon:
    push ax
    push si
    push cs
    pop ds
    mov si, su_str_colon
    push di
    ; Fg/bg: inverse (field-on-ink) when Time is the highlighted row, else plain.
    push bx
    xor ax, ax
    push ds
    mov ds, ax
    mov al, [SU_CURSOR]
    pop ds
    cmp al, SU_ROW_TIME
    jne .plain
    mov bl, SU_C_FIELD
    mov bh, SU_C_INK
    jmp .have_color
.plain:
    mov bl, SU_C_INK
    mov bh, SU_C_FIELD
.have_color:
    mov ax, SU_MENU_Y
    call lfb_text
    pop bx
    pop di
    add di, 8
    pop si
    pop ax
    ret

; ============================ Time sub-page ============================
; su_time_page: edit the working clock. Left/Right move the highlighted field,
; Up/Down (and Enter) bump it, Esc returns to the main menu. The first change sets
; SU_TIME_EDITED so su_commit knows to write the RTC. DS bounces between 0 and CS.
su_time_page:
    sti
    call su_time_draw_fresh
.loop:
    ; Non-blocking peek loop (Bug 5), matching su_run_menu and the bootbox model.
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if the ring is empty
    jc .loop
    xor ah, ah
    call kb_getkey                  ; consume the waiting key
    cmp ah, SC_ESC
    je .back
    cmp ah, SC_LEFT
    je .field_left
    cmp ah, SC_RIGHT
    je .field_right
    cmp ah, SC_UP
    je .bump_up
    cmp ah, SC_DOWN
    je .bump_down
    cmp ah, SC_ENTER
    je .bump_up
    jmp .loop
.field_left:
    call su_time_field_dec
    call su_time_draw
    jmp .loop
.field_right:
    call su_time_field_inc
    call su_time_draw
    jmp .loop
.bump_up:
    mov cl, 1
    call su_time_adjust
    call su_time_draw
    jmp .loop
.bump_down:
    mov cl, 0xff                    ; -1
    call su_time_adjust
    call su_time_draw
    jmp .loop
.back:
    ret

; su_time_field_dec / inc: move the highlighted time field, wrapping. DS = 0.
su_time_field_dec:
    xor ax, ax
    mov ds, ax
    mov bx, SU_TIME_FIELD
    mov al, [bx]
    test al, al
    jnz .ok
    mov al, SU_TF_COUNT
.ok:
    dec al
    mov [bx], al
    ret
su_time_field_inc:
    xor ax, ax
    mov ds, ax
    mov bx, SU_TIME_FIELD
    mov al, [bx]
    inc al
    cmp al, SU_TF_COUNT
    jb .ok
    xor al, al
.ok:
    mov [bx], al
    ret

; su_time_adjust: add CL (1 or 0xFF for -1) to the highlighted field, wrapping
; within that field's range, and mark the time edited. DS = 0. Clobbers AX, BX, DX.
su_time_adjust:
    xor ax, ax
    mov ds, ax
    mov byte [SU_TIME_EDITED], 1
    mov bx, SU_TIME_FIELD
    mov al, [bx]
    ; Dispatch on the field index to its scratch byte and modulus.
    cmp al, SU_TF_HOUR
    je .hour
    cmp al, SU_TF_MIN
    je .min
    cmp al, SU_TF_SEC
    je .sec
    cmp al, SU_TF_DAY
    je .day
    cmp al, SU_TF_MONTH
    je .month
    jmp .year
.hour:
    mov bx, SU_TIME_HOUR
    mov dl, 24
    mov dh, 0                       ; range 0..23
    jmp .apply
.min:
    mov bx, SU_TIME_MIN
    mov dl, 60
    mov dh, 0
    jmp .apply
.sec:
    mov bx, SU_TIME_SEC
    mov dl, 60
    mov dh, 0
    jmp .apply
.day:
    mov bx, SU_TIME_DAY
    mov dl, 31
    mov dh, 1                       ; range 1..31
    jmp .apply
.month:
    mov bx, SU_TIME_MONTH
    mov dl, 12
    mov dh, 1                       ; range 1..12
    jmp .apply
.year:
    mov bx, SU_TIME_YEAR
    mov dl, 100
    mov dh, 0                       ; range 0..99
.apply:
    mov al, [bx]
    sub al, dh                      ; shift into a 0-based value for the wrap math
    cmp cl, 1
    je .up
    ; down: -1, wrapping to (modulus-1)
    test al, al
    jnz .down_ok
    mov al, dl
.down_ok:
    dec al
    jmp .store
.up:
    inc al
    cmp al, dl
    jb .store
    xor al, al
.store:
    add al, dh                      ; restore the 1-based offset for day/month
    mov [bx], al
    ret

; su_time_draw: paint the time sub-page's editable fields. su_time_draw_fresh
; clears first and draws the static chrome (title, box, hint footer), used on
; entry from the main menu. The field-edit hot path calls su_time_draw, which
; only wipes and repaints the two 64px field lines inside the box.
su_time_draw_fresh:
    call su_enter_setup_lfb
    mov al, COLOR_BG
    call lfb_clear
    push cs
    pop ds
    mov si, su_str_time_title
    mov di, SU_TITLE_X
    mov ax, SU_TITLE_Y
    mov bl, SU_C_TITLE
    mov bh, SU_C_FIELD
    call lfb_text
    ; Sub-page box, shorter than the main menu box (just the two lines).
    mov di, SU_BOX_X
    mov si, SU_BOX_Y
    mov cx, SU_BOX_W
    mov dx, 56
    mov bl, SU_C_FIELD
    mov bh, SU_C_INK
    call lfb_box
    ; Hint footer, below the box.
    mov di, SU_HELP_X
    mov si, SU_HELP1_Y
    mov cx, SU_BOX_W
    mov dx, 11
    mov bl, COLOR_BG
    call lfb_fill_rect
    mov si, su_str_time_hint
    mov di, SU_HELP_X
    mov ax, SU_HELP1_Y
    mov bl, SU_C_DISABLED
    mov bh, COLOR_BG
    call lfb_text
su_time_draw:
    ; Wipe both field lines back to the box field colour (erases the previous
    ; highlight band; each line is 3 fields * 16px + 2 separators * 8px = 64px,
    ; bands run from y-1, 10 high).
    mov di, SU_MENU_X
    mov si, SU_BOX_Y + 11
    mov cx, 64
    mov dx, 10
    mov bl, SU_C_FIELD
    call lfb_fill_rect
    mov di, SU_MENU_X
    mov si, SU_BOX_Y + 31
    mov cx, 64
    mov dx, 10
    mov bl, SU_C_FIELD
    call lfb_fill_rect
    ; Time line: HH:MM:SS, each field colored by whether it is highlighted.
    mov di, SU_MENU_X
    mov bp, SU_BOX_Y + 12
    mov bl, SU_TF_HOUR
    mov si, SU_TIME_HOUR
    call su_time_field
    call su_time_colon
    mov bl, SU_TF_MIN
    mov si, SU_TIME_MIN
    call su_time_field
    call su_time_colon
    mov bl, SU_TF_SEC
    mov si, SU_TIME_SEC
    call su_time_field
    ; Date line: DD / MM / YY.
    mov di, SU_MENU_X
    mov bp, SU_BOX_Y + 32
    mov bl, SU_TF_DAY
    mov si, SU_TIME_DAY
    call su_time_field
    call su_time_slash
    mov bl, SU_TF_MONTH
    mov si, SU_TIME_MONTH
    call su_time_field
    call su_time_slash
    mov bl, SU_TF_YEAR
    mov si, SU_TIME_YEAR
    call su_time_field
    ret

; su_time_field: draw the two-digit value at segment-0 offset SI at pixel DI/BP,
; in an inverse ink band if field index BL is the highlighted field else plain
; ink-on-field. Advances DI by 16 px. DS bounces 0/CS internally. lfb_fill_rect
; is pusha/popa internally, so DI/SI/BX/CX/DX all survive that call for free.
; Clobbers AX, CX.
su_time_field:
    push si
    push dx
    xor ax, ax
    mov ds, ax                      ; DS = 0 for the field state, value, and buffer
    ; DH = highlighted field index; decide plain vs. inverse.
    push bx
    mov bx, SU_TIME_FIELD
    mov dh, [bx]
    pop bx
    cmp bl, dh
    jne .plain
    ; Highlighted: paint a small ink band under this field (DI/SI still hold the
    ; caller's x/bp-relative y going in; lfb_fill_rect preserves them via popa).
    push si
    mov si, bp
    dec si
    mov cx, 14
    mov dx, 10
    push bx
    mov bl, SU_C_INK
    call lfb_fill_rect
    pop bx
    pop si
    mov dl, SU_C_FIELD               ; fg = field
    mov dh, SU_C_INK                 ; bg = ink
    jmp .have_color
.plain:
    mov dl, SU_C_INK                 ; fg = ink
    mov dh, SU_C_FIELD                ; bg = field
.have_color:
    ; Format the two-digit value at [SI] into BIOS_VALBUF.
    mov al, [si]
    xor ah, ah
    mov cl, 10
    div cl                          ; AL = tens, AH = units
    add al, '0'
    add ah, '0'
    mov [BIOS_VALBUF], al
    mov [BIOS_VALBUF + 1], ah
    mov byte [BIOS_VALBUF + 2], 0
    mov si, BIOS_VALBUF
    mov bl, dl                      ; BL = chosen foreground
    mov bh, dh                      ; BH = chosen background
    mov ax, bp                      ; y
    call lfb_text                   ; DS = 0, SI = buffer
    add di, 16
    pop dx
    pop si
    ret

; su_time_colon / su_time_slash: draw a separator at DI/BP, advancing DI by 8 px.
su_time_colon:
    push si
    push cs
    pop ds
    mov si, su_str_colon
    jmp su_time_sep_common
su_time_slash:
    push si
    push cs
    pop ds
    mov si, su_str_slash
su_time_sep_common:
    mov bl, SU_C_INK
    mov bh, SU_C_FIELD
    mov ax, bp
    call lfb_text
    add di, 8
    pop si
    ret

; ============================ Peripherals sub-page ============================
; SU_PROBE name, handler, rowindex: draw the probe's label, call the handler, and
; draw PASS (green) / FAIL (red) at the value column. A MEASURE handler counts as a
; pass here (it returned a value, so the device answered). Expanded inline in
; su_periph_page so each probe is a few instructions, not a shared dispatcher.
%macro SU_PROBE 3
    push cs
    pop ds
    mov si, %1
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * %3
    mov bl, SU_C_INK
    mov bh, SU_C_FIELD
    call lfb_text
    mov ax, BDA_SEG
    mov ds, ax                      ; probe contract: DS = BDA on entry
    call %2                         ; AL = HRES_* on return
    push cs
    pop ds                          ; DS = CS for the status string
    cmp al, HRES_FAIL
    je %%fail
    mov si, su_str_pass
    mov bl, SU_C_PASS
    jmp %%put
%%fail:
    mov si, su_str_fail
    mov bl, SU_C_FAIL
%%put:
    mov di, SU_VALUE_X
    mov ax, SU_MENU_Y + SU_ROW_DY * %3
    mov bh, SU_C_FIELD
    call lfb_text
%endmacro

; su_periph_page: re-run each POST probe live (they are non-destructive) and list
; its PASS/FAIL as a boxed table with an aligned status column. Esc returns. The
; probe handlers follow the POST contract: AL = HRES_PASS/FAIL/MEASURE on return;
; MEASURE leaves SI = a segment-0 ASCIIZ value offset (unused here -- none of
; these seven probes return MEASURE in practice). We enter each with DS = BDA_SEG
; to match that contract.
su_periph_page:
    call su_enter_setup_lfb
    mov al, COLOR_BG
    call lfb_clear
    push cs
    pop ds
    mov si, su_str_periph_title
    mov di, SU_TITLE_X
    mov ax, SU_TITLE_Y
    mov bl, SU_C_TITLE
    mov bh, SU_C_FIELD
    call lfb_text
    ; Table box sized for the 7 probe rows.
    mov di, SU_BOX_X
    mov si, SU_BOX_Y
    mov cx, SU_BOX_W
    mov dx, SU_ROW_DY * 7 + 12
    mov bl, SU_C_FIELD
    mov bh, SU_C_INK
    call lfb_box
    ; A thin rule under the header band separating the title row from the probes
    ; would need another box primitive call; skipped to keep this lean -- the row
    ; color contrast already reads as a table.

    ; One row per probe. SU_PROBE macro: name string, handler label, row index.
    SU_PROBE su_str_p_lotura, probe_lotura, 0
    SU_PROBE su_str_p_8042,   probe_kbd8042, 1
    SU_PROBE su_str_p_pit,    probe_pit, 2
    SU_PROBE su_str_p_com1,   probe_serial, 3
    SU_PROBE su_str_p_sbdsp,  probe_sbdsp, 4
    SU_PROBE su_str_p_opl,    probe_opl, 5
    SU_PROBE su_str_p_margo,  probe_margo, 6

    ; Hint footer, below the box.
    push cs
    pop ds
    mov di, SU_HELP_X
    mov si, SU_HELP1_Y
    mov cx, SU_BOX_W
    mov dx, 11
    mov bl, COLOR_BG
    call lfb_fill_rect
    mov si, su_str_periph_hint
    mov di, SU_HELP_X
    mov ax, SU_HELP1_Y
    mov bl, SU_C_DISABLED
    mov bh, COLOR_BG
    call lfb_text

    sti
.loop:
    ; Non-blocking peek loop (Bug 5). The probe readouts are drawn once on entry;
    ; Esc returns. Any other key is consumed and ignored.
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if the ring is empty
    jc .loop
    xor ah, ah
    call kb_getkey                  ; consume the waiting key
    cmp ah, SC_ESC
    jne .loop
    ret

; ============================ Strings ============================
su_str_title:        db 'IZARRA 3000 SETUP', 0
su_str_time:         db 'TIME', 0
su_str_layout:       db 'KEYBOARD', 0
su_str_gsw:          db 'CPU MODE', 0
su_str_periph:       db 'PERIPHERALS', 0
su_str_repair:       db 'REPAIR TOKA-DOS', 0
su_str_repaired:     db 'Toka-DOS repaired', 0
su_str_svc_fail:     db 'No Toka-DOS image', 0
su_str_save:         db 'SAVE AND EXIT', 0
su_str_discard:      db 'DISCARD AND EXIT', 0
; Context-sensitive help, one dual-NUL pair per main-menu row: line 1, then
; line 2 right after its terminator (su_draw_help draws both). Two short lines
; because one long line overflows the 320px scanline and wraps.
su_str_help_time:    db 'Enter: set clock', 0
                     db 'Up/Dn move  Esc back', 0
su_str_help_layout:  db 'Left/Right: change keyboard', 0
                     db 'Enter: open', 0
su_str_help_gsw:     db 'Left/Right: change CPU speed', 0
                     db 'Enter: open', 0
su_str_help_periph:  db 'Enter: check devices', 0
                     db 'Up/Dn move  Esc back', 0
su_str_help_repair:  db 'Enter: reinstall Toka-DOS', 0
                     db 'Up/Dn move', 0
su_str_help_save:    db 'Enter/F10: save and reboot', 0
                     db 'Up/Dn move', 0
su_str_help_discard: db 'Enter/Esc: discard and reboot', 0
                     db 'Up/Dn move', 0
; POST hotkey feedback (Bug 4). Drawn on the white POST field, so kept short.
su_str_enter_setup:  db 'Entering configuration...', 0
su_str_enter_boot:   db 'Entering boot menu...', 0
su_str_colon:        db ':', 0
su_str_slash:        db '/', 0

su_str_mode386:      db '386', 0
su_str_mode486:      db '486', 0
su_str_mode586:      db '586', 0
su_str_mode386slow:  db '386-slow', 0

; Keyboard layout names, indexed by CMOS 0x10 (0 US .. 16 LA).
; Codes match the MS-DOS KEYB tool identifiers.
su_str_lay_us:       db 'US', 0
su_str_lay_uk:       db 'UK', 0
su_str_lay_es:       db 'SP', 0
su_str_lay_fr:       db 'FR', 0
su_str_lay_de:       db 'GR', 0
su_str_lay_it:       db 'IT', 0
su_str_lay_be:       db 'BE', 0
su_str_lay_cf:       db 'CF', 0
su_str_lay_dk:       db 'DK', 0
su_str_lay_nl:       db 'NL', 0
su_str_lay_no:       db 'NO', 0
su_str_lay_po:       db 'PO', 0
su_str_lay_sf:       db 'SF', 0
su_str_lay_sg:       db 'SG', 0
su_str_lay_su:       db 'SU', 0
su_str_lay_sv:       db 'SV', 0
su_str_lay_la:       db 'LA', 0
su_layout_names:     dw su_str_lay_us, su_str_lay_uk, su_str_lay_es
                     dw su_str_lay_fr, su_str_lay_de, su_str_lay_it
                     dw su_str_lay_be, su_str_lay_cf, su_str_lay_dk
                     dw su_str_lay_nl, su_str_lay_no, su_str_lay_po
                     dw su_str_lay_sf, su_str_lay_sg, su_str_lay_su
                     dw su_str_lay_sv, su_str_lay_la

su_str_time_title:   db 'SET TIME AND DATE', 0
su_str_time_hint:    db 'L/R field  Up/Dn change  Esc back', 0

su_str_periph_title: db 'PERIPHERAL CHECK', 0
su_str_periph_hint:  db 'Esc: back to menu', 0
su_str_pass:         db 'PASS', 0
su_str_fail:         db 'FAIL', 0
su_str_p_lotura:     db 'LOTURA', 0
su_str_p_8042:       db '8042 KBD', 0
su_str_p_pit:        db 'PIT TIMER', 0
su_str_p_com1:       db 'COM1 UART', 0
su_str_p_sbdsp:      db 'SB DSP', 0
su_str_p_opl:        db 'OPL FM', 0
su_str_p_margo:      db 'MARGO VGA', 0
