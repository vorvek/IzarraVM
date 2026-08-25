; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only
;
; 32-bit payload entry. pm16 jmps here after filling linear 0x200000.
; Watcom -3s: C names have no trailing underscore (desk_main, v86_call).

bits 32
cpu 386

%include "stubabi.inc"

global _start
global stub_lin_slot
global v86_call
extern desk_main

section _TEXT class=CODE use32

; First 8 bytes are patched by pm16 before the jump to _start.
stub_lin_slot:
    dd 0
    dd 0
_start:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, [stub_lin_slot + 4]
    xor ebp, ebp
    call desk_main
.hang:
    jmp .hang

; Fill StubAbi then far-jump to stub CS 0x18. Return lands at v86_back with
; EBX = saved ESP. Use the VALUE at stub_lin_slot (lin_base), not the label
; address 0x200000+disp.
v86_call:
    push ebx
    push esi
    push edi
    push ebp
    mov eax, [stub_lin_slot]
    mov [eax + STUB_ABI_OFF + ABI_SAVED_ESP], esp
    mov dword [eax + STUB_ABI_OFF + ABI_RET_EIP], v86_back
    mov word [eax + STUB_ABI_OFF + ABI_RET_CS], 0x08
    jmp far [eax + STUB_ABI_OFF + ABI_THUNK_OUT]
v86_back:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, ebx
    cld
    pop ebp
    pop edi
    pop esi
    pop ebx
    ret
