; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only
;
; 16-bit MZ stub for TOKADESK.EXE. VCPI client, VBE 0x4117.
; The first payload is small enough to read in V86 into the 4K bounce.
; 16-bit protected mode copies it to linear 0x200000, then far-jumps into
; the 32-bit code segment. DE0C lands on a 16-bit CS first: a D=1 landing
; from the monitor is not how vcpisw.asm switches.
; Build: nasm -f bin stub.asm -o stub.bin
cpu 386
org 0

PAYLOAD_LIN equ 0x200000
STACK16     equ 0x0400

jmp start
times 16-($-$$) db 0
hdr_overlay     dd 0
hdr_payload     dd 0
hdr_bss         dd 0
hdr_stack       dd 0
hdr_n           dd 0
hdr_stub_bytes  dd 0

start:
    mov [cs:psp_seg], es
    mov ax, cs
    mov ds, ax
    mov ss, ax
    mov sp, stack16_top
    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [lin_base], eax
    mov [rm_seg], cs
    mov [rm_sp], sp

    ; Shrink the MCB to PSP + load image.
    mov ax, [psp_seg]
    mov es, ax
    mov bx, cs
    sub bx, ax
    mov eax, [hdr_stub_bytes]
    add eax, 15
    shr eax, 4
    add bx, ax
    add bx, 1
    mov ah, 0x4A
    int 0x21

    ; VCPI 1.0
    mov ax, 0xDE00
    int 0x67
    or ah, ah
    jnz fail_vcpi
    cmp bx, 0x0100
    jne fail_vcpi

    ; VBE 1024x768x16 linear
    mov ax, 0x4F02
    mov bx, 0x4117
    int 0x10
    cmp ax, 0x004F
    jne fail_vbe

    call align_area
    call build_tables
    call alloc_payload
    call open_self
    call read_payload
    call switch_to_copy32
    ; never returns
    jmp fail_hang

fail_vcpi:
    mov dx, msg_vcpi
    mov al, 0xE1
    jmp fail
fail_vbe:
    mov dx, msg_vbe
    mov al, 0xE3
    jmp fail
fail_de01:
    mov dx, msg_de01
    mov al, 0xE2
    jmp fail
fail_alloc:
    mov dx, msg_alloc
    mov al, 0xE4
    jmp fail
fail_open:
    mov dx, msg_open
    mov al, 0xE5
    jmp fail
fail_hang:
    mov dx, msg_hang
    mov al, 0xEF
fail:
    push ax
    mov ax, cs
    mov ds, ax
    mov ah, 0x09
    int 0x21
    pop ax
    call ut_exit
.die:
    jmp .die

; AL = exit code. Lotura CMD_EXIT.
ut_exit:
    push ax
    mov al, 12
    out 0xE4, al
    pop ax
    out 0xE5, al
    mov al, 3
    out 0xE6, al
    ret

; Round area up to 4K. Layout: PD, PT0, LFB PT, MMIO PT, bounce.
align_area:
    mov eax, [lin_base]
    add eax, area
    add eax, 0xFFF
    and eax, 0xFFFFF000
    mov [pd_phys], eax
    add eax, 0x1000
    mov [pt0_phys], eax
    add eax, 0x1000
    mov [lfb_pt_phys], eax
    add eax, 0x1000
    mov [mmio_pt_phys], eax
    add eax, 0x1000
    mov [bounce_phys], eax
    mov eax, [pd_phys]
    sub eax, [lin_base]
    mov [pd_off], ax
    mov eax, [pt0_phys]
    sub eax, [lin_base]
    mov [pt0_off], ax
    mov eax, [lfb_pt_phys]
    sub eax, [lin_base]
    mov [lfb_pt_off], ax
    mov eax, [mmio_pt_phys]
    sub eax, [lin_base]
    mov [mmio_pt_off], ax
    mov eax, [bounce_phys]
    sub eax, [lin_base]
    mov [bounce_off], ax
    ret

build_tables:
    push es
    push cs
    pop es
    ; zero PD
    mov di, [pd_off]
    mov cx, 0x800
    xor ax, ax
    rep stosw
    ; zero PT0 then DE01 fills 0..0x10F
    mov di, [pt0_off]
    mov cx, 0x800
    xor ax, ax
    rep stosw
    pop es

    mov di, [pt0_off]
    mov si, gdt + 0x30
    push cs
    pop es
    mov ax, 0xDE01
    int 0x67
    or ah, ah
    jnz fail_de01
    mov [entry_off], ebx
    mov word [entry_sel], 0x30

    ; PD[0] = PT0 | P|R/W|U
    mov bx, [pd_off]
    mov eax, [pt0_phys]
    or eax, 7
    mov [bx], eax
    ; PD[0x380] LFB PT, PD[0x381] MMIO PT. Supervisor, PCD.
    mov eax, [lfb_pt_phys]
    or eax, 0x13
    mov [bx + 0x380 * 4], eax
    mov eax, [mmio_pt_phys]
    or eax, 0x13
    mov [bx + 0x381 * 4], eax

    call fill_lfb_pt
    call fill_mmio_pt
    call fill_gdt
    call fill_swst
    ret

fill_lfb_pt:
    push es
    push cs
    pop es
    mov di, [lfb_pt_off]
    xor eax, eax
    mov ecx, 1024
.z:
    stosd
    loop .z
    mov di, [lfb_pt_off]
    mov eax, 0xE0000000
    or eax, 0x13
    mov ecx, 1024
.p:
    stosd
    add eax, 0x1000
    loop .p
    pop es
    ret

fill_mmio_pt:
    push es
    push cs
    pop es
    mov di, [mmio_pt_off]
    xor eax, eax
    mov ecx, 1024
.z:
    stosd
    loop .z
    mov di, [mmio_pt_off]
    mov eax, 0xE0400000
    or eax, 0x13
    mov ecx, 16
.p:
    stosd
    add eax, 0x1000
    loop .p
    pop es
    ret

; 4GB flat code/data, 16-bit stub mirrors, TSS. Server trio already at 0x30.
fill_gdt:
    xor eax, eax
    mov dword [gdt+0], eax
    mov dword [gdt+4], eax
    ; 0x08 code32 base 0 limit 4GB D=1
    mov word [gdt+0x08], 0xFFFF
    mov word [gdt+0x08+2], 0
    mov byte [gdt+0x08+4], 0
    mov byte [gdt+0x08+5], 0x9B
    mov byte [gdt+0x08+6], 0xCF
    mov byte [gdt+0x08+7], 0
    ; 0x10 data32 base 0 limit 4GB B=1
    mov word [gdt+0x10], 0xFFFF
    mov word [gdt+0x10+2], 0
    mov byte [gdt+0x10+4], 0
    mov byte [gdt+0x10+5], 0x93
    mov byte [gdt+0x10+6], 0xCF
    mov byte [gdt+0x10+7], 0
    ; 0x18 code16 stub
    mov eax, [lin_base]
    mov word [gdt+0x18], 0xFFFF
    mov [gdt+0x18+2], ax
    shr eax, 16
    mov [gdt+0x18+4], al
    mov byte [gdt+0x18+5], 0x9B
    mov byte [gdt+0x18+6], 0
    mov [gdt+0x18+7], ah
    ; 0x20 data16 stub
    mov eax, [lin_base]
    mov word [gdt+0x20], 0xFFFF
    mov [gdt+0x20+2], ax
    shr eax, 16
    mov [gdt+0x20+4], al
    mov byte [gdt+0x20+5], 0x93
    mov byte [gdt+0x20+6], 0
    mov [gdt+0x20+7], ah
    ; 0x28 TSS
    mov eax, [lin_base]
    add eax, tss
    mov word [gdt+0x28], 0x67
    mov [gdt+0x28+2], ax
    shr eax, 16
    mov [gdt+0x28+4], al
    mov byte [gdt+0x28+5], 0x89
    mov byte [gdt+0x28+6], 0
    mov [gdt+0x28+7], ah
    mov word [gdtr_pd], 9 * 8 - 1
    mov eax, [lin_base]
    add eax, gdt
    mov [gdtr_pd+2], eax
    mov word [idtr_pd], 0
    mov dword [idtr_pd+2], 0
    ret

fill_swst:
    mov eax, [pd_phys]
    mov [swst+0], eax
    mov eax, [lin_base]
    add eax, gdtr_pd
    mov [swst+4], eax
    mov eax, [lin_base]
    add eax, idtr_pd
    mov [swst+8], eax
    mov word [swst+0x0C], 0
    mov word [swst+0x0E], 0x28
    ; Land 16-bit first (CS 0x18 base = lin_base, EIP = offset), then jump
    ; to the 32-bit CS. vcpisw.asm uses this shape; a D=1 landing is next.
    mov eax, pm16
    mov [swst+0x10], eax
    mov word [swst+0x14], 0x18
    ret

alloc_payload:
    mov ecx, [hdr_n]
    xor esi, esi
.map:
    jcxz .done
    mov ax, 0xDE04
    int 0x67
    or ah, ah
    jnz fail_alloc
    test edx, 0xFFF
    jnz fail_alloc
    cmp edx, 0x100000
    jb fail_alloc
    ; PT0[0x200 + i] = phys | P|R/W
    push ecx
    mov eax, edx
    or eax, 3
    mov bx, [pt0_off]
    mov edi, esi
    add edi, 0x200
    shl edi, 2
    add bx, di
    mov [bx], eax
    pop ecx
    inc esi
    dec ecx
    jmp .map
.done:
    ; zero unused PT0: 0x110..0x1FF and 0x200+N..0x3FF already zero from stosw
    ; except DE01 filled 0..0x10F. Good.
    ret

open_self:
    ; EXE path is after the environment: PSP:[2Ch] -> env, skip to the path.
    mov es, [psp_seg]
    mov ax, [es:0x2C]
    mov es, ax
    xor di, di
    xor ax, ax
.find:
    cmp word [es:di], 0
    je .endenv
    inc di
    jmp .find
.endenv:
    add di, 4
    push ds
    push es
    pop ds
    mov dx, di
    mov ax, 0x3D00
    int 0x21
    pop ds
    jc fail_open
    mov [file_handle], ax
    ; seek to overlay
    mov bx, ax
    mov dx, [hdr_overlay]
    mov cx, [hdr_overlay+2]
    mov ax, 0x4200
    int 0x21
    jc fail_open
    ret

read_payload:
    ; Payload is a few hundred bytes in PR 1 and fits in the 4K bounce.
    mov eax, [hdr_payload]
    cmp eax, 4096
    ja fail_open
    mov ah, 0x3F
    mov bx, [file_handle]
    mov cx, ax
    mov dx, [bounce_off]
    int 0x21
    jc fail_open
    cmp ax, [hdr_payload]
    jne fail_open
    ret

switch_to_copy32:
    mov ebp, [lin_base]
    mov eax, ebp
    add eax, swst
    mov esi, eax
    cli
    mov ax, 0xDE0C
    int 0x67
    jmp fail_hang

bits 16
pm16:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, stack16_top
    mov esi, [bounce_phys]
    mov ecx, [hdr_payload]
    mov ebx, [hdr_n]
    mov edi, PAYLOAD_LIN
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    a32 rep movsb
    mov eax, ebp
    a32 mov [dword PAYLOAD_LIN], eax
    shl ebx, 12
    add ebx, PAYLOAD_LIN
    a32 mov [dword PAYLOAD_LIN + 4], ebx
    mov ax, 0x20
    mov ds, ax
    mov dword [pm32_off], PAYLOAD_LIN + 8
    mov word [pm32_cs], 0x08
    jmp dword far [pm32_off]

msg_vcpi db 'TokaDESK needs TOKAEMM (VCPI).', 13, 10, '$'
msg_vbe  db 'TokaDESK: VBE mode 117h failed.', 13, 10, '$'
msg_de01 db 'TokaDESK: VCPI DE01 failed.', 13, 10, '$'
msg_alloc db 'TokaDESK: VCPI page alloc failed.', 13, 10, '$'
msg_open db 'TokaDESK: cannot read TOKADESK.EXE.', 13, 10, '$'
msg_hang db 'TokaDESK: mode switch failed.', 13, 10, '$'

align 4
lin_base        dd 0
pd_phys         dd 0
pt0_phys        dd 0
lfb_pt_phys     dd 0
mmio_pt_phys    dd 0
bounce_phys     dd 0
pd_off          dw 0
pt0_off         dw 0
lfb_pt_off      dw 0
mmio_pt_off     dw 0
bounce_off      dw 0
psp_seg         dw 0
rm_seg          dw 0
rm_sp           dw 0
file_handle     dw 0
entry_off       dd 0
entry_sel       dw 0
pm32_off        dd 0
pm32_cs         dw 0

align 8
gdt:            times 9 * 8 db 0
tss:            times 104 db 0
gdtr_pd:        times 6 db 0
idtr_pd:        times 6 db 0
swst:           times 24 db 0

align 16
stack16:        times STACK16 db 0
stack16_top:

; 5 pages + 4K slack for alignment (PD, PT0, LFB PT, MMIO PT, bounce)
align 16
area:           times 6 * 4096 db 0
