; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only
;
; 32-bit payload entry. copy32 jmps here after filling linear 0x200000.
; Watcom -3s: desk_main_ is the C name of desk_main.

bits 32
cpu 386

global _start
global stub_lin_slot
extern desk_main

section _TEXT class=CODE use32

; First 8 bytes are patched by copy32 before the jump to _start.
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
