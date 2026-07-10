; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Linear-framebuffer draw primitives for the
; proprietary Margo mode 0x150 (320x240x8). All routines preserve caller
; registers. Fills, clears, and text go through the Margo blit engine (the
; MMIO register block at MARGO_MMIO, reached via the same unreal-mode flat FS
; that unreal_init established) so the CPU never touches those pixels; the
; two bitmap blitters still poke LFB_BASE + y*LFB_PITCH + x directly (32-bit
; [fs:ebx], same pattern as ramtest_poke32). The 8x8 CP437 font (bios_font)
; and the red wordmark (bios_logo_bitmap) are shared with izbios-gfx.inc.

; Margo blit-engine MMIO registers (docs/vega/vega-programmers-guide.md).
%define MARGO_MMIO     0xe0400000
%define MG_STATUS      0x0008     ; bit 0 = BUSY
%define MG_DST_BASE    0x0100     ; VRAM byte offset (0 = the visible screen)
%define MG_DST_PITCH   0x0104
%define MG_DEPTH       0x0110     ; bytes per pixel
%define MG_DST_XY      0x0114     ; (y << 16) | x
%define MG_DIM         0x011c     ; (h << 16) | w
%define MG_FG_COLOR    0x0120
%define MG_BG_COLOR    0x0124
%define MG_ROP         0x0128     ; 0xF0 PATCOPY, 0xCC SRCCOPY
%define MG_FLAGS       0x0130
%define MG_COMMAND     0x0150     ; 0x01 FILL, 0x03 COLOR_EXPAND_DATA
%define MG_MONO_DATA   0x0160     ; glyph bit stream, one dword per row

; margo_wait: spin until the blit engine goes idle (STATUS.BUSY clear).
margo_wait:
    push eax
    push ebx
    mov ebx, MARGO_MMIO + MG_STATUS
.spin:
    mov eax, [fs:ebx]
    test al, 1
    jnz .spin
    pop ebx
    pop eax
    ret

; margo_dst_setup: latch the common destination registers for a mode-0x150
; engine operation: the visible screen at VRAM offset 0, 320-byte pitch, 8bpp,
; DST_XY from DI(x)/SI(y). Internal helper; enter with EBX = MARGO_MMIO after
; a margo_wait. Clobbers EAX.
margo_dst_setup:
    xor eax, eax
    mov [fs:ebx + MG_DST_BASE], eax
    mov eax, LFB_PITCH
    mov [fs:ebx + MG_DST_PITCH], eax
    mov eax, 1
    mov [fs:ebx + MG_DEPTH], eax
    movzx eax, si
    shl eax, 16
    mov ax, di
    mov [fs:ebx + MG_DST_XY], eax
    ret

; lfb_set_mode: install the proprietary 320x240x8 mode via VBE 4F02h.
lfb_set_mode:
    push ax
    push bx
    mov ax, 0x4f02
    mov bx, LFB_MODE
    int 0x10
    pop bx
    pop ax
    ret

; lfb_clear: fill the whole framebuffer with color index AL.
lfb_clear:
    pushad
    mov bl, al                  ; whole-screen engine FILL
    xor di, di
    xor si, si
    mov cx, LFB_W
    mov dx, LFB_H
    call lfb_fill_rect
    popad
    ret

; lfb_pixel_addr: DI=x, AX=y -> EBX = LFB linear address of (x,y).
; Scratches EDX; preserves everything else. Internal helper.
lfb_pixel_addr:
    push eax
    push edx
    movzx ebx, di               ; ebx = x
    movzx eax, ax               ; eax = y
    mov edx, LFB_PITCH
    mul edx                     ; edx:eax = y*pitch (fits 32 bits)
    add ebx, eax
    add ebx, LFB_BASE
    pop edx
    pop eax
    ret

; lfb_glyph: draw one 8x8 glyph. AL=char, DI=x, SI(word)=y, BL=fg, BH=bg.
; Internal helper used by lfb_text. Preserves caller registers.
;
; An opaque engine color expand: FG/BG latched, COLOR_EXPAND_DATA issued, then
; the eight font rows streamed through MONO_DATA (one dword per row, bits in
; the high byte, MSB first -- each row starts on a word boundary).
lfb_glyph:
    pushad
    call margo_wait
    movzx bp, al
    shl bp, 3
    add bp, bios_font           ; CS:BP -> 8 glyph rows
    push bx                     ; BL=fg / BH=bg, wanted after EBX is repointed
    mov ebx, MARGO_MMIO
    call margo_dst_setup        ; screen dst, DST_XY from DI/SI
    mov eax, 0x00080008
    mov [fs:ebx + MG_DIM], eax  ; 8x8
    pop dx                      ; DL=fg, DH=bg
    movzx eax, dl
    mov [fs:ebx + MG_FG_COLOR], eax
    movzx eax, dh
    mov [fs:ebx + MG_BG_COLOR], eax
    mov eax, 0xcc
    mov [fs:ebx + MG_ROP], eax  ; SRCCOPY of the expanded source
    xor eax, eax
    mov [fs:ebx + MG_FLAGS], eax ; opaque: clear bits paint BG_COLOR
    mov eax, 0x03
    mov [fs:ebx + MG_COMMAND], eax ; COLOR_EXPAND_DATA
    mov cx, 8                   ; stream the 8 rows
.row:
    movzx eax, byte [cs:bp]
    inc bp
    shl eax, 24                 ; row bits in the high byte
    mov [fs:ebx + MG_MONO_DATA], eax
    loop .row
    call margo_wait
    popad
    ret

; lfb_text: DS:SI=ASCIIZ, DI=x, AX=y, BL=fg, BH=bg. 8px advance per glyph.
lfb_text:
    pusha
    mov dx, ax                  ; dx = y (constant for the line)
.next:
    mov al, [si]
    test al, al
    jz .done
    inc si
    push si
    mov si, dx                  ; y (word) for lfb_glyph
    call lfb_glyph              ; AL, DI(x), SI(y), BL/BH
    pop si
    add di, 8
    jmp .next
.done:
    popa
    ret

; lfb_putdec: AX=value, DI=x, SI(word)=y, BL=fg, BH=bg. Fixed 5-digit, zero-pad.
lfb_putdec:
    pusha
    push es
    push ds
    push di
    push si
    push bx
    xor cx, cx
    mov ds, cx                  ; DS=0: BIOS_VALBUF lives in segment 0
    mov di, BIOS_VALBUF + 5
    mov byte [di], 0
    mov cx, 5
    mov bx, 10
.digit:
    dec di
    xor dx, dx
    div bx
    add dl, '0'
    mov [di], dl
    loop .digit
    mov si, di                  ; DS:SI -> 5-digit string
    pop bx
    pop ax                      ; AX = y
    pop di                      ; DI = x
    call lfb_text
    pop ds
    pop es
    popa
    ret

; lfb_fill_rect: DI=x, SI(word)=y, CX=w, DX=h, BL=color. Solid rectangle,
; an engine FILL (PATCOPY of FG_COLOR). A zero width or height latches a
; degenerate DIM and the engine paints nothing, matching the old loop.
lfb_fill_rect:
    pushad
    call margo_wait
    push bx                     ; BL = color, wanted after EBX is repointed
    mov ebx, MARGO_MMIO
    call margo_dst_setup        ; screen dst, DST_XY from DI/SI
    movzx eax, dx
    shl eax, 16
    mov ax, cx
    mov [fs:ebx + MG_DIM], eax  ; (h << 16) | w
    pop ax                      ; AL = color
    movzx eax, al
    mov [fs:ebx + MG_FG_COLOR], eax
    mov eax, 0xf0
    mov [fs:ebx + MG_ROP], eax  ; PATCOPY
    xor eax, eax
    mov [fs:ebx + MG_FLAGS], eax
    mov eax, 0x01
    mov [fs:ebx + MG_COMMAND], eax ; FILL
    call margo_wait
    popad
    ret

; lfb_box: DI=x, SI(word)=y, CX=w, DX=h, BL=interior color, BH=border color.
; A 1px border rectangle with a filled interior, built from four lfb_fill_rect
; calls (top edge, bottom edge, then the left+right border folded into two
; side slivers around a shrunk interior fill) rather than baked art, so
; setup-ui's boxed panels need no new RLE assets. The geometry lives in the
; BOX_* segment-0 scratch (mirroring lfb_blit_1bpp) so each lfb_fill_rect call
; is free to clobber DI/SI/CX/DX/BX. Boxes narrower/shorter than 2px in either
; axis skip the interior fill (nothing to fill). Preserves caller registers.
lfb_box:
    pusha
    push ds
    xor ax, ax
    mov ds, ax
    mov [BOX_X], di
    mov [BOX_Y], si
    mov [BOX_W], cx
    mov [BOX_H], dx
    mov [BOX_IN], bl             ; interior color
    mov [BOX_BD], bh             ; border color
    ; Top edge: (x, y, w, 1).
    mov di, [BOX_X]
    mov si, [BOX_Y]
    mov cx, [BOX_W]
    mov dx, 1
    mov bl, [BOX_BD]
    call lfb_fill_rect
    ; Bottom edge: (x, y+h-1, w, 1). Skipped when h < 2 (the top edge covers it).
    cmp word [BOX_H], 2
    jb .no_bottom
    mov di, [BOX_X]
    mov si, [BOX_Y]
    add si, [BOX_H]
    dec si
    mov cx, [BOX_W]
    mov dx, 1
    mov bl, [BOX_BD]
    call lfb_fill_rect
.no_bottom:
    ; Left edge: (x, y+1, 1, h-2). Skipped when h < 3 (nothing between top/bottom).
    cmp word [BOX_H], 3
    jb .no_sides
    mov di, [BOX_X]
    mov si, [BOX_Y]
    inc si
    mov cx, 1
    mov dx, [BOX_H]
    sub dx, 2
    mov bl, [BOX_BD]
    call lfb_fill_rect
    ; Right edge: (x+w-1, y+1, 1, h-2). Skipped when w < 2 (the left edge covers it).
    cmp word [BOX_W], 2
    jb .no_sides
    mov di, [BOX_X]
    add di, [BOX_W]
    dec di
    mov si, [BOX_Y]
    inc si
    mov cx, 1
    mov dx, [BOX_H]
    sub dx, 2
    mov bl, [BOX_BD]
    call lfb_fill_rect
.no_sides:
    ; Interior: (x+1, y+1, w-2, h-2). Skipped when w < 3 or h < 3.
    cmp word [BOX_W], 3
    jb .done
    cmp word [BOX_H], 3
    jb .done
    mov di, [BOX_X]
    inc di
    mov si, [BOX_Y]
    inc si
    mov cx, [BOX_W]
    sub cx, 2
    mov dx, [BOX_H]
    sub dx, 2
    mov bl, [BOX_IN]
    call lfb_fill_rect
.done:
    pop ds
    popa
    ret

; lfb_blit_1bpp: draw a 1-bpp ROM bitmap; set bits painted in color BL, clear
; bits leave the background. CS:SI=bitmap top row, DI=x, AX=y, CX=width(px),
; DX=height(rows), BH=stride(bytes/row). Uses the BLIT_* segment-0 scratch.
lfb_blit_1bpp:
    pusha
    push ds
    push ax                     ; save y
    xor ax, ax
    mov ds, ax
    mov [BLIT_X], di
    mov [BLIT_W], cx
    mov [BLIT_H], dx
    mov [BLIT_COLOR], bl
    movzx bx, bh
    mov [BLIT_STRIDE], bx
    pop ax
    mov [BLIT_Y], ax
.row_loop:
    xor ax, ax
    mov ds, ax
    mov dx, [BLIT_H]
    test dx, dx
    jz .done
    mov di, [BLIT_X]
    mov ax, [BLIT_Y]
    call lfb_pixel_addr         ; EBX = LFB addr of (x, y)
    xor bp, bp                  ; column counter
.col_loop:
    cmp bp, [BLIT_W]
    jae .row_end
    mov cx, bp
    shr cx, 3                   ; byte index in the source row
    push si
    add si, cx
    mov al, [cs:si]
    pop si
    mov cx, bp
    and cl, 7
    mov ch, 7
    sub ch, cl                  ; bit position 7..0
    mov cl, ch
    shr al, cl
    test al, 1
    jz .skip
    mov al, [BLIT_COLOR]
    mov [fs:ebx], al
.skip:
    inc ebx
    inc bp
    jmp .col_loop
.row_end:
    mov ax, [BLIT_Y]
    inc ax
    mov [BLIT_Y], ax
    mov ax, [BLIT_STRIDE]
    add si, ax
    dec word [BLIT_H]
    jmp .row_loop
.done:
    pop ds
    popa
    ret

; lfb_load_palette: load the whole art palette into the VGA DAC via INT 10h
; AX=1012h (BX=start index, CX=count, ES:DX -> 6-bit RGB triples). The palette
; bytes live in the ROM (CS), so ES=CS. The Margo 8bpp scanout reads this DAC.
lfb_load_palette:
    push ax
    push bx
    push cx
    push dx
    push es
    push cs
    pop es                      ; ES:DX -> art_palette (ROM)
    mov dx, art_palette
    mov ax, 0x1012
    xor bx, bx                  ; start at DAC index 0
    mov cx, ART_PAL_COUNT
    int 0x10
    pop es
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; lfb_blit_rle: decode a flat [count,value] RLE stream into a w*h rectangle of
; the LFB. CS:SI = RLE source, DI = x, AX = y, CX = w, DX = h. Opaque (every
; pixel written, value 0 included); decodes exactly w*h pixels; runs may cross
; row boundaries. Geometry + run state live in segment-0 scratch (BLIT_*) so the
; inner loop keeps registers free, mirroring lfb_blit_1bpp.
;
; EBX holds the running LFB address that
; lfb_pixel_addr returns, so the pixel value is written from AL and BL/BH are
; never touched. lfb_pixel_addr preserves SI (the RLE pointer) and CX.
lfb_blit_rle:
    pusha
    push ds
    push ax                     ; save y
    xor ax, ax
    mov ds, ax                  ; DS = 0 for the scratch words
    mov [BLIT_X], di
    mov [BLIT_W], cx
    mov [BLIT_H], dx
    mov word [BLIT_RUN_LEFT], 0 ; no active run yet
    pop ax
    mov [BLIT_Y], ax
.row:
    xor ax, ax
    mov ds, ax
    mov dx, [BLIT_H]
    test dx, dx
    jz .done
    mov di, [BLIT_X]
    mov ax, [BLIT_Y]
    call lfb_pixel_addr         ; EBX = LFB addr of (x, y); SI, CX preserved
    mov cx, [BLIT_W]            ; CX = columns remaining this row
.col:
    cmp word [BLIT_RUN_LEFT], 0
    jne .have_run
    mov al, [cs:si]             ; next run: count
    mov ah, [cs:si + 1]         ; value
    add si, 2
    movzx dx, al
    mov [BLIT_RUN_LEFT], dx
    mov [BLIT_RUN_VAL], ah
.have_run:
    mov al, [BLIT_RUN_VAL]
    mov [fs:ebx], al            ; write the pixel (AL, never BL/BH)
    inc ebx
    dec word [BLIT_RUN_LEFT]
    dec cx
    jnz .col
    mov ax, [BLIT_Y]
    inc ax
    mov [BLIT_Y], ax
    dec word [BLIT_H]
    jmp .row
.done:
    pop ds
    popa
    ret
