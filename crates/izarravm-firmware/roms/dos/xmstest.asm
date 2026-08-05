; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; XMS round-trip fixture. Runs in V86 under TOKAEMM.
;
; Install-check (INT 2Fh 4300) -> get entry (4310) -> version -> alloc 64 KB ->
; lock -> move a pattern conventional->EMB -> move EMB->conventional -> verify ->
; unlock -> free, then repeat the move at the end of a near-full EMB and verify
; a failed growth leaves the old allocation intact. Success is 0xA5; 0xEn
; names the step that broke.
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

    ; 3b. query free extended memory (08h): DX = total KB, expect >= 64
    mov ah, 0x08
    call far [entry]
    cmp dx, 64
    jb f_query

    ; 3c. HMA claim (01h) succeeds, re-claim fails with BL=0x91, release (02h)
    mov ah, 0x01
    mov dx, 0xFFFF
    call far [entry]
    or ax, ax
    jz f_hma
    mov ah, 0x01
    mov dx, 0xFFFF
    call far [entry]
    or ax, ax
    jnz f_hma                    ; a second claim must fail
    cmp bl, 0x91
    jne f_hma
    mov ah, 0x02
    call far [entry]
    or ax, ax
    jz f_hma

    ; 3d. A20 local enable (05h) -> query (07h) == 1 -> disable (06h)
    mov ah, 0x05
    call far [entry]
    or ax, ax
    jz f_a20
    mov ah, 0x07
    call far [entry]
    cmp ax, 1
    jne f_a20
    mov ah, 0x06
    call far [entry]
    or ax, ax
    jz f_a20

    ; 4. allocate a 64 KB EMB (09h): DX = KB -> DX = handle
    mov ah, 0x09
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_alloc
    mov [handle], dx

    ; 4b. handle info (0Eh): DX = size KB, expect 64
    mov ah, 0x0E
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_info
    cmp dx, 64
    jne f_info

    ; 4c. resize (0Fh) to 128 KB, then info reports 128
    mov ah, 0x0F
    mov bx, 128
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_resize
    mov ah, 0x0E
    mov dx, [handle]
    call far [entry]
    cmp dx, 128
    jne f_resize

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

    ; 12. Allocate the largest free EMB and write the pattern to its last
    ; 256 bytes. This exercises the dedicated XMS pool at its boundary.
    mov ah, 0x08
    call far [entry]
    or ax, ax
    jz f_large
    mov [large_kb], ax
    mov dx, ax
    mov ah, 0x09
    call far [entry]
    or ax, ax
    jz f_large
    mov [handle], dx
    movzx eax, word [large_kb]
    shl eax, 10
    sub eax, 256
    mov [large_off], eax
    mov dword [d_len], 256
    mov word [d_srch], 0
    mov word [d_srcoff], srcbuf
    mov ax, cs
    mov word [d_srcoff+2], ax
    mov ax, [handle]
    mov [d_dsth], ax
    mov eax, [large_off]
    mov [d_dstoff], eax
    mov ah, 0x0B
    mov si, desc
    call far [entry]
    or ax, ax
    jz f_large

    ; A deliberately impossible growth must fail and restore the old handle.
    mov ah, 0x0F
    mov bx, 0xFFFF
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jnz f_large
    cmp bl, 0xA0
    jne f_large
    mov ah, 0x0E
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_large
    cmp dx, [large_kb]
    jne f_large

    ; Lock the restored handle, read the end pattern back, and release it.
    mov ah, 0x0C
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_large
    push es
    push cs
    pop es
    mov di, dstbuf
    mov cx, 256
    xor al, al
    rep stosb
    pop es
    mov dword [d_len], 256
    mov ax, [handle]
    mov [d_srch], ax
    mov eax, [large_off]
    mov [d_srcoff], eax
    mov word [d_dsth], 0
    mov word [d_dstoff], dstbuf
    mov ax, cs
    mov word [d_dstoff+2], ax
    mov ah, 0x0B
    mov si, desc
    call far [entry]
    or ax, ax
    jz f_large
    mov si, dstbuf
    mov cx, 256
.large_verify:
    lodsb
    cmp al, 0x5A
    jne f_large
    loop .large_verify
    mov ah, 0x0D
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_large
    mov ah, 0x0A
    mov dx, [handle]
    call far [entry]
    or ax, ax
    jz f_large

    ; --- 08h must distinguish LARGEST from TOTAL -------------------------
    ; XMS 08h returns AX = largest free block and DX = total free -- the figure
    ; DOS/16M-family loaders size their requests from. Fragment the arena with
    ; two blocks, free the lower one, and the largest must then be strictly
    ; less than the total.
    ;
    ; NOTE: this does NOT currently fail. 64 KB blocks are 4 KB-page-aligned,
    ; and any hole that shape can create is one the 4 KB bitmap can already
    ; represent exactly -- AX (arena_longest_clear, a bit-scan max) and DX
    ; (xms_free, a counter kept in lockstep with every arena_mark_xms/
    ; arena_clear_xms call) are sum-vs-max over the SAME bitmap, so AX < DX
    ; holds whenever there is more than one free run, at ANY page granularity.
    ; Verified empirically: this block runs clean through to the 1 KB check
    ; below on today's driver. It is not a defect-discriminating red test --
    ; it is a forward regression guard on the largest/total relationship that
    ; Task 4's arena_query must keep honoring. The actual pre-Task-4 defect is
    ; the 1 KB request below.
    mov ah, 0x09                  ; low block, 64 KB
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_gran
    mov [frag_lo], dx
    mov ah, 0x09                  ; a second block pins a hole below it
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_gran
    mov [frag_hi], dx
    mov ah, 0x0A                  ; release the low block -> interior hole
    mov dx, [frag_lo]
    call far [entry]
    or ax, ax
    jz f_gran
    mov ah, 0x08
    call far [entry]
    cmp ax, dx
    jae f_largest                 ; largest == total: the hole is invisible
    mov ah, 0x0A
    mov dx, [frag_hi]
    call far [entry]
    or ax, ax
    jz f_gran

    ; --- a 1 KB request must cost 1 KB ------------------------------------
    ; 386MAX allocates XMS on a 1 KB boundary (ALLOC_LIM @ALLOC_XMS). A 4 KB
    ; page arena rounds a 1 KB block up and burns 4 KB of the reported total.
    mov ah, 0x08
    call far [entry]
    mov [gran_before], dx
    mov ah, 0x09
    mov dx, 1
    call far [entry]
    or ax, ax
    jz f_gran
    mov [gran_handle], dx
    mov ah, 0x08
    call far [entry]
    mov ax, [gran_before]
    sub ax, dx                    ; kilobytes the 1 KB block actually consumed
    cmp ax, 1
    jne f_gran
    mov ah, 0x0A
    mov dx, [gran_handle]
    call far [entry]
    or ax, ax
    jz f_gran

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
f_query:    mov al, 0xE9
            jmp sig
f_hma:      mov al, 0xEA
            jmp sig
f_a20:      mov al, 0xEB
            jmp sig
f_info:     mov al, 0xEC
            jmp sig
f_resize:   mov al, 0xED
            jmp sig
f_large:    mov al, 0xEE
            jmp sig
; 0xE0-0xEE are already claimed (f_noxms above through f_large just above), so
; the two new codes go just past them rather than at the plan's originally-
; picked 0xE0/0xEF (0xE0 collides with f_noxms; see the commit message).
f_largest:  mov al, 0xEF
            jmp sig
f_gran:     mov al, 0xF0
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
large_kb: dw 0
large_off: dd 0
frag_lo:     dw 0
frag_hi:     dw 0
gran_before: dw 0
gran_handle: dw 0
desc:
d_len:    dd 0
d_srch:   dw 0
d_srcoff: dd 0
d_dsth:   dw 0
d_dstoff: dd 0
srcbuf:  times 256 db 0
dstbuf:  times 256 db 0
