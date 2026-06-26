; izbios-codepage.inc - load a code-page font into the VGA character generator.
; The host banks the requested page into a 4 KB window at C400:0000 when we write
; a selector to Lotura port 0xE7. sel = cp*3 + size_index (size 0=8x16, 1=8x14,
; 2=8x8). We pick the size from the BDA character height (0040:0085).
%define CPFONT_WINDOW_SEG  0xC400
%define CPFONT_SELECT_PORT 0xE7
; CMOS 0x11 holds the code-page index: 0=CP437, 1=CP850, 2=CP860, 3=CP863, 4=CP865.
%define CP_CMOS_INDEX      0x11

; load_codepage_font: AL = code-page index (0..4, clamped to 0 if out of range).
; Banks the matching font page for the current text-mode character height into
; the host window at C400:0000, then asks the BIOS video service (INT 10h
; AH=11h AL=00h) to copy it into the VGA character generator. The INT 10h call
; is intercepted by the host emulator which writes the data directly into the
; font tables that the text renderer reads, so this works regardless of whether
; the low-level plane-2 register sequence is active. Saves and restores all
; registers it modifies.
load_codepage_font:
    pusha
    push es

    ; Clamp the code-page index (AL) to 0..4.
    cmp al, 5
    jb .cp_ok
    xor al, al
.cp_ok:
    ; Save cp index in BL.
    mov bl, al

    ; Determine size_index and bytes-per-char from BDA 0040:0085 (char height).
    ; Default to 8x16 (size_index=0, bh=16) when the field is not 8 or 14.
    push ds
    mov ax, BDA_SEG
    mov ds, ax
    mov ax, [0x85]          ; BDA char height (word)
    pop ds

    ; AX = height. Map to (size_index in AH, bytes-per-char in BH).
    cmp ax, 14
    je .h14
    cmp ax, 8
    je .h8
    mov bh, 16
    mov ah, 0               ; size_index 0 (8x16)
    jmp .have_size
.h14:
    mov bh, 14
    mov ah, 1               ; size_index 1 (8x14)
    jmp .have_size
.h8:
    mov bh, 8
    mov ah, 2               ; size_index 2 (8x8)
.have_size:
    ; Compute selector: sel = cp*3 + size_index. AL = cp*3 + size_index.
    mov al, bl              ; AL = cp index
    add al, al              ; AL = cp*2
    add al, bl              ; AL = cp*3
    add al, ah              ; AL = cp*3 + size_index
    out CPFONT_SELECT_PORT, al  ; host copies the matching font page into C400:0000

    ; Load the banked font page into the VGA character generator via INT 10h.
    ; AH=11h AL=00h: load user font from ES:BP, BH=bytes/char, CX=256, DX=0,
    ; BL=0 (font block 0). The host intercepts this INT and calls load_font_table()
    ; which updates the internal font cache the text renderer uses.
    mov ax, CPFONT_WINDOW_SEG
    mov es, ax
    xor bp, bp              ; ES:BP -> C400:0000 (the banked window)
    xor dx, dx              ; first character = 0
    mov cx, 256             ; all 256 glyphs
    xor bl, bl              ; font block 0
    mov ah, 0x11
    mov al, 0x00
    int 0x10

    pop es
    popa
    ret

; boot_load_codepage: read CMOS 0x11 for the boot code-page index and call
; load_codepage_font. Called once at boot before the OS handoff. Saves/restores
; all registers through load_codepage_font's pusha/popa.
boot_load_codepage:
    push ax
    mov al, CP_CMOS_INDEX
    out 0x70, al            ; select CMOS byte 0x11
    in al, 0x71             ; read the code-page index
    call load_codepage_font ; clamping is inside load_codepage_font
    pop ax
    ret
