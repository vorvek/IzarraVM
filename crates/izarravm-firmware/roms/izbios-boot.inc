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
; boot_entry: dispatch on BOOT_CHOICE. Floppy boots; Disk and CD-ROM report and
; return so the machine idles (the Tab menu handles them interactively, but a
; non-interactive boot lands here with whatever default was seeded).
boot_entry:
    ; Do NOT repaint the POST screen here. The graphical screen is already on the
    ; LFB with each component icon lit grey->colour from its probe; re-running
    ; gfx_post_screen would re-blit the grey background over the lit icons (the
    ; probes do not run again), losing the reveal. The boot_status_line messages on
    ; the no-media paths are legacy mode-13h text (invisible in the LFB mode), so
    ; the clean-console reset is cosmetic; CONSOLE_ROW_Y is still seeded for it.
    ; (Phase 4 owns restoring the LFB screen after the mode-13h Tab/Del menus.)
    xor ax, ax
    mov ds, ax
    mov word [CONSOLE_ROW_Y], POST_HINT_Y
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

.read_error:
    ; The floppy read failed. Chain to the next device in the default boot order
    ; (floppy -> hard disk -> CD-ROM). Print the floppy error, then fall into the
    ; disk stub. With the HDD and CD-ROM backends still stubbed this collapses to
    ; floppy-or-idle, but the fall-through is wired so the order lights up the
    ; moment those boots become real.
    mov si, str_boot_error
    call boot_status_line
    ; fall through into .disk

.disk:
    ; Fixed-disk boot. Try a REAL hard-disk (Katea/ATA) boot first: read the MBR
    ; at CHS(0,0,1) of drive 0x80 to 0000:7C00 and enter it, exactly like the
    ; floppy path above. The MBR relocates itself and chains to the active
    ; partition's VBR, which loads the kernel. With no fixed disk present the host
    ; INT 13h is a no-op that leaves FLAGS untouched, so the preset CF survives as
    ; "no disk" and we fall through to the HLE Toka-DOS C: shim below. This is what
    ; lets the real FreeDOS kernel boot from an ATA disk while the legacy host-
    ; backed C: still works when no disk is attached.
    xor ax, ax
    mov es, ax                  ; ES = 0
    mov bx, 0x7c00             ; ES:BX = 0000:7C00 destination
    mov ax, 0x0201            ; AH=02 read, AL=01 one sector
    mov cx, 0x0001            ; CH=0 cylinder, CL=1 sector (1-based)
    mov dx, 0x0080            ; DH=0 head, DL=80h first fixed disk
    stc                        ; preset CF: no disk -> host no-op leaves it set
    int 0x13
    jc .disk_hle               ; no fixed disk attached: fall back to the HLE shim

    ; MBR read OK. Set DS=ES=0 first (the .read_error fall-through ran boot_status_line,
    ; which leaves DS pointing at a string segment), so the signature test and the boot
    ; sector both see segment 0. Then require the 55AA boot signature, exactly like the
    ; INT 19h ATA branch: a disk with none is not bootable and falls back to the HLE
    ; Toka-DOS C: shim. DL = first fixed disk so the MBR (and its VBR) know the drive.
    xor ax, ax
    mov ds, ax
    mov es, ax
    cmp word [0x7dfe], 0xaa55
    jne .disk_hle
    ; Hand the OS a clean VGA text screen before entering the MBR. The graphical
    ; POST left the Margo LFB active; a real MBR + FreeDOS kernel boots in text
    ; mode without ever setting a video mode, so without this the display stays
    ; frozen on the POST splash while the booted OS writes to the (hidden) text
    ; buffer. A real BIOS leaves mode 03h before INT 19h -- match that. Setting
    ; mode 03h via INT 10h clears the Margo latch so the VGA core drives the
    ; display (machine.rs int10_set_mode_number, modes 0x00-0x03). INT 10h may
    ; use DS/ES as scratch, so re-zero them for the boot sector afterward.
    mov ax, 0x0003
    int 0x10
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov dl, 0x80
    jmp 0x0000:0x7c00

.disk_hle:
    ; No bootable fixed disk (no 55AA MBR signature on the ATA primary master).
    ; There is no HLE fallback any more -- the Rust Toka-DOS kernel was retired --
    ; so report and idle, exactly like the no-CD-ROM path below.
    ; fall through into .disk_absent

.disk_absent:
    mov si, bm_str_disk_msg
    call boot_status_line
    ; fall through into .cdrom

.cdrom:
    ; No CD-ROM drive is present. Report and idle (return to the caller's loop).
    mov si, bm_str_cd_msg
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
bm_str_disk_msg: db 'No bootable disk', 0
bm_str_cd_msg:   db 'CD-ROM drive not available', 0

str_boot_error:  db 'FLOPPY READ ERROR', 0
