; mbr.asm - a minimal Master Boot Record for the Katea static FAT32 HDD image.
;
; FreeDOS SYS does not ship a standalone MBR (it writes only the partition VBR
; and relies on the disk already carrying one), so we author a tiny standard MBR
; here. Task 1's INT 19h loads LBA 0 (this code) to 0000:7C00 with DL=80h and
; jumps to it; this MBR then loads the active partition's VBR and chains to it.
;
; Convention (matches IBM/MS-DOS MBRs): relocate self from 0000:7C00 up to
; 0000:0600, scan the 4-entry partition table for the one with the active flag
; (0x80), read its first sector (partition start LBA, from the entry's RelSect
; dword) to 0000:7C00 via INT 13h AH=42h (EDD/LBA), verify the 0x55AA signature,
; then jump to it with DL = boot drive and DS:SI -> the active partition entry
; (the FreeDOS FAT32 VBR ignores SI but the convention is harmless to honor).
;
; Assemble: nasm mbr.asm -o mbr.bin   (exactly 512 bytes, 0x55AA at 510/511).

        org     0x600           ; we relocate to here before running

%define ORIG    0x7c00          ; where the BIOS/INT 19h loaded us
%define RELOC   0x0600          ; where we move ourselves
%define PTABLE  0x7be           ; partition table offset within the MBR
%define VBR     0x7c00          ; where we load the active partition's VBR

start:
        cli
        xor     ax, ax
        mov     ds, ax
        mov     es, ax
        mov     ss, ax
        mov     sp, ORIG        ; stack just below the load address
        ; Relocate this MBR from 0000:7C00 to 0000:0600 so loading the VBR back
        ; at 0x7C00 does not overwrite us mid-execution.
        mov     si, ORIG
        mov     di, RELOC
        mov     cx, 256         ; 512 bytes = 256 words
        cld
        rep     movsw
        ; far jump into the relocated copy (CS=0, IP=below in the moved image)
        jmp     0:reloc_entry

reloc_entry:
        sti
        mov     [boot_drive], dl    ; BIOS-passed boot drive (80h)

        ; --- find the active partition entry (active flag 0x80) ---
        mov     si, RELOC + (PTABLE - 0x600) ; -> first table entry in our copy
        mov     cx, 4
.scan:
        mov     al, [si]            ; boot indicator
        cmp     al, 0x80
        je      .found
        cmp     al, 0x00            ; 0x00 = inactive; anything else is invalid
        jne     .bad_table
        add     si, 16
        loop    .scan
        ; no active partition found
        mov     si, msg_noactive
        jmp     error

.bad_table:
        mov     si, msg_badtable
        jmp     error

.found:
        ; SI -> active partition entry. Its RelSect (start LBA) is at offset 8.
        push    si                  ; preserve entry pointer for the chain jump
        mov     eax, [si + 8]       ; partition start LBA (dword)
        mov     [dap_lba], eax

        ; --- read the VBR (1 sector) via INT 13h AH=42h (EDD/LBA) ---
        mov     ah, 0x42
        mov     dl, [boot_drive]
        mov     si, dap
        int     0x13
        jc      .read_error

        pop     si                  ; restore active partition entry pointer

        ; verify the VBR boot signature
        cmp     word [VBR + 0x1fe], 0xaa55
        jne     .no_signature

        ; chain to the VBR: DL = boot drive, DS:SI -> active partition entry
        mov     dl, [boot_drive]
        jmp     0:VBR

.read_error:
        pop     si
        mov     si, msg_readerr
        jmp     error
.no_signature:
        mov     si, msg_nosig
        jmp     error

; --- print DS:SI (NUL-terminated) via INT 10h teletype, then halt ---
error:
.next:
        lodsb
        test    al, al
        jz      .halt
        mov     ah, 0x0e
        mov     bx, 0x0007
        int     0x10
        jmp     .next
.halt:
        cli
        hlt
        jmp     .halt

; --- Disk Address Packet for AH=42h (read 1 sector to 0000:7C00) ---
dap:
        db      0x10            ; packet size = 16
        db      0               ; reserved
        dw      1               ; sector count
        dw      VBR             ; buffer offset
        dw      0               ; buffer segment
dap_lba:
        dd      0               ; starting LBA low
        dd      0               ; starting LBA high

boot_drive:     db 0x80

msg_noactive:   db "No active partition", 0
msg_badtable:   db "Bad partition table", 0
msg_readerr:    db "VBR read error", 0
msg_nosig:      db "VBR not bootable", 0

        ; pad to the partition table (the builder stamps the real table over the
        ; zeros here at offset 0x1be); keep code strictly below 0x1be.
        times 0x1be - ($ - $$) db 0
        ; 4 * 16 bytes of partition table (filled in by the image builder)
        times 64 db 0
        dw      0xaa55
