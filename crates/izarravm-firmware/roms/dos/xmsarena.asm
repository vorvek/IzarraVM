; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; XMS arena-shape fixture. Runs in V86 under TOKAEMM.
;
; Two assertions about the SHAPE of the shared arena, split out of xmstest.com
; so a regression anywhere in the XMS round trip cannot hide them. In one
; program they sat behind fifteen prerequisite steps, and the arena is exactly
; what the shared-pool work keeps touching, so any of those fifteen failing
; reported an unrelated code and both assertions went unrun.
;
; Install-check -> get entry -> premise floor -> 08h must report largest
; separately from total -> a 1 KB request must cost 1 KB. Success is 0xA5.
; 0xEF and 0xF0 are the ASSERTIONS; 0xD0, 0xD1, 0xF1 and 0xF2 are setup, kept
; distinct so a premise failure can never be read as a failed assertion.
;
; Codes are NOT renumbered from xmstest.com. 0xEF and 0xF0 name specific
; failures in the campaign log and in the driver's own comments; compacting
; them would silently invalidate that. Exit codes are a per-program namespace,
; so 0xD0/0xD1 are new here only for legibility against those records.
;
; Build: nasm -f bin xmsarena.asm -o xmsarena.com
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

    ; 3. Premise floor. Both assertions below allocate two 64 KB blocks, so a
    ; near-empty arena makes them degenerate rather than false: at exactly
    ; 128 KB free, largest and total both come to 64 KB after the low free and
    ; the largest-vs-total check would report 0xEF, an ASSERTION code, for what
    ; is really "not enough memory to run the test". 256 KB is comfortably clear
    ; of that boundary. xmstest.com's own floor is 64 KB, which is not enough
    ; here, so this is a separate check with a setup code of its own.
    mov ah, 0x08
    call far [entry]
    or ax, ax
    jz f_floor
    cmp dx, 256
    jb f_floor

    ; --- 08h must distinguish LARGEST from TOTAL -------------------------
    ; XMS 08h returns AX = largest free block and DX = total free, the figure
    ; DOS/16M-family loaders size their requests from. Allocate two 64 KB
    ; blocks, free the lower one, and the largest must then be strictly less
    ; than the total. Either allocation order works: whichever block the
    ; allocator places second, freeing the other leaves an interior hole, so
    ; this does not depend on the placement policy.
    ;
    ; This is a FORWARD GUARD, not a red-before-the-rewrite test. Both AX and DX
    ; come from one arena_query walk of the same bitmap, AX as the longest clear
    ; run and DX as the count of clear granules, so AX < DX holds whenever more
    ; than one free run exists, at ANY granularity. Granularity cannot break
    ; that relationship, which is why the 1 KB check below, not this one, was
    ; the assertion that was red before the arena rewrite.
    mov ah, 0x09                  ; low block, 64 KB
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_frag
    mov [frag_lo], dx
    mov ah, 0x09                  ; a second block pins a hole against it
    mov dx, 64
    call far [entry]
    or ax, ax
    jz f_frag
    mov [frag_hi], dx
    mov ah, 0x0A                  ; release the low block -> interior hole
    mov dx, [frag_lo]
    call far [entry]
    or ax, ax
    jz f_frag
    mov ah, 0x08
    call far [entry]
    or ax, ax
    jz f_frag                     ; nothing free at all: not this assertion
    cmp ax, dx
    jae f_largest                 ; largest == total: the hole is invisible
    mov ah, 0x0A
    mov dx, [frag_hi]
    call far [entry]
    or ax, ax
    jz f_frag

    ; --- a 1 KB request must cost 1 KB ------------------------------------
    ; 386MAX allocates XMS on a 1 KB boundary (ALLOC_LIM @ALLOC_XMS). A 4 KB
    ; page arena rounds a 1 KB block up and burns 4 KB of the reported total.
    ; Nothing but the allocation runs between the two 08h calls, and xf_alloc's
    ; only charge against the total is the granules it takes, so the difference
    ; is the rounding and nothing else: 0xF0 means granularity and never
    ; anything else.
    mov ah, 0x08
    call far [entry]
    or ax, ax
    jz f_gsetup
    mov [gran_before], dx
    mov ah, 0x09
    mov dx, 1
    call far [entry]
    or ax, ax
    jz f_gsetup
    mov [gran_handle], dx
    mov ah, 0x08
    call far [entry]
    or ax, ax
    jz f_gsetup
    mov ax, [gran_before]
    sub ax, dx                    ; kilobytes the 1 KB block actually consumed
    cmp ax, 1
    jne f_gran
    mov ah, 0x0A
    mov dx, [gran_handle]
    call far [entry]
    or ax, ax
    jz f_gsetup

    mov al, OK
    jmp sig

; 0xEF and 0xF0 are the two ASSERTIONS. Everything else here is setup, split
; off so an absent driver, a too-small arena, an arena that could not be
; fragmented, or a 1 KB block that could not be allocated at all never reports
; as a failed assertion. Same split, and the same reason, as emmprobe.asm's
; f_xms_* codes.
f_noxms:    mov al, 0xD0        ; setup: no XMS driver installed
            jmp sig
f_floor:    mov al, 0xD1        ; setup: arena too small to run the assertions
            jmp sig
f_largest:  mov al, 0xEF        ; ASSERTION: 08h collapsed largest into total
            jmp sig
f_gran:     mov al, 0xF0        ; ASSERTION: a 1 KB block did not cost 1 KB
            jmp sig
f_frag:     mov al, 0xF1        ; setup: could not fragment the arena
            jmp sig
f_gsetup:   mov al, 0xF2        ; setup: could not size or place the 1 KB block

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

entry:       dd 0
frag_lo:     dw 0
frag_hi:     dw 0
gran_before: dw 0
gran_handle: dw 0
