; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only
;
; StubAbi lives at file offset STUB_ABI_OFF in stub.bin (linear stub_lin+40).
; C (v86.h) must match the first ABI_C_SIZE bytes. The rest is asm-only.

STUB_ABI_OFF    equ 40

ABI_VECTOR      equ 0
ABI_AX          equ 2
ABI_BX          equ 4
ABI_CX          equ 6
ABI_DX          equ 8
ABI_SI          equ 10
ABI_DI          equ 12
ABI_DS          equ 14
ABI_ES          equ 16
ABI_FLAGS       equ 18
ABI_ERR         equ 20
ABI_PAD         equ 22
ABI_BOUNCE_LIN  equ 24
ABI_BOUNCE_OFF  equ 28
ABI_RM_SEG      equ 30
ABI_SAVED_ESP   equ 32
ABI_RET_EIP     equ 36
ABI_RET_CS      equ 40
ABI_THUNK_OUT   equ 42
ABI_SIZE        equ 48
ABI_C_SIZE      equ 32
