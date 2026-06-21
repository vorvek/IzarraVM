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
    cmp bl, 0xe0
    je .prefix
    mov ax, BDA_SEG
    mov ds, ax
    mov bh, [KB_PREFIX]
    mov byte [KB_PREFIX], 0
    test bl, 0x80
    jnz .break
    cmp bl, 0x2a
    je .set_lshift
    cmp bl, 0x36
    je .set_rshift
    cmp bl, 0x1d
    je .set_ctrl
    cmp bl, 0x38
    je .set_altgr
    push cx
    push si
    mov ax, BDA_SEG
    mov ds, ax
    ; Compute layout*384 as layout*128 + layout*256, then select its modifier
    ; table. SI finally indexes that 128-byte table by make scancode.
    movzx si, byte [KB_LAYOUT]
    mov ax, si
    shl si, 7
    shl ax, 8
    add si, ax
    cmp byte [KB_ALTGR], 0
    jne .altgr
    test byte [KB_FLAGS], 0x03      ; either shift held?
    jz .no_shift
    add si, 128
    jmp .no_shift
.altgr:
    add si, 256
.no_shift:
    add si, kbd_layout_tables       ; SI = table base for this layout/shift
    movzx cx, bl                    ; CX = scancode
    add si, cx
    mov al, [cs:si]                 ; AL = ASCII (0 if none)
    test byte [KB_FLAGS], 0x08
    jz .not_left_alt
    cmp byte [KB_ALTGR], 0
    jne .not_left_alt
    xor al, al
.not_left_alt:
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
.set_altgr:
    test bh, bh
    jz .set_alt
    mov byte [KB_ALTGR], 1
    or byte [KB_FLAGS], 0x08
    jmp .eoi
.set_alt:
    or byte [KB_FLAGS], 0x08
    jmp .eoi
.prefix:
    mov ax, BDA_SEG
    mov ds, ax
    mov byte [KB_PREFIX], 1
    jmp .eoi
.break:
    and bl, 0x7f
    cmp bl, 0x2a
    je .clr_lshift
    cmp bl, 0x36
    je .clr_rshift
    cmp bl, 0x1d
    je .clr_ctrl
    cmp bl, 0x38
    je .clr_altgr
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
    jmp .eoi
.clr_altgr:
    test bh, bh
    jz .clr_alt
    mov byte [KB_ALTGR], 0
    and byte [KB_FLAGS], 0xf7
    jmp .eoi
.clr_alt:
    and byte [KB_FLAGS], 0xf7
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
    cmp ah, 0x10        ; enhanced read -> same as AH=00h
    je .read
    cmp ah, 0x01
    je .peek
    cmp ah, 0x11        ; enhanced peek -> same as AH=01h
    je .peek
    cmp ah, 0x02
    je .flags
    cmp ah, 0x12        ; enhanced shift status -> same as AH=02h
    je .flags
    cmp ah, 0x05        ; keyboard buffer write
    je .bufwrite
    cmp ah, 0x03        ; set typematic rate and delay
    je .typematic
    cmp ah, 0x09        ; get keyboard functionality
    je .funcs
    cmp ah, 0x0a        ; get keyboard id
    je .kbid
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
.bufwrite:
    push ds
    push bx
    push cx
    mov bx, BDA_SEG
    mov ds, bx
    ; kb_enqueue wants AX = AH:scancode AL:ascii. CH=scancode, CL=ascii already
    ; match that order, so AX = CX.
    mov ax, cx
    ; Detect a full ring: compare the would-be tail to head before enqueuing.
    mov bx, [KB_BUF_TAIL]
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .bw_nowrap
    mov bx, KB_BUF_START
.bw_nowrap:
    cmp bx, [KB_BUF_HEAD]
    je .bw_full
    call kb_enqueue
    pop cx
    pop bx
    pop ds
    mov al, 0x00
    iret
.bw_full:
    pop cx
    pop bx
    pop ds
    mov al, 0x01
    iret
.typematic:
    ; AH=03h set typematic rate and delay. AL selects the sub-function:
    ;   00h default rate/delay, 01h..04h on/off and click variants -> return
    ;   cleanly, 05h program rate from BL (low 5 bits) and delay from BH (bits
    ;   6-5). Only 05h touches the keyboard; the rest are accepted no-ops so a
    ;   caller that just wants the default does not fault.
    cmp al, 0x05
    jne .typ_done
    push ax
    push bx
    ; The 0xF3 command and its rate byte each draw an ACK (0xFA) through the
    ; keyboard output buffer, which arms IRQ1. Mask IRQ1 and run with interrupts
    ; off so INT 09h cannot read the ACK and mistake it for a scancode, then
    ; drain each ACK ourselves.
    pushf
    cli
    in al, 0x21
    push ax                 ; save the live master PIC mask
    or al, 0x02             ; mask IRQ1
    out 0x21, al
    mov al, 0xf3            ; keyboard command: set typematic rate/delay
    out 0x60, al
    call .typ_drain
    ; Build the rate byte: BL low 5 bits are the repeat rate, BH bits 6-5 the
    ; initial delay. Mask each to its field and OR them.
    mov al, bl
    and al, 0x1f
    mov ah, bh
    and ah, 0x60
    or al, ah
    out 0x60, al            ; rate/delay parameter
    call .typ_drain
    pop ax                  ; AL = saved PIC mask
    out 0x21, al
    popf
    pop bx
    pop ax
.typ_done:
    clc
    iret
; .typ_drain: read and discard a pending keyboard byte (the 0xFA ACK). Bounded.
; Skips an AUX byte so a mouse report is left for its owner. Clobbers AX, CX.
.typ_drain:
    mov cx, 16
.typ_spin:
    in al, 0x64
    mov ah, al
    test al, 0x01           ; output buffer full?
    jz .typ_drained
    test ah, 0x20           ; AUX byte? leave it
    jnz .typ_drained
    in al, 0x60             ; discard the keyboard ACK
    loop .typ_spin
.typ_drained:
    ret
.funcs:
    ; AH=09h get keyboard functionality. AL = a bitmap of the INT 16h functions
    ; present (RBIL table 00585): bit 0 AX=0300h, bit 1 AX=0304h, bit 2 AX=0305h,
    ; bit 4 AH=0Ah, bit 5 AH=10h-12h. Bit 3 (AX=0306h) and bit 6 (122-key) are
    ; not implemented, so they stay clear.
    mov al, 0x37
    xor ah, ah
    iret
.kbid:
    ; AH=0Ah get keyboard ID. BX = MF2 enhanced-keyboard ID (BH=0xAB, BL=0x41).
    mov bx, 0xab41
    clc
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
