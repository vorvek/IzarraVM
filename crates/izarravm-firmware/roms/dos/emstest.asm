; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; EMS fixture. Runs in V86 under TOKAEMM loaded with
; DEVICE=C:\DOS\TOKAEMM.SYS RAM (the page frame provisioned).
;
; version -> frame segment -> page counts -> allocate 4 pages -> map logical
; pages through the frame slots, writing distinct patterns and reading them
; back through OTHER slots (the runtime-remap proof: the same backing page is
; visible wherever it is mapped) -> save context -> unmap -> restore context
; (the mapping comes back) -> map/unmap multiple pages in one call (50h, both
; the page-number and the segment form, plus its error answers) -> get/set
; handle names (53h: fresh name is zeros, set/get round-trip, duplicate ->
; A1h, bad handle/subfunction, the name dies with the handle) -> free and
; watch the counts recover -> then signal 0xA5 (success) via the unit-tester
; exit port. Any other code names the step that broke (0xEn, 0xDn for the 50h
; steps, 0xCn for the 53h steps).
;
; Build: nasm -f bin emstest.asm -o emstest.com
cpu 386
org 0x100
%define OK 0xA5
%define PAT_A 0xA55A1234
%define PAT_B 0x0FF0C3C3

start:
    ; 1. version (46h): AL = BCD 4.0
    mov ah, 0x46
    int 0x67
    or ah, ah
    jnz f_ver
    cmp al, 0x40
    jne f_ver

    ; 2. page frame segment (41h): BX = 0xE000
    mov ah, 0x41
    int 0x67
    or ah, ah
    jnz f_frame
    cmp bx, 0xE000
    jne f_frame

    ; 3. page counts (42h): EMS now shares the arena with XMS/VCPI (Task 6),
    ; so the total is no longer the fixed 192-page partition, and free is not
    ; guaranteed to equal total even at the top of this program -- something
    ; else (e.g. the shell's own XMS-swap block for running this child) may
    ; already hold part of the pool, which is expected now that they share
    ; one. Record both as this run's baseline instead of pinning a constant;
    ; steps 12 and 14 check the DELTA against that baseline.
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_counts
    cmp bx, dx
    ja f_counts                   ; free > total is an impossible state
    cmp bx, 16                    ; need enough headroom to allocate 4 pages
    jb f_counts                   ; and still prove a count that moves
    mov [ems_free0], bx

    ; 4. allocate 4 logical pages (43h) -> DX = handle
    mov ah, 0x43
    mov bx, 4
    int 0x67
    or ah, ah
    jnz f_alloc
    mov [handle], dx

    ; 5. map logical 0 -> slot 0 (44h); write pattern A through the frame
    mov ah, 0x44
    xor al, al
    xor bx, bx
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_map0
    mov ax, 0xE000
    mov es, ax
    mov dword [es:0], PAT_A
    cmp dword [es:0], PAT_A
    jne f_map0

    ; 6. map logical 1 -> slot 0; write pattern B
    mov ah, 0x44
    xor al, al
    mov bx, 1
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_map1
    mov dword [es:0], PAT_B
    cmp dword [es:0], PAT_B
    jne f_map1

    ; 7. map logical 0 -> slot 1: pattern A must be visible at E400 (the
    ;    remap proof — the backing page moved to a different frame window)
    mov ah, 0x44
    mov al, 1
    xor bx, bx
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_remap
    mov ax, 0xE400
    mov es, ax
    cmp dword [es:0], PAT_A
    jne f_remap

    ; 8. map logical 1 -> slot 1: pattern B follows it
    mov ah, 0x44
    mov al, 1
    mov bx, 1
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_remap2
    cmp dword [es:0], PAT_B
    jne f_remap2

    ; 9. save the mapping context (47h) under the handle
    mov ah, 0x47
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_save

    ; 10. unmap slot 1 (44h, logical 0xFFFF): E400 falls back to the dormant
    ;     UMB backing — pattern B must no longer be visible there
    mov ah, 0x44
    mov al, 1
    mov bx, 0xFFFF
    mov dx, [handle]              ; the unmap form still requires a valid handle
    int 0x67
    or ah, ah
    jnz f_unmap
    cmp dword [es:0], PAT_B
    je f_unmap

    ; 11. restore the context (48h): slot 1 maps logical 1 again -> pattern B
    mov ah, 0x48
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_restore
    cmp dword [es:0], PAT_B
    jne f_restore

    ; 11a. map/unmap multiple (50h subfn 0, physical page NUMBERS): one call
    ;      maps logical 0 -> slot 2 and logical 1 -> slot 3. UW.EXE maps its
    ;      whole frame with one 5000h call at startup and aborts with its
    ;      error C003 when the function answers 84h.
    mov ax, 0x5000
    mov cx, 2
    mov dx, [handle]
    mov si, map50
    int 0x67
    or ah, ah
    jnz f_mmap
    mov ax, 0xE800
    mov es, ax
    cmp dword [es:0], PAT_A
    jne f_mmap
    mov ax, 0xEC00
    mov es, ax
    cmp dword [es:0], PAT_B
    jne f_mmap

    ; 11b. one 5000h call unmaps both (logical 0xFFFF): the patterns fall out
    ;      of the frame windows again
    mov ax, 0x5000
    mov cx, 2
    mov dx, [handle]
    mov si, unmap50
    int 0x67
    or ah, ah
    jnz f_mmapu
    mov ax, 0xE800
    mov es, ax
    cmp dword [es:0], PAT_A
    je f_mmapu
    mov ax, 0xEC00
    mov es, ax
    cmp dword [es:0], PAT_B
    je f_mmapu

    ; 11c. segment form (50h subfn 1): map logical 0 -> segment E800h, see
    ;      pattern A, then unmap it again
    mov ax, 0x5001
    mov cx, 1
    mov dx, [handle]
    mov si, map51
    int 0x67
    or ah, ah
    jnz f_mseg
    mov ax, 0xE800
    mov es, ax
    cmp dword [es:0], PAT_A
    jne f_mseg
    mov ax, 0x5001
    mov cx, 1
    mov dx, [handle]
    mov si, unmap51
    int 0x67
    or ah, ah
    jnz f_mseg
    cmp dword [es:0], PAT_A
    je f_mseg

    ; 11d. error answers: subfunction 2 -> 8Fh, physical page 4 -> 8Bh,
    ;      logical past the allocation -> 8Ah, segment off the frame -> 8Bh
    mov ax, 0x5002
    mov cx, 1
    mov dx, [handle]
    mov si, map50
    int 0x67
    cmp ah, 0x8F
    jne f_merr
    mov ax, 0x5000
    mov cx, 1
    mov dx, [handle]
    mov si, badphys50
    int 0x67
    cmp ah, 0x8B
    jne f_merr
    mov ax, 0x5000
    mov cx, 1
    mov dx, [handle]
    mov si, badlog50
    int 0x67
    cmp ah, 0x8A
    jne f_merr
    mov ax, 0x5001
    mov cx, 1
    mov dx, [handle]
    mov si, badseg51
    int 0x67
    cmp ah, 0x8B
    jne f_merr

    ; 11e. handle name, get (5300h): a fresh handle's name is 8 zero bytes.
    ;      1830.EXE opens EMMXXXX0, allocates one page, then REQUIRES 5301h
    ;      to succeed; an 84h answer makes it print "You must have at least
    ;      2700K of expanded memory." and exit before any page math.
    push ds
    pop es
    mov di, name_buf
    mov ax, 0x5300
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_name
    mov si, name_buf
    mov cx, 8
nm_zero:
    lodsb
    or al, al
    jnz f_name
    loop nm_zero

    ; 11f. set a name (5301h), then read it back through 5300h
    mov ax, 0x5301
    mov dx, [handle]
    mov si, name_a
    int 0x67
    or ah, ah
    jnz f_name2
    mov ax, 0x5300
    mov dx, [handle]
    mov di, name_buf
    int 0x67
    or ah, ah
    jnz f_name2
    mov si, name_a
    mov di, name_buf
    mov cx, 8
    repe cmpsb
    jne f_name2

    ; 11g. a second handle may not take the same name (A1h); a different
    ;      name is accepted
    mov ah, 0x43
    mov bx, 1
    int 0x67
    or ah, ah
    jnz f_name3
    mov [handle2], dx
    mov ax, 0x5301
    mov dx, [handle2]
    mov si, name_a
    int 0x67
    cmp ah, 0xA1
    jne f_name3
    mov ax, 0x5301
    mov dx, [handle2]
    mov si, name_b
    int 0x67
    or ah, ah
    jnz f_name3

    ; 11h. error answers: unknown handle -> 83h, subfunction 2 -> 8Fh
    mov ax, 0x5300
    mov dx, 0x00FF
    mov di, name_buf
    int 0x67
    cmp ah, 0x83
    jne f_name4
    mov ax, 0x5302
    mov dx, [handle]
    int 0x67
    cmp ah, 0x8F
    jne f_name4
    mov ax, 0x5302                ; subfunction outranks handle: a bad
    mov dx, 0x00FF                ; handle must not turn 8Fh into 83h
    int 0x67
    cmp ah, 0x8F
    jne f_name4

    ; 11i. the name dies with the handle: release handle2, reallocate, and
    ;      5300h on the fresh handle must return 8 ZERO bytes -- this read is
    ;      the step's red proof (the dup scan skips free slots and a handle's
    ;      own entry, so a stale name could never answer A1h here). Then take
    ;      name_b again, and release so steps 12-14 see only the main handle.
    mov ah, 0x45
    mov dx, [handle2]
    int 0x67
    or ah, ah
    jnz f_name5
    mov ah, 0x43
    mov bx, 1
    int 0x67
    or ah, ah
    jnz f_name5
    mov [handle2], dx
    mov ax, 0x5300
    mov di, name_buf              ; ES = DS since 11e
    int 0x67
    or ah, ah
    jnz f_name5
    mov si, name_buf
    mov cx, 8
nm_zero2:
    lodsb
    or al, al
    jnz f_name5
    loop nm_zero2
    mov ax, 0x5301
    mov dx, [handle2]
    mov si, name_b
    int 0x67
    or ah, ah
    jnz f_name5
    mov ah, 0x45
    mov dx, [handle2]
    int 0x67
    or ah, ah
    jnz f_name5

    ; 11j. hardware info (59h, LIM 4.0 OS/E). Subfn 01: unallocated/total RAW
    ;      pages -- raw pages ARE 16 KB pages here, so both counts must equal
    ;      42h's. 1830's streaming module sizes its EMS pool from 5901h's BX
    ;      WITHOUT checking AH; an 84h answer left BX stale and the pool came
    ;      out 50 pages instead of ~1900. Subfn 00: the 5-word hardware array
    ;      (word 0 = raw page size in paragraphs = 0x400). Subfn 2 -> 8Fh.
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_raw
    mov si, bx                    ; SI = free per 42h
    mov di, dx                    ; DI = total per 42h
    mov ax, 0x5901
    int 0x67
    or ah, ah
    jnz f_raw
    cmp bx, si
    jne f_raw
    cmp dx, di
    jne f_raw
    push ds
    pop es
    mov di, hw_buf
    mov ax, 0x5900
    int 0x67
    or ah, ah
    jnz f_raw
    cmp word [hw_buf], 0x0400
    jne f_raw
    mov ax, 0x5902
    int 0x67
    cmp ah, 0x8F
    jne f_raw

    ; 12. counts reflect the allocation (42h): free dropped by exactly the 4
    ; pages this program holds, from the baseline step 3 recorded.
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_counts2
    mov ax, [ems_free0]
    sub ax, 4
    cmp bx, ax
    jne f_counts2

    ; 13. pages for the handle (4Ch) = 4; open handles (4Bh) = 1
    mov ah, 0x4C
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_pages
    cmp bx, 4
    jne f_pages
    mov ah, 0x4B
    int 0x67
    or ah, ah
    jnz f_pages
    cmp bx, 1
    jne f_pages

    ; 14. free the handle (45h); counts recover, no open handles remain
    mov ah, 0x45
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_free
    mov ah, 0x42
    int 0x67
    cmp bx, [ems_free0]
    jne f_free
    mov ah, 0x4B
    int 0x67
    or bx, bx
    jnz f_free

    mov al, OK
    jmp sig

f_ver:    mov al, 0xE1
          jmp sig
f_frame:  mov al, 0xE2
          jmp sig
f_counts: mov al, 0xE3
          jmp sig
f_alloc:  mov al, 0xE4
          jmp sig
f_map0:   mov al, 0xE5
          jmp sig
f_map1:   mov al, 0xE6
          jmp sig
f_remap:  mov al, 0xE7
          jmp sig
f_remap2: mov al, 0xE8
          jmp sig
f_save:   mov al, 0xE9
          jmp sig
f_unmap:  mov al, 0xEA
          jmp sig
f_restore: mov al, 0xEB
          jmp sig
f_counts2: mov al, 0xEC
          jmp sig
f_pages:  mov al, 0xED
          jmp sig
f_free:   mov al, 0xEE
          jmp sig
f_mmap:   mov al, 0xD1
          jmp sig
f_mmapu:  mov al, 0xD2
          jmp sig
f_mseg:   mov al, 0xD3
          jmp sig
f_merr:   mov al, 0xD4
          jmp sig
f_name:   mov al, 0xC1
          jmp sig
f_name2:  mov al, 0xC2
          jmp sig
f_name3:  mov al, 0xC3
          jmp sig
f_name4:  mov al, 0xC4
          jmp sig
f_name5:  mov al, 0xC5
          jmp sig
f_raw:    mov al, 0xC6

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

handle: dw 0
handle2: dw 0
ems_free0: dw 0
name_a:   db '1830RAIL'
name_b:   db 'ZUGZWANG'
name_buf: times 8 db 0xAA         ; prefilled: 11e proves 5300h wrote zeros
hw_buf:   times 10 db 0xAA        ; 5900h's 5-word hardware array (11j)
; 50h arrays: (logical, physical) word pairs
map50:     dw 0, 2, 1, 3
unmap50:   dw 0xFFFF, 2, 0xFFFF, 3
map51:     dw 0, 0xE800
unmap51:   dw 0xFFFF, 0xE800
badphys50: dw 0, 4
badlog50:  dw 7, 2
badseg51:  dw 0, 0xE123
