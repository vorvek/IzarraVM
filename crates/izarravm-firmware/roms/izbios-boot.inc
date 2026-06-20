; izbios-boot.inc - the INT 19h bootstrap. After POST the core calls boot_entry.
; The floppy path resets the disk system, reads the first sector to 0000:7C00,
; and far jumps there with DL set to the boot drive. There is NO 0xAA55 signature
; check: the IBM PC/XT BIOS boots a floppy unconditionally, and period booters
; (the Wizardry boot sector carries 0x90D0 at offset 0x1FE, not 0xAA55) rely on
; that. Only a disk read error (CF set) aborts to the caller.
;
; INT 13h is serviced host-side, so the int instructions below are intercepted by
; the emulator. boot_entry is plain code reached from the core; it does not need
; an IVT slot. On a read error it returns to the caller, which idles.

boot_entry:
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
    ; The floppy read failed. Print a short message and fall back to the caller's
    ; idle loop so the machine stays on rather than appearing to power off.
    push cs
    pop ds                      ; DS = CS so DS:SI reaches the ROM string
    xor ax, ax
    mov es, ax                  ; ES = 0 for the console row word in segment 0
    mov ax, [es:CONSOLE_ROW_Y]  ; next free console row
    add ax, STATUS_LINE_DY
    mov di, STATUS_X
    mov si, str_boot_error
    mov bl, COLOR_FAIL
    mov bh, COLOR_BG
    call gfx_text
    ret

str_boot_error: db 'FLOPPY READ ERROR', 0
