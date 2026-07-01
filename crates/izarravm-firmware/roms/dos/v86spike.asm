; v86spike.asm — SP-4b M0 Task 2 standalone V86 spike.
;
; Increment 3: the monitor now also REFLECTS software interrupts to the V86
; real-mode IVT and unwinds IRET. The V86 stub installs an IVT[0x80] handler,
; does INT 0x80 (#GP -> monitor reflects -> handler runs in V86 -> IRET -> #GP ->
; monitor unwinds), and checks a marker the handler set. It also re-checks the
; CLI/STI/PUSHF virtual-IF path from increment 2. Signals 0xA5 on success.
;
; Split into a 512-byte boot sector (real-mode setup) + a stage2 (PM monitor +
; V86 stub) at 0x8000, because the monitor no longer fits in one sector.
;
; Physical map (identity-paged, < 1 MiB):
;   0x00800 VIF byte   0x00810 marker byte   0x01000 PD   0x02000 PT (1 MiB)
;   0x03000 GDT   0x04000 IDT   0x05000 TSS(+bitmap)   0x06000 V86 stack top
;   0x07000 ring-0 stack top (ESP0)   0x07c00 boot sector   0x08000 stage2
cpu 386

section .boot vstart=0x7c00
bits 16
start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7000

    mov di, 0x0800                          ; zero VIF/marker + tables
    mov cx, (0x6000 - 0x0800) / 2
    xor ax, ax
    rep stosw

    mov dword [0x1000], 0x2000 | 7          ; PD[0] -> PT
    mov di, 0x2000
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
    mov dword [0x301C], 0x00008900

    mov dword [0x5004], 0x7000              ; ESP0
    mov word  [0x5008], 0x0010              ; SS0
    mov word  [0x5066], 0x0068              ; I/O-map base

    mov word [0x4000 + 13*8],     monitor   ; IDT[13] #GP -> monitor
    mov word [0x4000 + 13*8 + 2], 0x0008
    mov byte [0x4000 + 13*8 + 4], 0
    mov byte [0x4000 + 13*8 + 5], 0x8E
    mov word [0x4000 + 13*8 + 6], 0

    lgdt [gdtr]
    lidt [idtr]
    mov eax, 0x1000
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80000001
    mov cr0, eax
    jmp dword 0x08:pm_entry

gdtr:
    dw 0x1F
    dd 0x3000
idtr:
    dw 0xFF
    dd 0x4000

    times 510 - ($ - $$) db 0
    dw 0xAA55

section .stage2 follows=.boot vstart=0x8000
bits 32
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x7000
    mov ax, 0x18
    ltr ax
    push dword 0                            ; GS
    push dword 0                            ; FS
    push dword 0                            ; DS
    push dword 0                            ; ES
    push dword 0                            ; SS = 0
    push dword 0x6000                       ; V86 ESP
    push dword 0x00020002                   ; EFLAGS VM | bit1, IOPL 0
    push dword 0                            ; CS = 0
    push dword v86_stub                     ; EIP (linear)
    iretd

; ---- ring-0 monitor. entry: [esp]=err,+4=EIP,+8=CS,+12=EFLAGS,+16=ESP,+20=SS ----
monitor:
    mov eax, [esp+8]                        ; V86 CS
    shl eax, 4
    movzx ebx, word [esp+4]
    add eax, ebx                            ; eax = linear of faulting opcode
    movzx edx, byte [eax]
    cmp dl, 0xFA
    je .cli
    cmp dl, 0xFB
    je .sti
    cmp dl, 0x9C
    je .pushf
    cmp dl, 0x9D
    je .popf
    cmp dl, 0xCD
    je .intn
    cmp dl, 0xCF
    je .iret_op
    mov al, 0xFD                            ; unhandled -> fail
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
    mov ax, [esp+12]
    and ax, 0xFDFF
    cmp byte [0x0800], 0
    je .pf_store
    or ax, 0x0200
.pf_store:
    sub word [esp+16], 2
    mov ebx, [esp+20]
    shl ebx, 4
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    inc word [esp+4]
    add esp, 4
    iretd
.popf:
    mov ebx, [esp+20]
    shl ebx, 4
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]
    add word [esp+16], 2
    test ax, 0x0200
    setnz cl
    mov [0x0800], cl
    and ax, 0xFDFF
    mov word [esp+12], ax
    inc word [esp+4]
    add esp, 4
    iretd
.intn:
    ; opcode 0xCD imm8 at eax; reflect INT n to real-mode IVT.
    movzx esi, byte [eax+1]                 ; n
    mov ebx, [esp+20]                        ; V86 SS
    shl ebx, 4
    ; push FLAGS (IF := VIF)
    mov ax, [esp+12]
    and ax, 0xFDFF
    cmp byte [0x0800], 0
    je .i_flags
    or ax, 0x0200
.i_flags:
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    ; push CS
    mov ax, [esp+8]
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    ; push return IP = EIP + 2
    mov ax, [esp+4]
    add ax, 2
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    ; load CS:IP from IVT[n] (linear n*4)
    mov edi, esi
    shl edi, 2
    movzx eax, word [edi]                    ; new IP
    mov word [esp+4], ax
    movzx eax, word [edi+2]                  ; new CS
    mov word [esp+8], ax
    mov byte [0x0800], 0                     ; interrupt gate clears VIF
    add esp, 4
    iretd
.iret_op:
    ; pop IP, CS, FLAGS from V86 stack back into the frame.
    mov ebx, [esp+20]
    shl ebx, 4
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]                        ; IP
    mov word [esp+4], ax
    add word [esp+16], 2
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]                        ; CS
    mov word [esp+8], ax
    add word [esp+16], 2
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]                        ; FLAGS
    add word [esp+16], 2
    test ax, 0x0200
    setnz cl
    mov [0x0800], cl
    and ax, 0xFDFF
    mov word [esp+12], ax
    add esp, 4
    iretd

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
    xor ax, ax
    mov ds, ax
    mov ss, ax
    mov sp, 0x6000
    ; increment-2 regression: CLI -> IF clear
    cli
    pushf
    pop ax
    test ax, 0x0200
    jnz .fail
    sti                                     ; IF set
    pushf
    pop ax
    test ax, 0x0200
    jz .fail
    ; increment-3: INT reflection + IRET unwind
    mov word [0x200], int80_handler         ; IVT[0x80] IP (CS base 0)
    mov word [0x202], 0                      ; IVT[0x80] CS
    mov byte [0x810], 0                      ; marker
    int 0x80
    cmp byte [0x810], 0xCC
    jne .fail
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

int80_handler:
    mov byte [0x810], 0xCC
    iret
