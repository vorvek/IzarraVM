; umbmech.com — SP-4b M3 UMB *mechanism* e2e. Drives TOKAEMM's XMS 10h/11h/12h
; directly (no DOS=UMB, so the whole upper region is free for us), exercising the
; allocator paths the DOS=UMB e2e doesn't reach: the too-big probe (B0h+largest),
; alloc, grow (12h), release (11h), and reuse-after-free. Write/read a pattern to
; prove the paged RAM. Signals 0xA5, or a 0xEn code naming the failed step.
;
; Build: nasm -f bin umbmech.asm -o umbmech.com
cpu 386
org 0x100
%define OK 0xA5

start:
    mov ax, 0x4300               ; XMS install-check
    int 0x2F
    cmp al, 0x80
    jne f_noxms
    mov ax, 0x4310              ; get the driver entry point
    int 0x2F
    mov [entry], bx
    mov [entry+2], es

    ; 10h(DX=0xFFFF): too big -> AX=0, BL=0xB0, DX=largest (the whole 0x2800 window)
    mov ah, 0x10
    mov dx, 0xFFFF
    call far [entry]
    or ax, ax
    jnz f_probe
    cmp bl, 0xB0
    jne f_probe
    cmp dx, 0x2800
    jne f_probe

    ; 10h(DX=0x100): allocate 256 paras -> AX=1, BX=0xC800, DX=0x100
    mov ah, 0x10
    mov dx, 0x100
    call far [entry]
    or ax, ax
    jz f_alloc
    cmp bx, 0x0C800
    jne f_alloc
    mov [seg1], bx

    ; write/read a pattern to the returned segment (proves the paged RAM)
    mov es, bx
    xor di, di
    mov cx, 256
    mov al, 0x5A
    cld
    rep stosb
    xor si, si
    mov cx, 256
.v:
    mov al, [es:si]
    cmp al, 0x5A
    jne f_ram
    inc si
    loop .v

    ; 12h: grow the block to 0x200 paras (space above is free) -> AX=1
    mov ah, 0x12
    mov bx, 0x200
    mov dx, [seg1]
    call far [entry]
    or ax, ax
    jz f_grow

    ; 11h: release it -> AX=1
    mov ah, 0x11
    mov dx, [seg1]
    call far [entry]
    or ax, ax
    jz f_rel

    ; 10h(0x100) again: reuse the freed low run -> AX=1, BX=0xC800
    mov ah, 0x10
    mov dx, 0x100
    call far [entry]
    or ax, ax
    jz f_reuse
    cmp bx, 0x0C800
    jne f_reuse
    mov ah, 0x11               ; release it
    mov dx, bx
    call far [entry]
    or ax, ax
    jz f_rel

    ; SP-4b M2 regression (the umb_free_run 16-bit wrap): fill the whole window,
    ; then probe with a large-but-not-over-window need. The scan cursor advances
    ; to the fill block's top (0xF000); without the carry guard, cursor+need
    ; wrapped past 0xFFFF, slipped the window-end check, and a bogus run at/above
    ; the window end (ROM space) came back. Correct: no-fit, AX=0, BL=0xB1, DX=0.
    mov ah, 0x10
    mov dx, 0x2800               ; the whole frameless window
    call far [entry]
    or ax, ax
    jz f_wrap
    mov [seg1], bx
    mov ah, 0x10
    mov dx, 0x1000               ; passes the entry guard; must not wrap-succeed
    call far [entry]
    or ax, ax
    jnz f_wrap                   ; a success here is the wrap bug
    cmp bl, 0xB1
    jne f_wrap
    or dx, dx
    jnz f_wrap
    mov ah, 0x11                 ; release the fill block
    mov dx, [seg1]
    call far [entry]
    or ax, ax
    jz f_rel

    mov al, OK
    jmp sig

f_noxms: mov al, 0xE0
         jmp sig
f_probe: mov al, 0xE1
         jmp sig
f_alloc: mov al, 0xE2
         jmp sig
f_ram:   mov al, 0xE3
         jmp sig
f_grow:  mov al, 0xE4
         jmp sig
f_rel:   mov al, 0xE5
         jmp sig
f_reuse: mov al, 0xE6
         jmp sig
f_wrap:  mov al, 0xE7

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

entry: dd 0
seg1:  dw 0
