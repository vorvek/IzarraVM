; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; pmirq5.com -- protected-mode SB16 DMA IRQ5 smoke test.
; Assemble: nasm -f bin pmirq5.asm -o pmirq5.com
cpu 386
org 0x100

%define OK          0xA5
%define FAIL_RESET  0xE1
%define FAIL_IRQ    0xE2

%define PIC_CMD     0x20
%define PIC_DATA    0x21
%define DMA1_MODE   0x0B
%define DMA1_ADDR   0x02
%define DMA1_COUNT  0x03
%define DMA1_PAGE   0x83
%define DMA1_MASK   0x0A
%define DSP_RESET   0x226
%define DSP_READ    0x22A
%define DSP_WRITE   0x22C
%define DSP_STATUS  0x22E

bits 16
start:
    cli
    cld
    mov ax, cs
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, rm_stack

    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [lin_base], eax

    call patch_descriptor_bases

    mov eax, [lin_base]
    add eax, gdt
    mov [gdtr + 2], eax

    mov eax, [lin_base]
    add eax, idt
    mov [idtr + 2], eax

    ; IDT[0x0D] = 32-bit interrupt gate, selector 0x08, offset irq5_handler.
    mov di, idt + 0x0D * 8
    mov eax, irq5_handler
    mov [di], ax
    mov word [di + 2], 0x08
    mov byte [di + 4], 0
    mov byte [di + 5], 0x8E
    shr eax, 16
    mov [di + 6], ax

    lgdt [gdtr]
    lidt [idtr]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp dword 0x08:pm_entry

patch_descriptor_bases:
    mov eax, [lin_base]
    mov di, gdt + 0x08
    call patch_base
    mov eax, [lin_base]
    mov di, gdt + 0x10
    call patch_base
    ret

patch_base:
    mov [di + 2], ax
    shr eax, 16
    mov [di + 4], al
    mov [di + 7], ah
    ret

bits 32
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, pm_stack_top

    call init_pic
    call fill_dma_buffer
    call reset_dsp
    test al, al
    jz .reset_ok
    mov al, FAIL_RESET
    jmp signal
.reset_ok:
    call program_dma
    call arm_dsp

    mov byte [irq_seen], 0
    in al, PIC_DATA
    and al, 0xDF
    out PIC_DATA, al

    sti
    mov ecx, 200000
.wait:
    cmp byte [irq_seen], 0
    jne .ok
    loop .wait
    mov al, FAIL_IRQ
    jmp signal
.ok:
    mov al, OK
    jmp signal

init_pic:
    mov al, 0x11
    out PIC_CMD, al
    mov al, 0x08
    out PIC_DATA, al
    mov al, 0x04
    out PIC_DATA, al
    mov al, 0x01
    out PIC_DATA, al
    mov al, 0xFF
    out PIC_DATA, al
    ret

fill_dma_buffer:
    mov edi, dma_buf
    mov al, 0x80
    mov ecx, 32
.fill:
    stosb
    add al, 8
    loop .fill
    ret

reset_dsp:
    mov dx, DSP_RESET
    mov al, 1
    out dx, al
    mov ecx, 0x4000
.d1:
    loop .d1
    xor al, al
    out dx, al
    mov ecx, 16
.poll:
    push ecx
    mov ecx, 0x4000
.d2:
    loop .d2
    pop ecx
    mov dx, DSP_STATUS
    in al, dx
    test al, 0x80
    jnz .ready
    loop .poll
    mov al, 1
    ret
.ready:
    mov dx, DSP_READ
    in al, dx
    cmp al, 0xAA
    jne .bad
    xor al, al
    ret
.bad:
    mov al, 1
    ret

program_dma:
    mov eax, [lin_base]
    add eax, dma_buf
    mov ebx, eax

    mov dx, DMA1_MODE
    mov al, 0x49
    out dx, al

    mov dx, DMA1_ADDR
    mov ax, bx
    out dx, al
    mov al, ah
    out dx, al

    mov dx, DMA1_COUNT
    mov ax, 31
    out dx, al
    mov al, ah
    out dx, al

    mov dx, DMA1_PAGE
    mov eax, ebx
    shr eax, 16
    out dx, al

    mov dx, DMA1_MASK
    mov al, 0x01
    out dx, al
    ret

arm_dsp:
    mov dx, DSP_WRITE
    mov al, 0x41
    out dx, al
    mov al, 0x2B
    out dx, al
    mov al, 0x11
    out dx, al
    mov al, 0x14
    out dx, al
    mov al, 31
    out dx, al
    xor al, al
    out dx, al
    ret

irq5_handler:
    push eax
    push edx
    mov dx, DSP_STATUS
    in al, dx
    mov al, 0x20
    out PIC_CMD, al
    mov byte [irq_seen], 1
    pop edx
    pop eax
    iretd

signal:
    cli
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.halt:
    hlt
    jmp .halt

align 4
lin_base: dd 0
irq_seen: db 0

gdtr:
    dw gdt_end - gdt - 1
    dd gdt
idtr:
    dw idt_end - idt - 1
    dd 0

align 8
gdt:
    dq 0
    ; 0x08: 32-bit code, base patched to the .COM linear base.
    dw 0xFFFF, 0
    db 0, 0x9A, 0x40, 0
    ; 0x10: 32-bit data/stack, base patched to the .COM linear base.
    dw 0xFFFF, 0
    db 0, 0x92, 0x40, 0
gdt_end:

align 8
idt:
    times 256 * 8 db 0
idt_end:

align 16
dma_buf:
    times 32 db 0x80

times 128 db 0
rm_stack:
times 512 db 0
pm_stack_top:
