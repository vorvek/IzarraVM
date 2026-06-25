; izbios-lfb.inc - STREAM F: linear-framebuffer draw primitives for the
; proprietary Margo mode 0x150 (320x240x8). All routines preserve caller
; registers. Pixels go to LFB_BASE + y*LFB_PITCH + x via the unreal-mode flat FS
; that unreal_init established (32-bit [fs:ebx], same pattern as ramtest_poke32).
; The 8x8 CP437 font (bios_font) and the red wordmark (bios_logo_bitmap) are
; shared with izbios-gfx.inc.

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
    push eax
    push ebx
    push ecx
    movzx eax, al
    mov ah, al                  ; replicate to all 4 bytes of EAX
    mov ecx, eax
    shl ecx, 16
    or eax, ecx
    mov ebx, LFB_BASE
    mov ecx, LFB_PIXELS / 4
.fill:
    mov [fs:ebx], eax
    add ebx, 4
    dec ecx
    jnz .fill
    pop ecx
    pop ebx
    pop eax
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
lfb_glyph:
    pusha
    push eax
    push ebx
    push ecx
    push edx
    movzx bp, al
    shl bp, 3
    add bp, bios_font           ; CS:bp -> 8 glyph rows
    mov dx, si                  ; dx = current y
    mov cx, 8                   ; 8 rows
.row:
    push cx
    push di                     ; x constant per row
    mov al, [cs:bp]             ; this row's bitmap
    inc bp
    push bx                     ; preserve BL=fg/BH=bg: lfb_pixel_addr returns EBX
    push ax
    mov si, dx                  ; y for lfb_pixel_addr (word)
    mov ax, dx
    call lfb_pixel_addr         ; EBX = LFB addr of (di, dx); clobbers BX
    pop ax
    pop bx                      ; restore fg/bg before the column loop reads them
    mov cx, 8                   ; 8 columns
.col:
    shl al, 1                   ; MSB -> CF
    jc .fg
    mov dl, bh                  ; background index
    jmp .put
.fg:
    mov dl, bl                  ; foreground index
.put:
    mov [fs:ebx], dl
    inc ebx
    loop .col
    pop di
    pop cx
    inc dx                      ; next scanline
    loop .row
    pop edx
    pop ecx
    pop ebx
    pop eax
    popa
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

; lfb_fill_rect: DI=x, SI(word)=y, CX=w, DX=h, BL=color. Solid rectangle.
lfb_fill_rect:
    pusha
.row:
    test dx, dx
    jz .done
    push cx
    push di
    push si
    push bx                     ; preserve BL=color: lfb_pixel_addr returns EBX
    mov ax, si
    call lfb_pixel_addr         ; EBX = addr of (di, si); clobbers BX
    pop bx                      ; restore color before the column loop writes it
    pop si
    pop di
    pop cx
    push cx
    push dx
.col:
    mov [fs:ebx], bl
    inc ebx
    loop .col
    pop dx
    pop cx
    inc si
    dec dx
    jmp .row
.done:
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
