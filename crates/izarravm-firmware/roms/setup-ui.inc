; setup-ui.inc - STREAM D: the mode-13h setup page. Owns this file and the
; SETUP_SCRATCH region (segment 0, 256 bytes). Core calls setup_maybe_enter after
; run_post and before enter_setup_or_boot.
;
; Flow: peek the keyboard for a brief window. If the setup hotkey (Del, scancode
; SETUP_HOTKEY) is not waiting, return and let the BIOS boot. If it is, switch to
; mode 13h and draw the main menu. The menu is navigated by Up/Down (highlight),
; Left/Right (edit the inline rows: keyboard layout and GSW mode), Enter (open a
; sub-page or fire a Save/Discard action), and Esc (back out, treated as Discard
; on the main menu).
;
; The page edits a working copy held in SETUP_SCRATCH and seeded at entry from the
; live hardware and CMOS NVRAM. Nothing is committed until Save: Save writes the
; CMOS NVRAM area (layout 0x10, GSW default 0x12), refreshes the 0x2E/0x2F
; checksum, sets the RTC registers when the time was edited, and writes the GSW
; default to the live Lotura mode register (PORT_LOTURA_MODE). Discard leaves all
; of that untouched. Both exits return to the boot path.
;
; The peripherals page re-runs the POST probe routines live (they are
; non-destructive) and shows each as PASS/FAIL or a MEASURE value, rather than
; parsing the VDTS result block by name in 16-bit code. The health page shows
; voltage rails and temperatures with a small per-refresh jitter from an LFSR.

; --- SETUP_SCRATCH layout (segment 0) --------------------------------------
%define SU_WORKING_MODE   SETUP_SCRATCH + 0    ; edited GSW mode (0/1/2)
%define SU_WORKING_LAYOUT SETUP_SCRATCH + 1    ; edited keyboard layout (0..5)
%define SU_CURSOR         SETUP_SCRATCH + 2    ; highlighted main-menu row
%define SU_TIME_FIELD     SETUP_SCRATCH + 3    ; highlighted time field (0..5)
%define SU_TIME_EDITED    SETUP_SCRATCH + 4    ; 1 once the time was changed
%define SU_TIME_HOUR      SETUP_SCRATCH + 5    ; working clock, binary
%define SU_TIME_MIN       SETUP_SCRATCH + 6
%define SU_TIME_SEC       SETUP_SCRATCH + 7
%define SU_TIME_DAY       SETUP_SCRATCH + 8
%define SU_TIME_MONTH     SETUP_SCRATCH + 9
%define SU_TIME_YEAR      SETUP_SCRATCH + 10   ; two-digit year (0..99)
%define SU_LFSR_LO        SETUP_SCRATCH + 12   ; health-page jitter LFSR (word)
%define SU_HEALTH_TICK    SETUP_SCRATCH + 14   ; last BDA tick the health page drew (word)

; Main-menu selectable rows, in highlight order.
%define SU_ROW_TIME       0
%define SU_ROW_LAYOUT     1
%define SU_ROW_GSW        2
%define SU_ROW_PERIPH     3
%define SU_ROW_HEALTH     4
%define SU_ROW_REPAIR     5
%define SU_ROW_FORMAT     6
%define SU_ROW_SAVE       7
%define SU_ROW_DISCARD    8
%define SU_SEL_COUNT      9

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
%define SU_NVRAM_LAYOUT   0x10
%define SU_NVRAM_GSW      0x12

; Number of selectable keyboard layouts (en-US/UK/ES/FR/DE/IT). The active table
; selection lives in CMOS 0x10; the layout-switching ISR is a separate stream, so
; this page only persists the index and names the choice.
%define SU_LAYOUT_COUNT   6

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

; Menu layout (mode 13h pixels).
%define SU_MENU_X         16
%define SU_MENU_Y         40
%define SU_ROW_DY         12
%define SU_VALUE_X        160
%define SU_COLOR_HEAD     0x0e        ; yellow heading
%define SU_COLOR_INFO     0x07        ; light grey body text
%define SU_COLOR_ROW      0x0f        ; white (unselected selectable row)
%define SU_COLOR_HILITE   0x0b        ; bright cyan (highlighted row)
%define SU_COLOR_PASS     0x0a        ; bright green
%define SU_COLOR_FAIL     0x0c        ; bright red
%define SU_COLOR_VALUE    0x0b        ; bright cyan (a measured/edited value)

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
    call boot_menu                  ; Stream E: the Tab boot-device menu
.done:
    ret

; su_wait_hotkey: peek the keyboard ring for the setup or boot-menu hotkey for a
; bounded spin. Returns CF=1 with AL=0 if Del (SETUP_HOTKEY) was seen, or CF=1 with
; AL=1 if Tab (BOOTMENU_HOTKEY) was seen; CF=0 if the window elapsed with neither.
; The hotkey make is consumed from the ring. Any other key seen during the window is
; consumed and ignored so it does not leak into the boot path. Clobbers AX, CX.
su_wait_hotkey:
    mov cx, SU_HOTKEY_SPINS
.spin:
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if empty
    jc .next
    cmp ah, SETUP_HOTKEY
    je .hit_setup
    cmp ah, BOOTMENU_HOTKEY
    je .hit_boot
    ; some other key: drain it and keep watching for a hotkey.
    push cx
    xor ah, ah
    call kb_getkey
    pop cx
    jmp .next
.next:
    loop .spin
    clc                             ; window elapsed, no hotkey
    ret
.hit_setup:
    xor ah, ah
    call kb_getkey                  ; drain the Del make from the ring
    mov si, su_str_enter_setup
    call su_post_note               ; brief feedback on the POST screen
    xor al, al                      ; AL=0 -> setup page
    stc
    ret
.hit_boot:
    xor ah, ah
    call kb_getkey                  ; drain the Tab make from the ring
    mov si, su_str_enter_boot
    call su_post_note               ; brief feedback on the POST screen
    mov al, 1                       ; AL=1 -> boot menu
    stc
    ret

; su_post_note: draw a short status line on the still-visible graceful POST screen
; when a hotkey is caught, so the press gets immediate feedback before the setup
; page or boot menu repaints over it. SI = CS-relative ASCIIZ on entry. Black on the
; white POST field, just under the hint row. Preserves the caller's registers and
; the CF/AL the hotkey path is about to return.
su_post_note:
    pusha
    push cs
    pop ds                          ; DS = CS so DS:SI reaches the ROM string
    mov di, POST_TEXT_X
    mov ax, POST_HINT_Y + 12        ; one row below the "Del: Setup ..." hint
    mov bl, GFX_BLACK
    mov bh, GFX_WHITE
    call gfx_text
    popa
    ret

; su_run_menu: seed the working copy from live hardware and CMOS, draw the main
; menu, then loop reading keys until Save or Discard. Clobbers all caller-saved
; registers (core calls this directly and nothing after it depends on them).
su_run_menu:
    sti                             ; the keyboard ring fills on IRQ1
    call su_seed
    mov byte [SU_CURSOR], SU_ROW_TIME   ; DS = 0 left set by su_seed
    call su_draw
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
    call su_cursor_dec
    call su_draw
    jmp .loop
.down:
    call su_cursor_inc
    call su_draw
    jmp .loop
.left:
    call su_edit_left
    call su_draw
    jmp .loop
.right:
    call su_edit_right
    call su_draw
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
    cmp al, SU_ROW_HEALTH
    je .open_health
    cmp al, SU_ROW_REPAIR
    je .repair
    cmp al, SU_ROW_FORMAT
    je .format
    cmp al, SU_ROW_SAVE
    je .save
    cmp al, SU_ROW_DISCARD
    je .discard
    ; LAYOUT and GSW are inline: Enter cycles them forward, like Right.
    call su_edit_right
    call su_draw
    jmp .loop
.repair:
    mov al, 1                       ; Lotura service: Repair Toka-DOS
    mov si, su_str_repaired
    jmp .service
.format:
    mov al, 2                       ; Lotura service: Format Drive
    mov si, su_str_formatted
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
    call su_draw                    ; repaint the main menu on return
    jmp .loop
.open_periph:
    call su_periph_page
    call su_draw
    jmp .loop
.open_health:
    call su_health_page
    call su_draw
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
    ; GSW mode from the live Lotura register, clamped to 0..2.
    in al, PORT_LOTURA_MODE
    cmp al, 2
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
    ; Seed the LFSR from the live seconds count so the health jitter differs run
    ; to run; force it nonzero so the shift register never locks at 0.
    mov al, SU_RTC_SEC
    call su_cmos_read
    mov ah, 0xa3
    or al, 1
    mov [SU_LFSR_LO], ax
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
; LAYOUT cycles the keyboard layout 0..5; GSW cycles the mode 0..2. Other rows are
; not inline-editable, so the keys are ignored there. DS = 0. Clobbers AX, BX.
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
    mov al, 3
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
    cmp al, 3
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

; su_enter_mode13h: set mode 13h, then force DAC index 0 (the setup background) to
; black. The graceful POST screen reprograms index 0 to white, and INT 10h AH=00
; does not restore the DAC here, so the setup page would otherwise draw its white
; labels onto a white field. Setting index 0 black restores the standard mode-13h
; field this page assumes; every other index it uses is at its mode-13h default.
su_enter_mode13h:
    call set_mode13h
    push ax
    push bx
    push cx
    push dx
    mov bx, COLOR_BG                ; DAC index 0
    xor dh, dh                      ; R = 0
    xor ch, ch                      ; G = 0
    xor cl, cl                      ; B = 0
    call gfx_set_palette
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; ============================ Main-menu drawing ============================
; su_draw: paint the main menu from scratch. Heading, the seven selectable rows
; (Time/Layout/GSW with their values, Peripherals/Health, Save/Discard), and a
; footer hint. Sets DS = CS for the ROM strings; reads SU_CURSOR and the working
; values from segment 0.
su_draw:
    call su_enter_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds                          ; DS = CS for the label strings

    ; Title.
    mov si, su_str_title
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY * 2
    mov bl, SU_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text

    ; Time row (label + HH:MM:SS).
    mov bl, SU_ROW_TIME
    call su_row_color
    mov si, su_str_time
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y
    mov bh, COLOR_BG
    call gfx_text
    call su_draw_time_value         ; small HH:MM:SS next to the label

    ; Keyboard layout row (label + name).
    mov bl, SU_ROW_LAYOUT
    call su_row_color
    mov si, su_str_layout
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY
    mov bh, COLOR_BG
    call gfx_text
    call su_draw_layout_value

    ; GSW mode row (label + name).
    mov bl, SU_ROW_GSW
    call su_row_color
    mov si, su_str_gsw
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 2
    mov bh, COLOR_BG
    call gfx_text
    call su_draw_mode_value

    ; Peripherals row.
    mov bl, SU_ROW_PERIPH
    call su_row_color
    mov si, su_str_periph
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 3
    mov bh, COLOR_BG
    call gfx_text

    ; Health row.
    mov bl, SU_ROW_HEALTH
    call su_row_color
    mov si, su_str_health
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 4
    mov bh, COLOR_BG
    call gfx_text

    ; Repair Toka-DOS row.
    mov bl, SU_ROW_REPAIR
    call su_row_color
    mov si, su_str_repair
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 5
    mov bh, COLOR_BG
    call gfx_text

    ; Format Drive row.
    mov bl, SU_ROW_FORMAT
    call su_row_color
    mov si, su_str_format
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 6
    mov bh, COLOR_BG
    call gfx_text

    ; Save row.
    mov bl, SU_ROW_SAVE
    call su_row_color
    mov si, su_str_save
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 7
    mov bh, COLOR_BG
    call gfx_text

    ; Discard row.
    mov bl, SU_ROW_DISCARD
    call su_row_color
    mov si, su_str_discard
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 8
    mov bh, COLOR_BG
    call gfx_text

    ; Footer hint.
    push cs
    pop ds
    mov si, su_str_hint
    mov di, 8                       ; from x=8 so the line fits inside 320 px
    mov ax, SU_MENU_Y + SU_ROW_DY * 10 + 4
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    ; Keyboard note: the non-US layouts have no dead-key composition this slice, so
    ; a key that is a dead key on real hardware emits its base character.
    push cs
    pop ds
    mov si, su_str_deadnote
    mov di, 8
    mov ax, SU_MENU_Y + SU_ROW_DY * 11 + 4
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    ret

; su_service_note: draw a one-line Repair/Format result at SI (CS string) in the
; row gap below Discard, bright cyan. Persists until the next key repaints. Sets
; DS = CS. Clobbers AX, DI.
su_service_note:
    push cs
    pop ds
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 9
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
    ret

; su_row_color: BL = selectable-row index on entry; returns BL = the highlight
; color if that row is the cursor, else the plain row color. Restores DS = CS for
; the caller's gfx_text. Clobbers AX, CX.
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
    pop ds                          ; restore DS = CS for the caller's gfx_text
    ret

; su_draw_mode_value: draw the working GSW mode's name (386/486/586) at the value
; column on the GSW row. Reads SU_WORKING_MODE; uses DS = CS for the string.
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
    mov ax, SU_MENU_Y + SU_ROW_DY * 2
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
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
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
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
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
    pop di
    add di, 16
    pop si
    pop dx
    pop cx
    ret

; su_put_colon: draw a ':' separator at DI on the Time row, advancing DI by 8 px.
su_put_colon:
    push si
    push cs
    pop ds
    mov si, su_str_colon
    push di
    mov ax, SU_MENU_Y
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
    pop di
    add di, 8
    pop si
    ret

; ============================ Time sub-page ============================
; su_time_page: edit the working clock. Left/Right move the highlighted field,
; Up/Down (and Enter) bump it, Esc returns to the main menu. The first change sets
; SU_TIME_EDITED so su_commit knows to write the RTC. DS bounces between 0 and CS.
su_time_page:
    sti
    call su_time_draw
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

; su_time_draw: paint the time sub-page. Heading, the editable HH:MM:SS line and
; the DD/MM/YY date line with the highlighted field in cyan, and a hint footer.
su_time_draw:
    call su_enter_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds
    mov si, su_str_time_title
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY
    mov bl, SU_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text
    ; Time line: HH:MM:SS, each field colored by whether it is highlighted.
    mov di, SU_MENU_X
    mov bp, SU_MENU_Y + SU_ROW_DY
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
    mov bp, SU_MENU_Y + SU_ROW_DY * 3
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
    ; Hint.
    push cs
    pop ds
    mov si, su_str_time_hint
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y + SU_ROW_DY * 6
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    ret

; su_time_field: draw the two-digit value at segment-0 offset SI at pixel DI/BP,
; in cyan if field index BL is the highlighted field else white. Advances DI by
; 16 px. DS bounces 0/CS internally. Clobbers AX, CX.
su_time_field:
    push si
    push dx
    xor ax, ax
    mov ds, ax                      ; DS = 0 for the field state, value, and buffer
    ; DL = chosen foreground: cyan when this field is highlighted, else white.
    push bx
    mov bx, SU_TIME_FIELD
    mov dh, [bx]                    ; DH = highlighted field index
    pop bx
    mov dl, SU_COLOR_ROW
    cmp bl, dh
    jne .have_color
    mov dl, SU_COLOR_HILITE
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
    mov bh, COLOR_BG
    mov ax, bp                      ; y
    call gfx_text                   ; DS = 0, SI = buffer
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
    mov bl, SU_COLOR_ROW
    mov bh, COLOR_BG
    mov ax, bp
    call gfx_text
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
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    mov ax, BDA_SEG
    mov ds, ax                      ; probe contract: DS = BDA on entry
    call %2                         ; AL = HRES_* on return
    push cs
    pop ds                          ; DS = CS for the status string
    cmp al, HRES_FAIL
    je %%fail
    mov si, su_str_pass
    mov bl, SU_COLOR_PASS
    jmp %%put
%%fail:
    mov si, su_str_fail
    mov bl, SU_COLOR_FAIL
%%put:
    mov di, SU_VALUE_X
    mov ax, SU_MENU_Y + SU_ROW_DY * %3
    mov bh, COLOR_BG
    call gfx_text
%endmacro

; su_periph_page: re-run each POST probe live (they are non-destructive) and list
; its PASS/FAIL or MEASURE value. Esc returns. The probe handlers follow the POST
; contract: AL = HRES_PASS/FAIL/MEASURE on return; MEASURE leaves SI = a segment-0
; ASCIIZ value offset. We enter each with DS = BDA_SEG to match that contract.
su_periph_page:
    call su_enter_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds
    mov si, su_str_periph_title
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY
    mov bl, SU_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text

    ; One row per probe. SU_PROBE macro: name string, handler label, row index.
    SU_PROBE su_str_p_lotura, probe_lotura, 0
    SU_PROBE su_str_p_8042,   probe_kbd8042, 1
    SU_PROBE su_str_p_pit,    probe_pit, 2
    SU_PROBE su_str_p_com1,   probe_serial, 3
    SU_PROBE su_str_p_sbdsp,  probe_sbdsp, 4
    SU_PROBE su_str_p_opl,    probe_opl, 5
    SU_PROBE su_str_p_margo,  probe_margo, 6

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

; ============================ Health sub-page ============================
; su_health_page: show the voltage rails and temperatures with a small per-refresh
; jitter from the LFSR. Esc returns; any other key re-jitters and repaints at once.
; The values are fixed bases plus a signed wobble so the page looks alive without a
; real hardware monitor port.
;
; Non-blocking poll loop (Bug 5), modeled on the bootbox loop: each pass peeks the
; keyboard, then checks the BDA timer tick word at 0040:006C. When the tick has
; advanced past SU_HEALTH_TICK the sensors are re-read (re-jittered) and the page
; redraws, so the readouts animate on the timer with no keypress. The tick is then
; stored so the next advance triggers the next frame. This replaces the old
; hlt-based wait, matching the peek model the rest of the setup pages now use.
su_health_page:
    sti                             ; the keyboard ring and the BDA tick run on IRQs
    call su_health_draw
    call su_health_seed_tick
.loop:
    mov ah, 0x01
    call kb_getkey                  ; peek: CF=1 if the ring is empty
    jc .tick
    cmp ah, SC_ESC
    je .back
    xor ah, ah
    call kb_getkey                  ; drain the key; any key re-jitters and repaints
    call su_health_draw
    call su_health_seed_tick
    jmp .loop
.tick:
    ; Redraw once the BDA tick has advanced, so the readouts animate ~18 times a
    ; second without input. The tick word is volatile (INT 08h owns it), so read it
    ; under DS = BDA then bounce back to segment 0 for SU_HEALTH_TICK.
    push ds
    push ax
    push bx
    mov bx, BDA_SEG
    mov ds, bx
    mov ax, [BDA_TICKS]             ; low word of the running tick count
    xor bx, bx
    mov ds, bx
    cmp ax, [SU_HEALTH_TICK]
    pop bx
    pop ax
    pop ds
    je .loop                        ; tick unchanged: keep polling
    call su_health_draw
    call su_health_seed_tick
    jmp .loop
.back:
    xor ah, ah
    call kb_getkey                  ; consume the Esc make
    ret

; su_health_seed_tick: store the current BDA tick low word so the health loop can
; tell when about a second has elapsed. DS bounces 0040 -> 0.
su_health_seed_tick:
    push ds
    push ax
    push bx
    mov bx, BDA_SEG
    mov ds, bx
    mov ax, [BDA_TICKS]
    xor bx, bx
    mov ds, bx
    mov [SU_HEALTH_TICK], ax
    pop bx
    pop ax
    pop ds
    ret

; su_lfsr_step: advance the 16-bit Galois LFSR at SU_LFSR_LO and return AL = a
; small unsigned jitter in 0..4. The reading wobbles upward from its nominal each
; refresh, which is enough to look like a live sensor and avoids an underflow when
; the nominal fraction is 0. DS = 0. Clobbers AX, BX, DX.
su_lfsr_step:
    xor bx, bx
    mov ds, bx
    mov bx, SU_LFSR_LO
    mov ax, [bx]
    shr ax, 1
    jnc .no_xor
    xor ax, 0xb400                  ; taps 16,14,13,11 (a maximal-length set)
.no_xor:
    mov [bx], ax
    ; Map the low bits to 0..4: ax mod 5.
    and ax, 0x000f
    xor dx, dx
    mov bx, 5
    div bx                          ; DX = ax % 5 in 0..4
    mov ax, dx
    ret

; su_health_draw: paint the health page. Each rail or temperature is a label plus a
; jittered value. su_draw_rail / su_draw_temp do one line each; the labels and the
; row Y are passed in registers so the page is a flat list of calls.
su_health_draw:
    call su_enter_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds
    mov si, su_str_health_title
    mov di, SU_MENU_X
    mov ax, SU_MENU_Y - SU_ROW_DY
    mov bl, SU_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text

    ; Rails. su_draw_rail: SI = label, DX = prefix string ("<sign>WHOLE."),
    ; CL = base hundredths, BP = row Y. The hundredths get the LFSR jitter.
    mov si, su_str_r_5v
    mov dx, su_str_v_5
    mov cl, 0
    mov bp, SU_MENU_Y
    call su_draw_rail
    mov si, su_str_r_33v
    mov dx, su_str_v_33
    mov cl, 30
    mov bp, SU_MENU_Y + SU_ROW_DY
    call su_draw_rail
    mov si, su_str_r_12v
    mov dx, su_str_v_12
    mov cl, 0
    mov bp, SU_MENU_Y + SU_ROW_DY * 2
    call su_draw_rail
    mov si, su_str_r_n12v
    mov dx, su_str_v_n12
    mov cl, 0
    mov bp, SU_MENU_Y + SU_ROW_DY * 3
    call su_draw_rail
    mov si, su_str_r_vcore
    mov dx, su_str_v_vcore
    mov cl, 90
    mov bp, SU_MENU_Y + SU_ROW_DY * 4
    call su_draw_rail

    ; Temperatures. su_draw_temp: SI = label, CL = base degrees, BP = row Y.
    mov si, su_str_t_cpu
    mov cl, 42
    mov bp, SU_MENU_Y + SU_ROW_DY * 6
    call su_draw_temp
    mov si, su_str_t_sys
    mov cl, 35
    mov bp, SU_MENU_Y + SU_ROW_DY * 7
    call su_draw_temp
    ret

; su_draw_rail: draw one voltage rail line. SI = CS label, DX = CS prefix string
; ("<sign>WHOLE.", e.g. "+5."), CL = base hundredths, BP = row Y. Builds
; "<prefix>FF V" in BIOS_VALBUF and draws label + value. Preserves caller registers.
su_draw_rail:
    pusha
    ; Label.
    push cs
    pop ds
    mov di, SU_MENU_X
    mov ax, bp
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text                   ; SI = label
    ; Copy the prefix into BIOS_VALBUF, tracking the free index in DI.
    push cs
    pop ds
    mov si, dx                      ; prefix source
    mov di, BIOS_VALBUF
.copy:
    mov al, [si]
    inc si
    test al, al
    jz .copied
    push si
    xor bx, bx
    mov ds, bx
    mov bx, di
    mov [bx], al
    pop si
    inc di
    push cs
    pop ds
    jmp .copy
.copied:
    ; Two fraction digits = (base + jitter), then " V".
    call su_lfsr_step               ; AL = 0..4 (DS = 0 after)
    add al, cl                      ; base hundredths + jitter
    mov ah, 0
    mov bl, 10
    div bl                          ; AL = tens, AH = units
    add al, '0'
    add ah, '0'
    mov bx, di
    mov [bx], al
    mov byte [bx + 1], ah
    mov byte [bx + 2], ' '
    mov byte [bx + 3], 'V'
    mov byte [bx + 4], 0
    ; Draw the value.
    mov si, BIOS_VALBUF
    mov di, SU_VALUE_X
    mov ax, bp
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text                   ; DS = 0, SI = buffer
    popa
    ret

; su_draw_temp: draw one temperature line. SI = CS label, CL = base degrees, BP =
; row Y. Builds "<DD> C" in BIOS_VALBUF. Preserves caller registers.
su_draw_temp:
    pusha
    push cs
    pop ds
    mov di, SU_MENU_X
    mov ax, bp
    mov bl, SU_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text                   ; SI = label
    call su_lfsr_step               ; AL = 0..4 (DS = 0 after)
    ; Clamp the temperature wobble to +/-1 degree: (AL mod 3) - 1 gives -1/0/+1,
    ; tighter than the voltage rails, which keep their wider raw ripple.
    mov ah, 0
    mov bl, 3
    div bl                          ; AH = AL mod 3 (0..2)
    mov al, ah
    dec al                          ; -1, 0, or +1
    add al, cl                      ; base degrees +/- 1
    mov ah, 0
    mov bl, 10
    div bl                          ; AL = tens, AH = units
    add al, '0'
    add ah, '0'
    mov [BIOS_VALBUF + 0], al
    mov [BIOS_VALBUF + 1], ah
    mov byte [BIOS_VALBUF + 2], ' '
    mov byte [BIOS_VALBUF + 3], 'C'
    mov byte [BIOS_VALBUF + 4], 0
    mov si, BIOS_VALBUF
    mov di, SU_VALUE_X
    mov ax, bp
    mov bl, SU_COLOR_VALUE
    mov bh, COLOR_BG
    call gfx_text
    popa
    ret

; ============================ Strings ============================
su_str_title:        db 'IZARRA 3000 SETUP', 0
su_str_time:         db 'TIME', 0
su_str_layout:       db 'KEYBOARD', 0
su_str_gsw:          db 'CPU MODE', 0
su_str_periph:       db 'PERIPHERALS', 0
su_str_health:       db 'HEALTH', 0
su_str_repair:       db 'REPAIR TOKA-DOS', 0
su_str_format:       db 'FORMAT DRIVE', 0
su_str_repaired:     db 'Toka-DOS repaired', 0
su_str_formatted:    db 'Drive formatted', 0
su_str_svc_fail:     db 'No Toka-DOS image', 0
su_str_save:         db 'SAVE AND EXIT', 0
su_str_discard:      db 'DISCARD AND EXIT', 0
su_str_hint:         db 'Up/Dn move  L/R edit  Enter  Esc back', 0
su_str_deadnote:     db 'Note: dead keys emit base character', 0
; POST hotkey feedback (Bug 4). Drawn on the white POST field, so kept short.
su_str_enter_setup:  db 'Entering setup...', 0
su_str_enter_boot:   db 'Opening boot menu...', 0
su_str_colon:        db ':', 0
su_str_slash:        db '/', 0

su_str_mode386:      db '386', 0
su_str_mode486:      db '486', 0
su_str_mode586:      db '586', 0

; Keyboard layout names, indexed by CMOS 0x10 (0 en-US .. 5 IT).
su_str_lay_us:       db 'en-US', 0
su_str_lay_uk:       db 'UK', 0
su_str_lay_es:       db 'ES', 0
su_str_lay_fr:       db 'FR', 0
su_str_lay_de:       db 'DE', 0
su_str_lay_it:       db 'IT', 0
su_layout_names:     dw su_str_lay_us, su_str_lay_uk, su_str_lay_es
                     dw su_str_lay_fr, su_str_lay_de, su_str_lay_it

su_str_time_title:   db 'SET TIME AND DATE', 0
su_str_time_hint:    db 'L/R field  Up/Dn change  Esc back', 0

su_str_periph_title: db 'PERIPHERAL CHECK', 0
su_str_pass:         db 'PASS', 0
su_str_fail:         db 'FAIL', 0
su_str_p_lotura:     db 'LOTURA', 0
su_str_p_8042:       db '8042 KBD', 0
su_str_p_pit:        db 'PIT TIMER', 0
su_str_p_com1:       db 'COM1 UART', 0
su_str_p_sbdsp:      db 'SB DSP', 0
su_str_p_opl:        db 'OPL FM', 0
su_str_p_margo:      db 'MARGO VGA', 0

su_str_health_title: db 'SYSTEM HEALTH', 0
su_str_r_5v:         db '+5V RAIL', 0
su_str_r_33v:        db '+3.3V RAIL', 0
su_str_r_12v:        db '+12V RAIL', 0
su_str_r_n12v:       db '-12V RAIL', 0
su_str_r_vcore:      db 'VCORE', 0
su_str_t_cpu:        db 'CPU TEMP', 0
su_str_t_sys:        db 'SYS TEMP', 0
; Rail value prefixes: "<sign>WHOLE." The two jittered hundredths and " V" follow.
su_str_v_5:          db '+5.', 0
su_str_v_33:         db '+3.', 0
su_str_v_12:         db '+12.', 0
su_str_v_n12:        db '-12.', 0
su_str_v_vcore:      db '+2.', 0
