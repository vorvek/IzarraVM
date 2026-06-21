; izbios-kbd.inc - foundation: INT 09h / INT 16h keyboard path, lifted from
; kbd-bios-core.inc, plus kb_getkey / kb_flush for the setup page. The BDA ring
; offsets (KB_BUF_*) live in izbios-defs.inc. Installed at IVT[09h]/IVT[16h] by
; core bring-up.

; kb_getkey: keyboard access for the setup page.
;   AH=0 -> blocking read via INT 16h: AL=ASCII, AH=scancode.
;   AH=1 -> peek: CF=1 if empty; else AL=ASCII, AH=scancode (left in the ring).
; The peek reads the BDA ring directly rather than calling INT 16h AH=1: an ISR
; cannot return a flag through IRET (which pops the caller's saved FLAGS), so the
; peek's carry result would be lost. A near return preserves CF correctly.
kb_getkey:
    cmp ah, 0x01
    je .peek
    int 0x16
    ret
.peek:
    push ds
    push bx
    mov bx, BDA_SEG
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .empty
    mov ax, [bx]                ; AH=scancode, AL=ASCII (not consumed)
    pop bx
    pop ds
    clc
    ret
.empty:
    pop bx
    pop ds
    stc
    ret

; kb_flush: drop everything in the ring (head = tail).
kb_flush:
    push ds
    push ax
    push bx
    mov ax, BDA_SEG
    mov ds, ax
    mov bx, [KB_BUF_TAIL]
    mov [KB_BUF_HEAD], bx
    pop bx
    pop ax
    pop ds
    ret

; INT 09h: keyboard hardware ISR.
int09:
    push ax
    push bx
    push ds
    in al, 0x60
    mov bl, al
    test bl, 0x80
    jnz .break
    cmp bl, 0x2a
    je .set_lshift
    cmp bl, 0x36
    je .set_rshift
    cmp bl, 0x1d
    je .set_ctrl
    push cx
    push si
    mov ax, BDA_SEG
    mov ds, ax
    ; Compute the active layout's table base: kbd_layout_tables + layout*256,
    ; plus 128 when a shift is held. SI then indexes the half by scancode.
    mov al, [KB_LAYOUT]             ; cached layout index 0..5 (DS = BDA)
    mov ah, KBD_LAYOUT_STRIDE >> 1  ; 128, halved stride; the shift add doubles it
    mul ah                          ; AX = layout * 128
    shl ax, 1                       ; AX = layout * 256 (the block offset)
    test byte [KB_FLAGS], 0x03      ; either shift held?
    jz .no_shift
    add ax, KBD_LAYOUT_STRIDE >> 1  ; +128 selects the shifted half
.no_shift:
    mov si, ax
    add si, kbd_layout_tables       ; SI = table base for this layout/shift
    movzx cx, bl                    ; CX = scancode
    add si, cx
    mov al, [cs:si]                 ; AL = ASCII (0 if none)
    ; Ctrl + letter -> control code (Ctrl-A = 1 .. Ctrl-Z = 26), as a real BIOS
    ; does, so a full-screen program reads Ctrl-S as ASCII 0x13 from INT 16h.
    test byte [KB_FLAGS], 0x04
    jz .enqueue
    mov ah, al
    and ah, 0xdf                    ; fold case for the A..Z range test
    cmp ah, 'A'
    jb .enqueue
    cmp ah, 'Z'
    ja .enqueue
    and al, 0x1f
.enqueue:
    mov ah, bl                      ; AH = scancode
    call kb_enqueue
    pop si
    pop cx
    jmp .eoi
.set_lshift:
    mov ax, BDA_SEG
    mov ds, ax
    or byte [KB_FLAGS], 0x02
    jmp .eoi
.set_rshift:
    mov ax, BDA_SEG
    mov ds, ax
    or byte [KB_FLAGS], 0x01
    jmp .eoi
.set_ctrl:
    mov ax, BDA_SEG
    mov ds, ax
    or byte [KB_FLAGS], 0x04
    jmp .eoi
.break:
    and bl, 0x7f
    cmp bl, 0x2a
    je .clr_lshift
    cmp bl, 0x36
    je .clr_rshift
    cmp bl, 0x1d
    je .clr_ctrl
    jmp .eoi
.clr_lshift:
    mov ax, BDA_SEG
    mov ds, ax
    and byte [KB_FLAGS], 0xfd
    jmp .eoi
.clr_ctrl:
    mov ax, BDA_SEG
    mov ds, ax
    and byte [KB_FLAGS], 0xfb
    jmp .eoi
.clr_rshift:
    mov ax, BDA_SEG
    mov ds, ax
    and byte [KB_FLAGS], 0xfe
.eoi:
    mov al, 0x20
    out 0x20, al
    pop ds
    pop bx
    pop ax
    iret

; INT 16h: keyboard services.
int16:
    cmp ah, 0x00
    je .read
    cmp ah, 0x01
    je .peek
    cmp ah, 0x02
    je .flags
    iret
.read:
    sti
.read_wait:
    push ds
    mov bx, BDA_SEG
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    pop ds
    je .read_wait
    push ds
    mov cx, BDA_SEG
    mov ds, cx
    mov bx, [KB_BUF_HEAD]
    mov ax, [bx]
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .read_store
    mov bx, KB_BUF_START
.read_store:
    mov [KB_BUF_HEAD], bx
    pop ds
    iret
.peek:
    push ds
    mov bx, BDA_SEG
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .peek_empty
    mov ax, [bx]
    pop ds
    clc
    iret
.peek_empty:
    pop ds
    stc
    iret
.flags:
    push ds
    mov bx, BDA_SEG
    mov ds, bx
    mov al, [KB_FLAGS]
    pop ds
    xor ah, ah
    iret

; BDA ring enqueue: AX = scancode:ascii. Enters with DS = BDA_SEG. Drops on full.
kb_enqueue:
    push bx
    push cx
    mov bx, [KB_BUF_TAIL]
    mov cx, bx
    add cx, 2
    cmp cx, KB_BUF_START + 32
    jb .no_wrap
    mov cx, KB_BUF_START
.no_wrap:
    cmp cx, [KB_BUF_HEAD]
    je .full
    mov [bx], ax
    mov [KB_BUF_TAIL], cx
.full:
    pop cx
    pop bx
    ret

; The scancode -> ASCII tables for every layout live in kbd-layouts.inc, indexed
; above by the cached KB_LAYOUT byte.
