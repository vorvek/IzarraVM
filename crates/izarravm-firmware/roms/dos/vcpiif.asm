; vcpiif.com: TOKAEMM VCPI DE01 (Get Protected Mode Interface) fixture.
; Runs in V86 under a bare DEVICE=C:\DOS\TOKAEMM.SYS and validates every
; V86-observable effect of DE01: the page-table copy (identity first-MB
; entries, software bits 9-11 cleared, exactly 0x110 entries written, DI
; advanced to match), and the three furnished GDT descriptors (32-bit CPL0
; code with a sane limit, the exact flat-4GB data mirror, the driver-data
; mirror), plus a nonzero in-segment PM entry offset in EBX. The PM entry
; itself is exercised by the switch fixture because it can only be far-called
; from protected mode.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin vcpiif.asm -o vcpiif.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; poison the buffers so "server wrote it" is distinguishable
    cld
    mov di, ptbuf
    mov cx, (0x440 + 32 + 32) / 2 ; page-table buffer + slack + GDT area
    mov ax, 0xFFFF
    rep stosw

    ; DE01: ES:DI -> page-table buffer, DS:SI -> three GDT entries
    mov di, ptbuf
    mov si, gdtbuf
    mov ax, 0xDE01
    int 0x67
    or ah, ah
    jnz f_call
    cmp di, ptbuf + 0x440         ; DI -> first unused page table entry
    jne f_di
    or ebx, ebx                   ; PM entry offset: nonzero, inside a
    jz f_ebx                      ; sub-64K code segment
    cmp ebx, 0x10000
    jae f_ebx

    ; PTE spot checks: identity first MB, present/rw/user (flags 7). The
    ; CPU sets Accessed/Dirty (bits 5-6) in live PTEs as the guest runs, so
    ; mask them out of the comparison.
    mov eax, [ptbuf]                       ; page 0 -> phys 0
    and eax, ~0x60
    cmp eax, 0x00000007
    jne f_pte
    mov eax, [ptbuf + 0xB8*4]              ; VGA text page, identity
    and eax, ~0x60
    cmp eax, 0x000B8007
    jne f_pte
    mov eax, [ptbuf + 0x100*4]             ; A20/HMA window, va20=1 boot
    and eax, ~0x60
    cmp eax, 0x00100007
    jne f_pte

    ; software bits 9-11 clear in every copied entry; every entry present
    mov cx, 0x110
    mov si, ptbuf
.pchk:
    mov eax, [si]
    test eax, 0xE00               ; bits 9-11 must be cleared by the server
    jnz f_bits
    test al, 1                    ; the whole window is present-mapped
    jz f_bits
    add si, 4
    loop .pchk

    ; the server wrote exactly 0x110 entries: the poison beyond survives
    cmp dword [ptbuf + 0x440], 0xFFFFFFFF
    jne f_over

    ; descriptor 0: 32-bit CPL0 code. access 0x9B; flags D=1 G=0, limit
    ; high nibble 0; limit nonzero; base 31..24 zero (driver is low memory)
    cmp byte [gdtbuf+5], 0x9B
    jne f_code
    cmp byte [gdtbuf+6], 0x40
    jne f_code
    cmp word [gdtbuf], 0
    je f_code
    cmp byte [gdtbuf+7], 0
    jne f_code

    ; descriptor 1: the exact flat-4GB data mirror
    cmp dword [gdtbuf+8], 0x0000FFFF
    jne f_flat
    cmp dword [gdtbuf+12], 0x00CF9300
    jne f_flat

    ; descriptor 2: driver data: access 0x93, flags 0xCF, limit FFFF, and
    ; the same base as the code descriptor (both are base_lin)
    cmp word [gdtbuf+16], 0xFFFF
    jne f_data
    cmp byte [gdtbuf+21], 0x93
    jne f_data
    cmp byte [gdtbuf+22], 0xCF
    jne f_data
    mov ax, [gdtbuf+2]            ; code base 15..0
    cmp ax, [gdtbuf+18]           ; data base 15..0
    jne f_data
    mov al, [gdtbuf+4]            ; code base 23..16
    cmp al, [gdtbuf+20]
    jne f_data

    ; the poison past the third descriptor survives
    cmp dword [gdtbuf+24], 0xFFFFFFFF
    jne f_over

    mov al, OK
    jmp sig

f_call:   mov al, 0xE1
          jmp sig
f_di:     mov al, 0xE2
          jmp sig
f_ebx:    mov al, 0xE3
          jmp sig
f_pte:    mov al, 0xE4
          jmp sig
f_bits:   mov al, 0xE5
          jmp sig
f_over:   mov al, 0xE6
          jmp sig
f_code:   mov al, 0xE7
          jmp sig
f_flat:   mov al, 0xE8
          jmp sig
f_data:   mov al, 0xE9

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
ptbuf:  times 0x440 db 0         ; the 0x110 PTEs DE01 fills
        times 32 db 0            ; overwrite-guard slack (stays poisoned)
gdtbuf: times 24 db 0            ; the three descriptors
        times 32 db 0            ; overwrite-guard slack (stays poisoned)
