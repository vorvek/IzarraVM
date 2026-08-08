; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Small-memory XMS/VCPI pool probe. It accepts an empty VCPI pool, but checks
; that every reported or allocated page stays below detected physical RAM.
cpu 386
org 0x100
%define OK 0xA5

start:
    mov ax, 0xE801
    int 0x15
    jc f_bios
    mov si, ax
    mov di, bx
    mov ax, si
    or ax, di
    jnz .have_pair
    mov si, cx
    mov di, dx
.have_pair:
    movzx eax, si
    shl eax, 10
    movzx ebx, di
    shl ebx, 16
    add eax, ebx
    add eax, 0x100000
    cmp eax, 0x4000000
    jbe .top_ok
    mov eax, 0x4000000
.top_ok:
    mov [ram_top], eax

    mov ax, 0x4300
    int 0x2F
    cmp al, 0x80
    jne f_xms
    mov ax, 0x4310
    int 0x2F
    mov [xms_entry], bx
    mov [xms_entry+2], es
    mov ah, 0x08
    call far [xms_entry]
    cmp ax, dx
    ja f_xms                      ; largest run cannot exceed the total
    ; The pool must fit inside installed RAM -- which is what this fixture is
    ; for. It used to assert `dx <= 2048`, i.e. the old hard 2 MB XMS_EMB_BYTES
    ; cap; with XMS and VCPI sharing the whole extended category that ceiling is
    ; gone, and encoding it here only re-asserted the defect being removed.
    movzx ecx, dx
    shl ecx, 10                   ; free KB -> bytes
    add ecx, 0x100000             ; the pool starts above the first MB
    cmp ecx, [ram_top]
    ja f_xms

    mov ax, 0xDE00
    int 0x67
    or ah, ah
    jnz f_pres
    cmp bx, 0x0100
    jne f_pres
    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_count
    mov [free0], edx
    mov ax, 0xDE02
    int 0x67
    or ah, ah
    jnz f_max
    test edx, edx
    jz .empty
    test edx, 0xFFF
    jnz f_max
    cmp edx, [ram_top]
    jae f_max
    test dword [free0], 0xFFFFFFFF
    jz f_max
    mov [max_page], edx
    mov ax, 0xDE04
    int 0x67
    or ah, ah
    jnz f_alloc
    test edx, 0xFFF
    jnz f_alloc
    cmp edx, [max_page]
    ja f_alloc
    cmp edx, [ram_top]
    jae f_alloc
    mov [page], edx
    mov ax, 0xDE03
    int 0x67
    mov ecx, [free0]
    dec ecx
    cmp edx, ecx
    jne f_alloc
    mov edx, [page]
    mov ax, 0xDE05
    int 0x67
    or ah, ah
    jnz f_free
    jmp .balanced

.empty:
    test dword [free0], 0xFFFFFFFF
    jnz f_max
    mov ax, 0xDE04
    int 0x67
    cmp ah, 0x88
    jne f_alloc
    xor edx, edx
    mov ax, 0xDE05
    int 0x67
    cmp ah, 0x8A
    jne f_free

.balanced:
    mov ax, 0xDE03
    int 0x67
    cmp edx, [free0]
    jne f_count
    ; F001h used to answer the split-pool magic 'TL' with free VCPI KB in DX.
    ; XMS and VCPI now share one pool, so there is no second pool to report and
    ; both subfunctions answer the totals magic 'TK' with DX = 0. A caller that
    ; still summed DX onto the XMS free count would double-count the same pages.
    mov ax, 0xF001
    call far [xms_entry]
    cmp ax, 0x544B
    jne f_report
    mov [report_total], bx
    mov [report_ems], cx
    test dx, dx
    jnz f_report
    mov ax, 0xF000
    call far [xms_entry]
    cmp ax, 0x544B
    jne f_report
    cmp bx, [report_total]
    jne f_report
    cmp cx, [report_ems]
    jne f_report
    mov al, OK
    jmp sig

f_bios:   mov al, 0xE0
          jmp sig
f_xms:    mov al, 0xE1
          jmp sig
f_pres:   mov al, 0xE2
          jmp sig
f_count:  mov al, 0xE3
          jmp sig
f_max:    mov al, 0xE4
          jmp sig
f_alloc:  mov al, 0xE5
          jmp sig
f_free:   mov al, 0xE6
          jmp sig
f_report: mov al, 0xE7

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

align 4
ram_top: dd 0
free0: dd 0
max_page: dd 0
page: dd 0
report_total: dw 0
report_ems: dw 0
xms_entry: dd 0
