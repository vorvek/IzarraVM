; xmstest.com — SP-4b M1 XMS round-trip e2e fixture. Runs in V86 under TOKAEMM.
;
; Install-check (INT 2Fh 4300) -> get entry (4310) -> version -> alloc 64 KB ->
; lock -> move a pattern conventional->EMB -> move EMB->conventional -> verify ->
; unlock -> free, then signal 0xA5 (success) via the unit-tester exit port. Any
; other code names the step that broke (0xEn), so a NO-GO localizes the failure.
;
; Build: nasm -f bin xmstest.asm -o xmstest.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; 1. XMS install-check
    mov ax, 0x4300
    int 0x2F
    cmp al, 0x80
    jne f_noxms

    ; 2. get the driver entry point -> [entry]
    mov ax, 0x4310
    int 0x2F
    mov [entry], bx
    mov [entry+2], es

    ; 3. get version (00h): AX = 0x0300
    xor ah, ah
    call far [entry]
    cmp ax, 0x0300
    jne f_ver

    ; 4. allocate a 64 KB EMB (09h): DX = KB -> DX = handle
    mov ah, 0x09
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_alloc
    mov [handle], dx

    ; 5. lock the block (0Ch) — exercises lock and arms the free-locked guard
    mov ah, 0x0C
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_lock

    ; 6. fill srcbuf with the pattern 0x5A
    push es
    push cs
    pop es
    mov di, srcbuf
    mov cx, 256
    mov al, 0x5A
    cld
    rep stosb
    pop es

    ; 7. move srcbuf (conventional) -> EMB offset 0 (0Bh)
    ;    descriptor: len=256, srcH=0 srcOff=CS:srcbuf, dstH=handle dstOff=0
    mov dword [d_len], 256
    mov word [d_srch], 0
    mov word [d_srcoff], srcbuf
    mov ax, cs
    mov word [d_srcoff+2], ax
    mov ax, [handle]
    mov word [d_dsth], ax
    mov dword [d_dstoff], 0
    mov ah, 0x0B
    mov si, desc
    call far [entry]
    or ax, ax
    jz f_move_out

    ; 8. move EMB offset 0 -> dstbuf (conventional) (0Bh)
    mov dword [d_len], 256
    mov ax, [handle]
    mov word [d_srch], ax
    mov dword [d_srcoff], 0
    mov word [d_dsth], 0
    mov word [d_dstoff], dstbuf
    mov ax, cs
    mov word [d_dstoff+2], ax
    mov ah, 0x0B
    mov si, desc
    call far [entry]
    or ax, ax
    jz f_move_in

    ; 9. verify dstbuf == 0x5A * 256 (the pattern survived the round trip)
    mov si, dstbuf
    mov cx, 256
.vloop:
    lodsb
    cmp al, 0x5A
    jne f_verify
    loop .vloop

    ; 10. unlock (0Dh)
    mov ah, 0x0D
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_unlock

    ; 11. free (0Ah)
    mov ah, 0x0A
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_free

    mov al, OK
    jmp sig

f_noxms:    mov al, 0xE0
            jmp sig
f_ver:      mov al, 0xE1
            jmp sig
f_alloc:    mov al, 0xE2
            jmp sig
f_lock:     mov al, 0xE3
            jmp sig
f_move_out: mov al, 0xE4
            jmp sig
f_move_in:  mov al, 0xE5
            jmp sig
f_verify:   mov al, 0xE6
            jmp sig
f_unlock:   mov al, 0xE7
            jmp sig
f_free:     mov al, 0xE8

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

entry:   dd 0
handle:  dw 0
desc:
d_len:    dd 0
d_srch:   dw 0
d_srcoff: dd 0
d_dsth:   dw 0
d_dstoff: dd 0
srcbuf:  times 256 db 0
dstbuf:  times 256 db 0
