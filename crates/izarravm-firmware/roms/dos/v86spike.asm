; v86spike.asm — SP-4b M0 Task 2 standalone V86 spike.
;
; Increment 2: adds the ring-0 monitor. The V86 stub runs IOPL-sensitive
; instructions (CLI/STI/PUSHF/POPF) that #GP to a monitor via an IDT gate; the
; monitor emulates them against a virtual IF (VIF) byte and resumes. The stub
; self-checks VIF through PUSHF and signals 0xA5 on success, 0xFE on mismatch;
; the monitor signals 0xFD if it ever sees an opcode it doesn't handle.
;
; Physical map (identity-paged, < 1 MiB):
;   0x00800 VIF byte            0x01000 PD      0x02000 PT (identity, 1 MiB)
;   0x03000 GDT (null/08 code/10 data/18 TSS)  0x04000 IDT
;   0x05000 TSS (SS0:ESP0 + all-zero I/O bitmap)
;   0x06000 V86 stack top       0x07000 ring-0 (monitor) stack top (ESP0)
;   0x07c00 this boot code + monitor + V86 stub
cpu 386
bits 16
org 0x7c00

start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7000

    ; zero 0x0800..0x6000 (VIF + tables)
    mov di, 0x0800
    mov cx, (0x6000 - 0x0800) / 2
    xor ax, ax
    rep stosw

    mov dword [0x1000], 0x2000 | 7          ; PD[0] -> PT
    mov di, 0x2000                          ; PT: 256 identity entries
    mov eax, 0x0000_0007
    mov cx, 256
.fill_pt:
    mov [di], eax
    add eax, 0x1000
    add di, 4
    loop .fill_pt

    mov dword [0x3008], 0x0000FFFF          ; [08] ring0 code 32-bit
    mov dword [0x300C], 0x00CF9B00
    mov dword [0x3010], 0x0000FFFF          ; [10] ring0 data
    mov dword [0x3014], 0x00CF9300
    mov dword [0x3018], 0x50000088          ; [18] TSS base 0x5000 limit 0x88
    mov dword [0x301C], 0x00008900          ; access 0x89

    mov dword [0x5004], 0x7000              ; ESP0
    mov word  [0x5008], 0x0010              ; SS0
    mov word  [0x5066], 0x0068              ; I/O-map base (bitmap all zero)

    ; IDT[13] = #GP -> monitor (sel 0x08, 32-bit interrupt gate, DPL 0)
    mov word [0x4000 + 13*8],     monitor
    mov word [0x4000 + 13*8 + 2], 0x0008
    mov byte [0x4000 + 13*8 + 4], 0
    mov byte [0x4000 + 13*8 + 5], 0x8E
    mov word [0x4000 + 13*8 + 6], 0         ; monitor offset < 64K, hi = 0

    lgdt [gdtr]
    lidt [idtr]
    mov eax, 0x1000
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80000001
    mov cr0, eax
    jmp dword 0x08:pm_entry

bits 32
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x7000
    mov ax, 0x18
    ltr ax
    ; V86 IRET frame (push GS,FS,DS,ES,SS,ESP,EFLAGS,CS,EIP)
    push dword 0                            ; GS
    push dword 0                            ; FS
    push dword 0                            ; DS
    push dword 0                            ; ES
    push dword 0                            ; SS = 0
    push dword 0x6000                       ; ESP (V86 stack top)
    push dword 0x00020002                   ; EFLAGS: VM | bit1, IOPL 0
    push dword 0                            ; CS = 0
    push dword v86_stub                     ; EIP (linear; CS base 0)
    iretd

; ---- ring-0 monitor: emulate one IOPL-sensitive V86 instruction, then resume ----
; entry stack: [esp]=err, +4=EIP, +8=CS, +12=EFLAGS, +16=ESP, +20=SS, +24=ES ...
monitor:
    mov eax, [esp+8]                        ; V86 CS
    shl eax, 4
    movzx ebx, word [esp+4]                 ; V86 EIP (16-bit)
    add eax, ebx
    movzx edx, byte [eax]                   ; faulting opcode
    cmp dl, 0xFA
    je .cli
    cmp dl, 0xFB
    je .sti
    cmp dl, 0x9C
    je .pushf
    cmp dl, 0x9D
    je .popf
    mov al, 0xFD                            ; unhandled opcode -> fail
    jmp signal32
.cli:
    mov byte [0x0800], 0
    jmp .adv1
.sti:
    mov byte [0x0800], 1
.adv1:
    inc word [esp+4]
    add esp, 4
    iretd
.pushf:
    mov ax, [esp+12]                        ; V86 flags low16
    and ax, 0xFDFF                          ; clear IF
    cmp byte [0x0800], 0
    je .pf_store
    or ax, 0x0200                           ; IF := VIF
.pf_store:
    sub word [esp+16], 2                    ; V86 SP -= 2
    mov ebx, [esp+20]                       ; V86 SS
    shl ebx, 4
    movzx ecx, word [esp+16]
    add ebx, ecx
    mov [ebx], ax                           ; push flags image
    inc word [esp+4]
    add esp, 4
    iretd
.popf:
    mov ebx, [esp+20]                       ; V86 SS
    shl ebx, 4
    movzx ecx, word [esp+16]
    add ebx, ecx
    mov ax, [ebx]                           ; popped value
    add word [esp+16], 2                    ; V86 SP += 2
    test ax, 0x0200                         ; VIF := popped IF
    setnz cl
    mov [0x0800], cl
    and ax, 0xFDFF                          ; store flags with IF held by monitor
    mov word [esp+12], ax                   ; low16 only -> preserves VM
    inc word [esp+4]
    add esp, 4
    iretd

; signal exit code AL via the unit-tester port and stop (ring-0; I/O permitted at CPL0)
signal32:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

bits 16
v86_stub:
    cli                                     ; -> monitor VIF=0
    pushf                                   ; -> monitor pushes flags (IF=0)
    pop ax
    test ax, 0x0200
    jnz .fail
    sti                                     ; -> monitor VIF=1
    pushf                                   ; -> monitor pushes flags (IF=1)
    pop ax
    test ax, 0x0200
    jz .fail
    mov al, 0xA5
    jmp .signal
.fail:
    mov al, 0xFE
.signal:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

gdtr:
    dw 0x1F
    dd 0x3000
idtr:
    dw 0xFF
    dd 0x4000

    times 510 - ($ - $$) db 0
    dw 0xAA55
