%define KB_BUF_START   0x001e      ; BDA: keyboard buffer
%define KB_BUF_HEAD    0x001a      ; BDA: head pointer (offset into 0x40 seg)
%define KB_BUF_TAIL    0x001c      ; BDA: tail pointer
%define KB_FLAGS       0x0017      ; BDA: shift flags
; Second shift-flags byte (standard AT BIOS slot). Bits 4-7 are the transient
; "lock key held" guard so a held CapsLock/NumLock/ScrollLock/Insert key toggles
; the matching KB_FLAGS bit once per press, not once per auto-repeat.
%define KB_FLAGS_1     0x0018      ; BDA: second shift-flags byte
%define KB_LAYOUT      0x0096      ; BDA: active keyboard layout index
%define KB_PREFIX      0x0097      ; pending E0 extended-key prefix
%define KB_ALTGR       0x0098      ; right Alt held

; INT 09h: keyboard hardware ISR
int09:
    push ax
    push bx
    push ds
    in al, 0x60            ; read scancode
    mov bl, al
    cmp bl, 0xe0
    je .prefix
    mov ax, 0x0040
    mov ds, ax
    mov bh, [KB_PREFIX]
    mov byte [KB_PREFIX], 0
    test bl, 0x80          ; break code? update shift state, do not enqueue
    jnz .break
    ; Make of a shift/Ctrl key sets the held bit and never enqueues ASCII.
    cmp bl, 0x2a
    je .set_lshift
    cmp bl, 0x36
    je .set_rshift
    cmp bl, 0x1d
    je .set_ctrl
    cmp bl, 0x38
    je .set_altgr
    ; Lock/toggle keys flip a KB_FLAGS bit on make and enqueue no ASCII. The
    ; matching KB_FLAGS_1 transient bit guards against auto-repeat so a held key
    ; toggles once per press. INT 16h AH=02h then reports the live lock state.
    cmp bl, 0x3a
    je .make_caps
    cmp bl, 0x45
    je .make_num
    cmp bl, 0x46
    je .make_scroll
    cmp bl, 0x52
    jne .map_ascii
    test bh, bh
    jnz .make_ins
    test byte [KB_FLAGS], 0x20
    jz .make_ins
.map_ascii:
    ; Map scancode -> ASCII using the active layout and shift state, then enqueue
    ; (scancode:ascii).
    push cx
    push si
    mov ax, 0x0040
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
    test byte [KB_FLAGS], 0x03     ; either shift held?
    jz .no_shift
    add si, 128
    jmp .no_shift
.altgr:
    add si, 256
.no_shift:
    add si, kbd_layout_tables
    movzx cx, bl
    add si, cx
    mov al, [cs:si]                ; ASCII (0 if none)
    test bh, bh
    jnz .keypad_done
    cmp bl, 0x4a
    jne .keypad_plus
    mov al, '-'
    jmp .keypad_done
.keypad_plus:
    cmp bl, 0x4e
    jne .keypad_numlock
    mov al, '+'
    jmp .keypad_done
.keypad_numlock:
    test byte [KB_FLAGS], 0x20
    jz .keypad_done
    cmp bl, 0x47
    jb .keypad_done
    cmp bl, 0x53
    ja .keypad_done
    movzx si, bl
    sub si, 0x47
    mov al, [cs:keypad_num_ascii + si]
.keypad_done:
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
    and ah, 0xdf                   ; fold case for the A..Z range test
    cmp ah, 'A'
    jb .enqueue
    cmp ah, 'Z'
    ja .enqueue
    and al, 0x1f
.enqueue:
    ; Mark a 101/102-key extended key that carries no normal ASCII (BH is the E0
    ; prefix flag, AL=0 means the layout gave nothing): store AL=0xE0 as the MF-II
    ; gray-key marker. INT 16h AH=00h/01h collapse this marker back to AL=0 so
    ; legacy callers see "extended, no ASCII"; AH=10h/11h leave it so an enhanced
    ; caller can tell the gray cursor pad from the numeric pad. Gray keys that do
    ; carry ASCII (gray Enter, gray '/') keep their real AL and read alike on both.
    test bh, bh
    jz .enqueue_store
    test al, al
    jnz .enqueue_store
    mov al, 0xe0
.enqueue_store:
    mov ah, bl                     ; scancode in AH, ASCII in AL
    call kb_enqueue
    pop si
    pop cx
    jmp .eoi
.set_lshift:
    mov ax, 0x0040
    mov ds, ax
    or byte [KB_FLAGS], 0x02
    jmp .eoi
.set_rshift:
    mov ax, 0x0040
    mov ds, ax
    or byte [KB_FLAGS], 0x01
    jmp .eoi
.set_ctrl:
    mov ax, 0x0040
    mov ds, ax
    or byte [KB_FLAGS], 0x04
    ; Track left vs right Ctrl in KB_FLAGS_1 (BH is the E0 prefix flag): bit 2 is
    ; right Ctrl, bit 0 is left Ctrl. INT 16h AH=12h returns this byte as AH.
    test bh, bh
    jz .set_lctrl
    or byte [KB_FLAGS_1], 0x04
    jmp .eoi
.set_lctrl:
    or byte [KB_FLAGS_1], 0x01
    jmp .eoi
.set_altgr:
    mov ax, 0x0040
    mov ds, ax
    test bh, bh
    jz .set_alt
    mov byte [KB_ALTGR], 1
    or byte [KB_FLAGS], 0x08
    or byte [KB_FLAGS_1], 0x08      ; right Alt -> AH=12h bit 3
    jmp .eoi
.set_alt:
    or byte [KB_FLAGS], 0x08
    or byte [KB_FLAGS_1], 0x02      ; left Alt -> AH=12h bit 1
    jmp .eoi
.make_caps:
    mov ax, 0x0040
    mov ds, ax
    test byte [KB_FLAGS_1], 0x40    ; already held? skip the auto-repeat
    jnz .eoi
    or byte [KB_FLAGS_1], 0x40
    xor byte [KB_FLAGS], 0x40       ; toggle CapsLock
    jmp .eoi
.make_num:
    mov ax, 0x0040
    mov ds, ax
    test byte [KB_FLAGS_1], 0x20
    jnz .eoi
    or byte [KB_FLAGS_1], 0x20
    xor byte [KB_FLAGS], 0x20       ; toggle NumLock
    jmp .eoi
.make_scroll:
    mov ax, 0x0040
    mov ds, ax
    test byte [KB_FLAGS_1], 0x10
    jnz .eoi
    or byte [KB_FLAGS_1], 0x10
    xor byte [KB_FLAGS], 0x10       ; toggle ScrollLock
    jmp .eoi
.make_ins:
    mov ax, 0x0040
    mov ds, ax
    test byte [KB_FLAGS_1], 0x80
    jnz .eoi
    or byte [KB_FLAGS_1], 0x80
    xor byte [KB_FLAGS], 0x80       ; toggle Insert
    jmp .eoi
.prefix:
    mov ax, 0x0040
    mov ds, ax
    mov byte [KB_PREFIX], 1
    jmp .eoi
.break:
    and bl, 0x7f
    ; Left shift (0x2a) / right shift (0x36) / Ctrl (0x1d): clear the held bit.
    cmp bl, 0x2a
    je .clr_lshift
    cmp bl, 0x36
    je .clr_rshift
    cmp bl, 0x1d
    je .clr_ctrl
    cmp bl, 0x38
    je .clr_altgr
    ; Release of a lock key clears its transient guard so the next press toggles.
    cmp bl, 0x3a
    je .clr_caps_held
    cmp bl, 0x45
    je .clr_num_held
    cmp bl, 0x46
    je .clr_scroll_held
    cmp bl, 0x52
    je .clr_ins_held
    jmp .eoi
.clr_lshift:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS], 0xfd
    jmp .eoi
.clr_ctrl:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS], 0xfb
    ; Clear the matching left/right Ctrl bit in KB_FLAGS_1 (BH is the E0 prefix).
    test bh, bh
    jz .clr_lctrl
    and byte [KB_FLAGS_1], 0xfb     ; clear right Ctrl (bit 2)
    jmp .eoi
.clr_lctrl:
    and byte [KB_FLAGS_1], 0xfe     ; clear left Ctrl (bit 0)
    jmp .eoi
.clr_rshift:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS], 0xfe
    jmp .eoi
.clr_altgr:
    mov ax, 0x0040
    mov ds, ax
    test bh, bh
    jz .clr_alt
    mov byte [KB_ALTGR], 0
    and byte [KB_FLAGS], 0xf7
    and byte [KB_FLAGS_1], 0xf7     ; clear right Alt (bit 3)
    jmp .eoi
.clr_alt:
    and byte [KB_FLAGS], 0xf7
    and byte [KB_FLAGS_1], 0xfd     ; clear left Alt (bit 1)
    jmp .eoi
.clr_caps_held:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS_1], 0xbf
    jmp .eoi
.clr_num_held:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS_1], 0xdf
    jmp .eoi
.clr_scroll_held:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS_1], 0xef
    jmp .eoi
.clr_ins_held:
    mov ax, 0x0040
    mov ds, ax
    and byte [KB_FLAGS_1], 0x7f
.eoi:
    mov al, 0x20
    out 0x20, al           ; EOI to master PIC
    pop ds
    pop bx
    pop ax
    iret

; INT 16h: keyboard services
int16:
    cmp ah, 0x00
    je .read
    cmp ah, 0x10        ; enhanced read keystroke (101/102-key)
    je .read16
    cmp ah, 0x01
    je .peek
    cmp ah, 0x11        ; enhanced peek keystroke
    je .peek16
    cmp ah, 0x02
    je .flags
    cmp ah, 0x12        ; extended shift status
    je .flags12
    cmp ah, 0x04        ; PCjr keyclick state
    je .keyclick
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
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    pop ds
    je .read_wait          ; buffer empty: spin (IRQ fills it)
    push ds
    mov cx, 0x0040
    mov ds, cx
    mov bx, [KB_BUF_HEAD]
    mov ax, [bx]           ; AX = scancode:ascii
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .read_store
    mov bx, KB_BUF_START
.read_store:
    mov [KB_BUF_HEAD], bx
    pop ds
    ; Legacy read collapses the MF-II gray-key marker: a key the ISR tagged with
    ; AL=0xE0 (extended key, no normal ASCII) reads back as AL=0 here, so a 1981
    ; program sees the scancode in AH and an empty ASCII byte. Enhanced AH=10h
    ; below keeps the 0xE0 so a 101/102-key-aware program can tell them apart.
    cmp al, 0xe0
    jne .read_done
    xor al, al
.read_done:
    iret
.read16:
    ; Enhanced read (AH=10h): same ring as AH=00h but never collapses the gray-key
    ; marker, so extended cursor-pad keys keep their full scancode in AH and the
    ; 0xE0 marker in AL.
    sti
.read16_wait:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    pop ds
    je .read16_wait
    push ds
    mov cx, 0x0040
    mov ds, cx
    mov bx, [KB_BUF_HEAD]
    mov ax, [bx]
    add bx, 2
    cmp bx, KB_BUF_START + 32
    jb .read16_store
    mov bx, KB_BUF_START
.read16_store:
    mov [KB_BUF_HEAD], bx
    pop ds
    iret
.peek:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .peek_empty
    mov ax, [bx]
    pop ds
    ; Legacy peek collapses the gray-key marker the same way the legacy read does.
    cmp al, 0xe0
    jne .peek_done
    xor al, al
.peek_done:
    ; AH=01h reports availability in the caller's restored FLAGS image.
    push bp
    mov bp, sp
    and word [bp + 6], 0xffbe      ; CF=0, ZF=0: key present
    pop bp
    iret
.peek_empty:
    pop ds
    push bp
    mov bp, sp
    or word [bp + 6], 0x0041       ; CF=1, ZF=1: no key
    pop bp
    iret
.peek16:
    ; Enhanced peek (AH=11h): leaves the key in the ring and keeps the 0xE0 marker.
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .peek16_empty
    mov ax, [bx]
    pop ds
    push bp
    mov bp, sp
    and word [bp + 6], 0xffbe      ; CF=0, ZF=0: key present
    pop bp
    iret
.peek16_empty:
    pop ds
    push bp
    mov bp, sp
    or word [bp + 6], 0x0041       ; CF=1, ZF=1: no key
    pop bp
    iret
.flags:
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov al, [KB_FLAGS]
    pop ds
    xor ah, ah
    iret
.flags12:
    ; AH=12h extended shift status. AL = the standard shift flags (KB_FLAGS, the
    ; same byte AH=02h returns). AH = the second shift-flags byte (KB_FLAGS_1):
    ; bit 0 left Ctrl, bit 1 left Alt, bit 2 right Ctrl, bit 3 right Alt, plus the
    ; lock-pressed bits the ISR keeps in bits 4-7. Returns AX.
    push ds
    mov bx, 0x0040
    mov ds, bx
    mov al, [KB_FLAGS]
    mov ah, [KB_FLAGS_1]
    pop ds
    iret
.keyclick:
    ; PCjr-only keyclick enable/disable. This AT-style keyboard has no clicker.
    iret
.bufwrite:
    push ds
    push bx
    push cx
    mov bx, 0x0040
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
    ; Build the rate byte: BL low 5 bits are the repeat rate, BH is a 0-3 delay
    ; value (00h=250ms..03h=1000ms) placed into bits 5-6. Mask and OR them.
    mov al, bl
    and al, 0x1f
    mov ah, bh
    and ah, 0x03
    shl ah, 5
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

; BDA ring enqueue: AX = scancode:ascii
; Enters with DS = 0x0040. Drops the key if the buffer is full.
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
    je .full               ; tail+2 == head: full, drop
    mov [bx], ax
    mov [KB_BUF_TAIL], cx
.full:
    pop cx
    pop bx
    ret

keypad_num_ascii:
    db '7','8','9',0,'4','5','6',0,'1','2','3','0','.'

%include "kbd-layouts.inc"
