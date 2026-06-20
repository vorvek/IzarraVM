; izbios-boot.inc - STREAM E: the Tab boot menu and the INT 19h bootstrap. After
; POST the core calls setup_maybe_enter (which opens this menu on Tab) and then
; boot_entry. boot_entry reads the chosen device from BOOT_CHOICE and dispatches:
;   0 floppy  -> the floppy path below
;   1 disk    -> Toka-DOS on C:, not installed yet
;   2 CD-ROM  -> unavailable
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
; right after BOOT_CHOICE (0x052E), so it stays clear of the Stream C/D scratch.
%define BOOT_MENU_CURSOR (BIOS_SCRATCH + 0x0f)   ; 0x052F

; Navigation make codes (Set 1).
%define BM_SC_ESC        0x01
%define BM_SC_ENTER      0x1c
%define BM_SC_UP         0x48
%define BM_SC_DOWN       0x50

; boot_menu: the Tab boot-device menu. Seeds the highlight from CMOS 0x11 (boot
; order), draws Floppy / Disk / CD-ROM, and loops on Up/Down/Enter/Esc. Enter on
; Floppy stores BOOT_DEV_FLOPPY and returns so boot_entry boots it. Enter on Disk
; shows "Toka-DOS not installed yet" and stays on the menu. CD-ROM is unavailable.
; Esc leaves the highlight at the seeded default. DS bounces between 0 and CS.
; Clobbers all caller-saved registers (core calls this directly).
boot_menu:
    ; Seed the highlighted entry from the persisted boot order in CMOS 0x11.
    ; 0 floppy-first -> Floppy, 1 disk-first -> Disk; anything else -> Floppy.
    mov al, BOOT_CMOS_ORDER
    out 0x70, al
    in al, 0x71
    cmp al, 1
    je .seed_disk
    mov al, BM_ROW_FLOPPY
    jmp .seed_store
.seed_disk:
    mov al, BM_ROW_DISK
.seed_store:
    xor cx, cx
    mov ds, cx
    mov [BOOT_MENU_CURSOR], al
    call bm_draw
.loop:
    xor ah, ah
    call kb_getkey                  ; blocking read: AH=scancode, AL=ascii
    cmp ah, BM_SC_UP
    je .up
    cmp ah, BM_SC_DOWN
    je .down
    cmp ah, BM_SC_ENTER
    je .enter
    cmp ah, BM_SC_ESC
    je .back
    jmp .loop
.up:
    call bm_cursor_dec
    call bm_draw
    jmp .loop
.down:
    call bm_cursor_inc
    call bm_draw
    jmp .loop
.enter:
    xor cx, cx
    mov ds, cx
    mov al, [BOOT_MENU_CURSOR]
    cmp al, BM_ROW_FLOPPY
    je .pick_floppy
    cmp al, BM_ROW_DISK
    je .pick_disk
    ; CD-ROM: unavailable, stay on the menu.
    push cs
    pop ds
    mov si, bm_str_cd_msg
    call bm_note
    jmp .loop
.pick_disk:
    ; Disk boots Toka-DOS on C:, which is not built yet. Report and stay put.
    push cs
    pop ds
    mov si, bm_str_disk_msg
    call bm_note
    jmp .loop
.pick_floppy:
    xor cx, cx
    mov ds, cx
    mov byte [BOOT_CHOICE], BOOT_DEV_FLOPPY
    ret
.back:
    ; Esc leaves BOOT_CHOICE at the boot-time default seeded in bios_start.
    ret

; bm_cursor_dec / bm_cursor_inc: move the highlight, wrapping. DS = 0. Clobbers
; AX, BX.
bm_cursor_dec:
    xor ax, ax
    mov ds, ax
    mov bx, BOOT_MENU_CURSOR
    mov al, [bx]
    test al, al
    jnz .ok
    mov al, BM_ROW_COUNT
.ok:
    dec al
    mov [bx], al
    ret
bm_cursor_inc:
    xor ax, ax
    mov ds, ax
    mov bx, BOOT_MENU_CURSOR
    mov al, [bx]
    inc al
    cmp al, BM_ROW_COUNT
    jb .ok
    xor al, al
.ok:
    mov [bx], al
    ret

; bm_draw: paint the boot menu from scratch. Switch to mode 13h on a black field,
; draw the title, the three entries (highlighting the cursor row), and a hint.
; Sets DS = CS for the ROM strings; reads BOOT_MENU_CURSOR from segment 0.
bm_draw:
    call bm_enter_mode13h
    mov al, COLOR_BG
    call gfx_clear
    push cs
    pop ds                          ; DS = CS for the label strings

    ; Title.
    mov si, bm_str_title
    mov di, BM_MENU_X
    mov ax, BM_TITLE_Y
    mov bl, BM_COLOR_HEAD
    mov bh, COLOR_BG
    call gfx_text

    ; Floppy entry.
    mov bl, BM_ROW_FLOPPY
    call bm_row_color
    mov si, bm_str_floppy
    mov di, BM_MENU_X
    mov ax, BM_FIRST_Y
    mov bh, COLOR_BG
    call gfx_text

    ; Disk entry (label, then the "not available yet" note in grey).
    mov bl, BM_ROW_DISK
    call bm_row_color
    mov si, bm_str_disk
    mov di, BM_MENU_X
    mov ax, BM_FIRST_Y + BM_ROW_DY
    mov bh, COLOR_BG
    call gfx_text
    push cs
    pop ds
    mov si, bm_str_disk_tag
    mov di, BM_MENU_X + 13 * 8
    mov ax, BM_FIRST_Y + BM_ROW_DY
    mov bl, BM_COLOR_NOTE
    mov bh, COLOR_BG
    call gfx_text

    ; CD-ROM entry (label, then the unavailable note in grey).
    mov bl, BM_ROW_CDROM
    call bm_row_color
    mov si, bm_str_cdrom
    mov di, BM_MENU_X
    mov ax, BM_FIRST_Y + BM_ROW_DY * 2
    mov bh, COLOR_BG
    call gfx_text
    push cs
    pop ds
    mov si, bm_str_cd_tag
    mov di, BM_MENU_X + 13 * 8
    mov ax, BM_FIRST_Y + BM_ROW_DY * 2
    mov bl, BM_COLOR_NOTE
    mov bh, COLOR_BG
    call gfx_text

    ; Hint footer.
    push cs
    pop ds
    mov si, bm_str_hint
    mov di, 8
    mov ax, BM_HINT_Y
    mov bl, BM_COLOR_INFO
    mov bh, COLOR_BG
    call gfx_text
    ret

; bm_row_color: BL = entry index on entry; returns BL = the highlight color if that
; entry is the cursor, else the plain row color. Restores DS = CS for the caller's
; gfx_text. Clobbers AX, CX.
bm_row_color:
    push cx
    mov cl, bl                      ; save the input row index across the DS:BX load
    xor ax, ax
    mov ds, ax
    mov bx, BOOT_MENU_CURSOR
    mov al, [bx]
    cmp al, cl
    mov bl, BM_COLOR_ROW
    jne .done
    mov bl, BM_COLOR_HILITE
.done:
    pop cx
    push cs
    pop ds                          ; restore DS = CS for the caller's gfx_text
    ret

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
; boot_entry: dispatch on BOOT_CHOICE. Floppy boots; Disk and CD-ROM report and
; return so the machine idles (the Tab menu handles them interactively, but a
; non-interactive boot lands here with whatever default was seeded).
boot_entry:
    xor ax, ax
    mov ds, ax
    mov al, [BOOT_CHOICE]
    cmp al, BOOT_DEV_DISK
    je .disk
    cmp al, BOOT_DEV_CDROM
    je .cdrom
    ; fall through: floppy.

.floppy:
    ; Reset the disk system on drive 0 (floppy A:).
    xor ax, ax                  ; AH=00 reset
    xor dx, dx                  ; DL=00 drive A:
    int 0x13

    ; Read one sector from CHS(0,0,1) into 0000:7C00.
    xor ax, ax
    mov es, ax                  ; ES = 0
    mov bx, 0x7c00              ; ES:BX = 0000:7C00 destination
    mov ax, 0x0201             ; AH=02 read, AL=01 one sector
    mov cx, 0x0001             ; CH=0 cylinder, CL=1 sector (sectors are 1-based)
    xor dx, dx                  ; DH=0 head, DL=00 drive A:
    stc                         ; preset CF: with no media the host INT 13h is a
                                ; no-op, so a left-set CF means "no disk, do not boot"
    int 0x13
    jc .read_error

    ; Hand the boot drive number to the boot sector and jump to it. No signature
    ; check: a clean read is enough to enter the loaded code. DS and ES start at 0
    ; (segment 0), matching how period boot sectors expect to be entered.
    xor ax, ax
    mov ds, ax
    mov es, ax
    xor dx, dx
    mov dl, 0                   ; DL = boot drive (A:)
    jmp 0x0000:0x7c00

.disk:
    ; Disk boots Toka-DOS on C:, which is not built yet. Report and return.
    mov si, bm_str_disk_msg
    call boot_status_line
    ret

.cdrom:
    ; No CD-ROM drive is present. Report and return.
    mov si, bm_str_cd_msg
    call boot_status_line
    ret

.read_error:
    ; The floppy read failed. Print a short message and fall back to the caller's
    ; idle loop so the machine stays on rather than appearing to power off.
    mov si, str_boot_error
    call boot_status_line
    ret

; boot_status_line: draw the ROM string at CS:SI on the next free console row of the
; graceful POST screen, red on white. DS = 0 on entry (boot_entry's segment).
; Clobbers AX, BX, DI, SI, ES.
boot_status_line:
    push cs
    pop ds                      ; DS = CS so DS:SI reaches the ROM string
    xor ax, ax
    mov es, ax                  ; ES = 0 for the console row word in segment 0
    mov ax, [es:CONSOLE_ROW_Y]  ; next free console row
    add ax, STATUS_LINE_DY
    mov di, STATUS_X
    mov bl, GFX_RED
    mov bh, GFX_WHITE
    call gfx_text
    ret

; --- Strings ----------------------------------------------------------------
bm_str_title:    db 'BOOT MENU', 0
bm_str_floppy:   db 'Floppy', 0
bm_str_disk:     db 'Disk', 0
bm_str_disk_tag: db '(not available yet)', 0
bm_str_cdrom:    db 'CD-ROM', 0
bm_str_cd_tag:   db '(unavailable)', 0
bm_str_hint:     db 'Up/Dn move   Enter select   Esc cancel', 0
bm_str_disk_msg: db 'Toka-DOS not installed yet', 0
bm_str_cd_msg:   db 'CD-ROM drive not available', 0

str_boot_error:  db 'FLOPPY READ ERROR', 0
