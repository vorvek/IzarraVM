; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Tab boot menu and INT 19h bootstrap. After
; POST the core calls setup_maybe_enter (which opens this menu on Tab) and then
; boot_entry. boot_entry reads the chosen device from BOOT_CHOICE and dispatches:
;   0 floppy  -> the floppy path below
;   1 disk    -> ATA fixed disk MBR
;   2 CD-ROM  -> no boot backend yet
; The floppy path resets the disk system, reads the first sector to 0000:7C00, and
; far jumps there with DL set to the boot drive. There is NO 0xAA55 signature check:
; the IBM PC/XT BIOS boots a floppy unconditionally, and period booters (the
; Wizardry boot sector carries 0x90D0 at offset 0x1FE, not 0xAA55) rely on that.
; Only a disk read error (CF set) aborts back to the caller.
;
; INT 13h is serviced host-side, so the int instructions below are intercepted by
; the emulator. boot_entry is plain code reached from the core; it does not need an
; IVT slot. On a read error it returns to the caller, which idles.

; --- Boot-menu layout (mode 13h pixels) ------------------------------------
%define BM_MENU_X        32
%define BM_TITLE_Y       40
%define BM_FIRST_Y       72
%define BM_ROW_DY        16
%define BM_HINT_Y        152
%define BM_COLOR_HEAD    0x0e          ; yellow heading
%define BM_COLOR_ROW     0x0f          ; white (unselected entry)
%define BM_COLOR_HILITE  0x0b          ; bright cyan (highlighted entry)
%define BM_COLOR_INFO    0x07          ; light grey body text
%define BM_COLOR_NOTE    0x08          ; dark grey (unavailable entry note)

; Menu entries, in highlight order.
%define BM_ROW_FLOPPY    0
%define BM_ROW_DISK      1
%define BM_ROW_CDROM     2
%define BM_ROW_COUNT     3

; Boot-device codes stored in BOOT_CHOICE.
%define BOOT_DEV_FLOPPY  0
%define BOOT_DEV_DISK    1
%define BOOT_DEV_CDROM   2

; The highlighted boot-menu entry. One byte in the free tail of BIOS_SCRATCH,
; right after BOOT_CHOICE (0x052E), so it stays clear of other scratch regions.
%define BOOT_MENU_CURSOR (BIOS_SCRATCH + 0x0f)   ; 0x052F

; Navigation make codes (Set 1).
%define BM_SC_ESC        0x01
%define BM_SC_ENTER      0x1c
%define BM_SC_UP         0x48
%define BM_SC_DOWN       0x50

; boot_menu: shim. The two-pane boxed boot+speed menu lives in izbios-bootbox.inc
; as bootbox_menu. setup-ui.inc calls this label by name (call boot_menu), so the
; label must stay resolvable here; it just forwards into the new menu. Keep the
; name; do NOT rename it.
boot_menu:
    jmp bootbox_menu

; bm_note: draw a one-line message at the hint row, first wiping the row to the
; background so a prior note is erased. DS:SI = CS-relative ASCIIZ on entry.
; Preserves caller registers.
bm_note:
    pusha
    push si
    mov di, 8
    mov si, BM_HINT_Y
    mov cx, 304
    mov dx, 8
    mov bl, COLOR_BG
    call gfx_fill_rect
    pop si                          ; the note pointer (CS-relative)
    push cs
    pop ds
    mov di, 8
    mov ax, BM_HINT_Y
    mov bl, BM_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    popa
    ret

; bm_enter_mode13h: set mode 13h, then force DAC index 0 (the menu background) to
; black. The graceful POST screen reprograms index 0 to white and INT 10h AH=00
; does not restore the DAC in this emulator, so the menu would otherwise draw onto
; a white field. Mirrors su_enter_mode13h; every other index it uses is at its
; mode-13h default.
bm_enter_mode13h:
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

; --- INT 19h bootstrap ------------------------------------------------------
; The machine-side INT 19h planner owns the primary and fallback policy for both
; power-on and guest-invoked bootstrap requests.
boot_entry:
    int 0x19
    ret

; --- Strings ----------------------------------------------------------------
bm_str_title:    db 'BOOT MENU', 0
bm_str_floppy:   db 'Floppy', 0
bm_str_disk:     db 'Disk', 0
bm_str_disk_tag: db '(not available yet)', 0
bm_str_cdrom:    db 'CD-ROM', 0
bm_str_cd_tag:   db '(unavailable)', 0
bm_str_hint:     db 'Up/Dn move   Enter select   Esc cancel', 0
