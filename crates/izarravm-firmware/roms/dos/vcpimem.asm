; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; vcpimem.com: TOKAEMM VCPI query/page-pool fixture. Runs in V86 under a
; bare DEVICE=C:\DOS\TOKAEMM.SYS and exercises the DE02-DE0B set: the page
; pool (count/alloc/free round-trip, 12-LSB masking, bad-free and double-free
; rejection), the V86 page-table query, CR0, the debug-register array, and
; the 8259 mapping report/record round-trip.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin vcpimem.asm -o vcpimem.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; Reserve one shared-arena page through XMS before VCPI starts. The two
    ; interfaces must not return or release each other's pages.
    mov ax, 0x4300
    int 0x2F
    cmp al, 0x80
    jne f_xms
    mov ax, 0x4310
    int 0x2F
    mov [xms_entry], bx
    mov [xms_entry+2], es
    mov ah, 0x09
    mov dx, 4
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov [xms_handle], dx
    mov ah, 0x0C
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov [xms_page], bx
    mov [xms_page+2], dx

    ; 1. DE03: free page count >= 256 (a real pool, at least 1 MB on this box)
    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_count
    mov [free0], edx
    cmp edx, 256
    jb f_count

    ; 2. DE02: highest page: nonzero, 4K-aligned, above 1 MB
    mov ax, 0xDE02
    int 0x67
    or ah, ah
    jnz f_max
    test edx, 0xFFF
    jnz f_max
    cmp edx, 0x100000
    jb f_max
    mov [maxpg], edx

    ; 3. DE04: allocate a page: 4K-aligned, above 1 MB, <= the DE02 ceiling,
    ;    and DE03 drops by exactly one
    mov ax, 0xDE04
    int 0x67
    or ah, ah
    jnz f_alloc
    test edx, 0xFFF
    jnz f_alloc
    cmp edx, 0x100000
    jb f_alloc
    cmp edx, [maxpg]
    ja f_alloc
    cmp edx, [xms_page]
    je f_owner
    mov [page1], edx
    mov ax, 0xDE03
    int 0x67
    mov ecx, [free0]
    dec ecx
    cmp edx, ecx
    jne f_alloc

    ; 4. DE05: free it (with junk in the 12 LSBs: the server must mask),
    ;    and DE03 returns to the starting count
    mov edx, [page1]
    or edx, 0xABC
    mov ax, 0xDE05
    int 0x67
    or ah, ah
    jnz f_free
    mov ax, 0xDE03
    int 0x67
    cmp edx, [free0]
    jne f_free

    ; DE05 must reject the page owned by the live XMS handle.
    mov edx, [xms_page]
    mov ax, 0xDE05
    int 0x67
    or ah, ah
    jz f_owner

    ; 5. DE05 on conventional memory (never a pool page) -> nonzero AH
    mov edx, 0x5000
    mov ax, 0xDE05
    int 0x67
    or ah, ah
    jz f_badfree

    ; 6. DE05 double-free of the already-freed page -> nonzero AH
    mov edx, [page1]
    mov ax, 0xDE05
    int 0x67
    or ah, ah
    jz f_dfree

    ; 7. DE06: V86 page 0 -> phys 0 (identity); page 0xB8 -> 0xB8000 (VGA
    ;    text, identity); page 0x1FF (past the furnished window) -> AH=8Bh
    xor cx, cx
    mov ax, 0xDE06
    int 0x67
    or ah, ah
    jnz f_pt
    test edx, edx
    jnz f_pt
    mov cx, 0xB8
    mov ax, 0xDE06
    int 0x67
    or ah, ah
    jnz f_pt
    cmp edx, 0xB8000
    jne f_pt
    mov cx, 0x1FF
    mov ax, 0xDE06
    int 0x67
    cmp ah, 0x8B
    jne f_pt

    ; 8. DE07: CR0 has PE (bit 0) and PG (bit 31) set (we run in V86 under
    ;    the paging monitor)
    mov ax, 0xDE07
    int 0x67
    or ah, ah
    jnz f_cr0
    mov eax, ebx
    and eax, 0x80000001
    cmp eax, 0x80000001
    jne f_cr0

    ; 9. DE08: read the debug registers into a poisoned buffer; AH=0 and the
    ;    DR4/DR5 slots (unused per the interface) come back zero
    mov di, drbuf
    mov cx, 16
    mov ax, 0xA5A5
    push di
    cld
    rep stosw                     ; poison all 8 dwords (ES=CS=DS in a .COM)
    pop di
    mov ax, 0xDE08
    int 0x67
    or ah, ah
    jnz f_dr
    cmp dword [drbuf+16], 0
    jne f_dr
    cmp dword [drbuf+20], 0
    jne f_dr

    ; 10. DE0A: the DOS-default mapping (BX=8, CX=70h); DE0B records a remap
    ;     report and DE0A echoes it back; then restore the defaults
    mov ax, 0xDE0A
    int 0x67
    or ah, ah
    jnz f_pic
    cmp bx, 8
    jne f_pic
    cmp cx, 0x70
    jne f_pic
    cli
    mov bx, 0x50
    mov cx, 0x58
    mov ax, 0xDE0B
    int 0x67
    sti
    or ah, ah
    jnz f_pic
    mov ax, 0xDE0A
    int 0x67
    cmp bx, 0x50
    jne f_pic
    cmp cx, 0x58
    jne f_pic
    cli
    mov bx, 8
    mov cx, 0x70
    mov ax, 0xDE0B
    int 0x67
    sti
    or ah, ah
    jnz f_pic

    mov ah, 0x0D                 ; release the XMS page after the VCPI checks
    mov dx, [xms_handle]
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov ah, 0x0A
    mov dx, [xms_handle]
    call far [xms_entry]
    or ax, ax
    jz f_xms

    mov al, OK
    jmp sig

f_count:  mov al, 0xE1
          jmp sig
f_max:    mov al, 0xE2
          jmp sig
f_alloc:  mov al, 0xE3
          jmp sig
f_free:   mov al, 0xE4
          jmp sig
f_badfree: mov al, 0xE5
          jmp sig
f_dfree:  mov al, 0xE6
          jmp sig
f_pt:     mov al, 0xE7
          jmp sig
f_cr0:    mov al, 0xE8
          jmp sig
f_dr:     mov al, 0xE9
          jmp sig
f_pic:    mov al, 0xEA
          jmp sig
f_xms:    mov al, 0xEB
          jmp sig
f_owner:  mov al, 0xEC

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

align 4
free0:  dd 0
maxpg:  dd 0
page1:  dd 0
xms_entry: dd 0
xms_page: dd 0
xms_handle: dw 0
drbuf:  times 32 db 0
