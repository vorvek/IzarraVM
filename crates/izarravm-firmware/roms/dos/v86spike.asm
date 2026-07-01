; v86spike.asm — SP-4b M0 Task 2 standalone V86 spike.
;
; Increment 3b: a real hardware timer IRQ, delivered to the V86 task by the CPU
; (SP-4a hardware_interrupt -> deliver_exception V86 path), is reflected by the
; monitor to the guest's V86 INT 08h handler. The stub programs the PIC (master
; base 0x20, to dodge the error-code exception vectors) + PIT, installs a V86
; timer handler at IVT[8], STIs, and spins until a tick lands. Signals 0xA5.
;
; Also still proves increments 2-3: CLI/STI/PUSHF virtual-IF + INT n reflection.
;
; The V86 task runs with REAL IF=1 (so IRQs reach the monitor); VIF is the guest's
; view of IF. The monitor reflects vector 0x20 -> V86 INT 08h.
;
; Physical map (identity-paged, < 1 MiB):
;   0x00800 VIF  0x00810 marker  0x00820 tick counter  0x01000 PD  0x02000 PT
;   0x03000 GDT  0x04000 IDT  0x05000 TSS  0x06000 V86 stack  0x07000 ESP0
;   0x07c00 boot sector   0x08000 stage2
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

    mov di, 0x0800
    mov cx, (0x6000 - 0x0800) / 2
    xor ax, ax
    rep stosw

    mov dword [0x1000], 0x2000 | 7
    mov di, 0x2000
    mov eax, 0x0000_0007
    mov cx, 256
.fill_pt:
    mov [di], eax
    add eax, 0x1000
    add di, 4
    loop .fill_pt

    mov dword [0x3008], 0x0000FFFF
    mov dword [0x300C], 0x00CF9B00
    mov dword [0x3010], 0x0000FFFF
    mov dword [0x3014], 0x00CF9300
    mov dword [0x3018], 0x50000088
    mov dword [0x301C], 0x00008900

    mov dword [0x5004], 0x7000
    mov word  [0x5008], 0x0010
    mov word  [0x5066], 0x0068

    ; IDT[13] #GP -> monitor (sensitive instructions)
    mov word [0x4000 + 13*8],     monitor
    mov word [0x4000 + 13*8 + 2], 0x0008
    mov byte [0x4000 + 13*8 + 4], 0
    mov byte [0x4000 + 13*8 + 5], 0x8E
    mov word [0x4000 + 13*8 + 6], 0
    ; IDT[0x20] hardware IRQ0 -> monitor_irq
    mov word [0x4000 + 0x20*8],     monitor_irq
    mov word [0x4000 + 0x20*8 + 2], 0x0008
    mov byte [0x4000 + 0x20*8 + 4], 0
    mov byte [0x4000 + 0x20*8 + 5], 0x8E
    mov word [0x4000 + 0x20*8 + 6], 0

    ; PIC: remap master to base 0x20, unmask IRQ0 only
    mov al, 0x11
    out 0x20, al
    mov al, 0x20
    out 0x21, al
    mov al, 0x04
    out 0x21, al
    mov al, 0x01
    out 0x21, al
    mov al, 0xFE
    out 0x21, al
    ; PIT channel 0, mode 3, count 0x8000 (ticks spaced well apart)
    mov al, 0x36
    out 0x43, al
    mov al, 0x00
    out 0x40, al
    mov al, 0x80
    out 0x40, al

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
    dw 0x1FF                                 ; 64 vectors (covers IRQ0 at 0x20)
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
    push dword 0x00020202                   ; EFLAGS: VM | IF (real) | bit1, IOPL 0
    push dword 0                            ; CS = 0
    push dword v86_stub                     ; EIP
    iretd

; ---- #GP monitor (software sensitive instructions). frame: err@0,EIP@4,CS@8,... ----
monitor:
    mov eax, [esp+8]
    shl eax, 4
    movzx ebx, word [esp+4]
    add eax, ebx
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
    mov al, 0xFD
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
    movzx esi, byte [eax+1]
    mov ebx, [esp+20]
    shl ebx, 4
    mov ax, [esp+12]
    and ax, 0xFDFF
    cmp byte [0x0800], 0
    je .i_flags
    or ax, 0x0200
.i_flags:
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    mov ax, [esp+8]
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    mov ax, [esp+4]
    add ax, 2
    sub word [esp+16], 2
    movzx ecx, word [esp+16]
    mov [ebx+ecx], ax
    mov edi, esi
    shl edi, 2
    movzx eax, word [edi]
    mov word [esp+4], ax
    movzx eax, word [edi+2]
    mov word [esp+8], ax
    mov byte [0x0800], 0
    add esp, 4
    iretd
.iret_op:
    mov ebx, [esp+20]
    shl ebx, 4
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]
    mov word [esp+4], ax
    add word [esp+16], 2
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]
    mov word [esp+8], ax
    add word [esp+16], 2
    movzx ecx, word [esp+16]
    mov ax, [ebx+ecx]
    add word [esp+16], 2
    test ax, 0x0200
    setnz cl
    mov [0x0800], cl
    and ax, 0xFDFF
    mov word [esp+12], ax
    add esp, 4
    iretd

; ---- hardware IRQ0 monitor (vector 0x20, NO error code). Reflect to V86 INT 08h. ----
; frame: EIP@0, CS@4, EFLAGS@8, ESP@12, SS@16, ...
monitor_irq:
    mov ebx, [esp+16]                        ; V86 SS
    shl ebx, 4
    mov ax, [esp+8]                          ; flags, IF := VIF
    and ax, 0xFDFF
    cmp byte [0x0800], 0
    je .q_flags
    or ax, 0x0200
.q_flags:
    sub word [esp+12], 2
    movzx ecx, word [esp+12]
    mov [ebx+ecx], ax                        ; push FLAGS
    mov ax, [esp+4]                          ; CS
    sub word [esp+12], 2
    movzx ecx, word [esp+12]
    mov [ebx+ecx], ax
    mov ax, [esp]                            ; return IP = interrupted EIP (async)
    sub word [esp+12], 2
    movzx ecx, word [esp+12]
    mov [ebx+ecx], ax
    movzx eax, word [0x20]                   ; IVT[8] IP
    mov word [esp], ax
    movzx eax, word [0x22]                   ; IVT[8] CS
    mov word [esp+4], ax
    mov byte [0x0800], 0                     ; interrupt gate clears VIF
    iretd                                    ; no error code -> no add esp

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
    cli
    pushf
    pop ax
    test ax, 0x0200
    jnz .fail
    sti
    pushf
    pop ax
    test ax, 0x0200
    jz .fail
    ; INT reflection + IRET unwind
    mov word [0x200], int80_handler
    mov word [0x202], 0
    mov byte [0x810], 0
    int 0x80
    cmp byte [0x810], 0xCC
    jne .fail
    ; IRQ0 timer reflection
    mov word [0x20], v86_timer               ; IVT[8]
    mov word [0x22], 0
    mov byte [0x820], 0
    sti
.wait_tick:
    cmp byte [0x820], 0
    je .wait_tick
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

v86_timer:
    inc byte [0x820]
    push ax
    mov al, 0x20
    out 0x20, al                             ; EOI
    pop ax
    iret
