bits 16
org 0

%define ROM_BASE 0x000f0000
%define VGA_TEXT_LINEAR 0x000b8000
%define VGA_TEXT 0xb800
%define BIOS_CURSOR 0x0500

start:
    cli
    cld

    mov ax, 0
    mov ds, ax
    mov ss, ax
    mov sp, 0x9000
    mov ax, int10_handler
    mov [0x0040], ax
    mov ax, 0xf000
    mov [0x0042], ax
    mov ax, 0
    mov [BIOS_CURSOR], ax

    mov ax, cs
    mov ds, ax
    mov ax, 0x0003
    int 0x10

    mov si, msg_int10
    call puts_int10

    mov ax, VGA_TEXT
    mov es, ax
    mov di, 160
    mov si, msg_direct
    call puts_direct16

    lgdt [gdt_descriptor]
    lidt [idt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp dword 0x0008:(ROM_BASE + protected_entry)

puts_int10:
    lodsb
    test al, al
    jz .done
    mov ah, 0x0e
    int 0x10
    jmp puts_int10
.done:
    ret

puts_direct16:
    lodsb
    test al, al
    jz .done
    mov ah, 0x0a
    stosw
    jmp puts_direct16
.done:
    ret

int10_handler:
    cmp ah, 0x00
    je .set_mode
    cmp ah, 0x0e
    je .teletype
    iret

.set_mode:
    push ax
    push ds
    mov ax, 0
    mov ds, ax
    mov word [BIOS_CURSOR], 0
    pop ds
    pop ax
    iret

.teletype:
    push di
    push ds
    push es
    push ax
    mov ax, 0
    mov ds, ax
    mov di, [BIOS_CURSOR]
    mov ax, VGA_TEXT
    mov es, ax
    pop ax
    mov ah, 0x07
    stosw
    mov [BIOS_CURSOR], di
    pop es
    pop ds
    pop di
    iret

align 8, db 0
gdt_start:
    dq 0x0000000000000000
    dq 0x00cf9a000000ffff
    dq 0x00cf92000000ffff
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd ROM_BASE + gdt_start

idt_start:
    times 14 dq 0
    dw page_fault_handler
    dw 0x0008
    dw 0x8e00
    dw 0x000f
    times (256 - 15) dq 0
idt_end:

idt_descriptor:
    dw idt_end - idt_start - 1
    dd ROM_BASE + idt_start

msg_int10 db 'RESET VECTOR + BIOS INT10 PASS', 0
msg_direct db 'B8000 DIRECT TEXT PASS', 0

bits 32
protected_entry:
    mov ax, 0x0010
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x00090000

    mov esi, ROM_BASE + msg_protected
    mov edi, VGA_TEXT_LINEAR + (160 * 2)
    mov ah, 0x0b
    call puts_direct32

    mov edi, 0x00001000
    mov eax, 0x00002003
    stosd
    mov eax, 0x00003003
    stosd

    mov edi, 0x00002000
    mov eax, 0x00000003
    mov ecx, 1024
.fill_identity:
    stosd
    add eax, 0x00001000
    loop .fill_identity

    mov edi, 0x00003000
    mov eax, 0x000b8003
    stosd

    mov eax, 0x00001000
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    jmp short .paging_flushed

.paging_flushed:
    mov esi, ROM_BASE + msg_paging
    mov edi, 0x00400000 + (160 * 3)
    mov ah, 0x0c
    call puts_direct32

    mov eax, [0x00800000]

    mov esi, ROM_BASE + msg_page_fault
    mov edi, VGA_TEXT_LINEAR + (160 * 4)
    mov ah, 0x0d
    call puts_direct32

    hlt

puts_direct32:
    lodsb
    test al, al
    jz .done
    stosw
    jmp puts_direct32
.done:
    ret

page_fault_handler:
    pop eax
    pop eax
    add eax, 5
    push eax
    iretd

msg_protected db 'PROTECTED MODE FLAT SEGMENTS PASS', 0
msg_paging db 'PAGING + B8000 ALIAS PASS', 0
msg_page_fault db 'RING0 PAGE FAULT HANDLER PASS', 0

bits 16
times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp 0xf000:0x0000
times 0x10000 - ($ - $$) db 0
