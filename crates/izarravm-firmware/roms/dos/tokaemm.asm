; TOKAEMM.SYS — SP-4b M0 memory manager (bespoke, runs the system in V86).
;
; Task 3 increment B: the driver's INIT builds a load-relative PM/paging +
; ring-0 monitor environment in its OWN resident memory, then instead of a
; signal stub it IRETDs the *running kernel* into V86 at the SYSINIT return
; point (the EXECRH post-INIT code), so real FreeDOS keeps booting virtualized
; under the monitor. The monitor emulates the V86 sensitive instructions
; (CLI/STI/PUSHF/POPF/INT/IRET via a virtual IF) and reflects the timer (IRQ0
; -> INT 08h) and keyboard (IRQ1 -> INT 09h) hardware interrupts to the guest's
; real-mode IVT, holding them pending while VIF is clear (real DOS brackets
; IRQ-sensitive code with CLI/STI).
;
; Addressing model (all load-segment relative):
;   * PM CODE selector 0x08  base = CS<<4    (monitor runs at driver offsets)
;   * PM DATA selector 0x10  base = 0 flat   (builds page tables at linear addrs)
;   * PM DATA selector 0x20  base = CS<<4    (monitor reaches its own VIF + the
;                                             saved kernel context, via FS)
; On a V86 fault the CPU nulls DS/ES/FS/GS; the monitor reads guest memory + the
; real-mode IVT through the null DS (base 0 == flat) and its own data through FS.
cpu 386
org 0

    dd 0xFFFFFFFF                 ; dh_next
    dw 0x8000                     ; dh_attr = char device
    dw strategy
    dw interrupt
    db 'TOKAEMM '

rh_ptr:  dd 0                     ; saved ES:BX (request header)
drv_seg: dw 0                     ; our load segment (CS)
base_lin: dd 0                    ; CS << 4
pd_lin:  dd 0                     ; page directory linear (page-aligned)

; Saved real-mode kernel context at INIT entry (for the return-to-V86 seam).
k_ss: dw 0
k_sp: dw 0
k_ds: dw 0
k_es: dw 0
k_fs: dw 0
k_gs: dw 0
k_cs: dw 0                        ; EXECRH far-return CS
k_ip: dw 0                        ; EXECRH far-return IP

vif: db 1                         ; virtual IF (guest's view; DOS boots with IF=1)
vip: db 0                         ; virtual interrupt pending: bit0=IRQ0, bit1=IRQ1

strategy:
    mov [cs:rh_ptr], bx
    mov [cs:rh_ptr+2], es
    retf

; ---- device interrupt entry. Real mode; ES:BX = request header (saved). ----
interrupt:
    cli
    ; Snapshot the real-mode kernel context FIRST (before anything perturbs it),
    ; via CS overrides so the kernel segment registers survive untouched.
    mov [cs:k_ss], ss
    mov [cs:k_sp], sp
    mov [cs:k_ds], ds
    mov [cs:k_es], es
    mov [cs:k_fs], fs
    mov [cs:k_gs], gs
    push bp
    mov bp, sp
    mov ax, [ss:bp+2]             ; EXECRH far-return IP (original [sp])
    mov [cs:k_ip], ax
    mov ax, [ss:bp+4]             ; EXECRH far-return CS
    mov [cs:k_cs], ax
    pop bp

    push cs
    pop ds                        ; DS = CS for our own data
    les bx, [rh_ptr]              ; request header -> ES:BX
    cmp byte [es:bx+2], 0         ; command 0 = INIT?
    je init
    ; Any non-INIT command (possibly reached in V86 later): just report done.
    mov word [es:bx+3], 0x0100    ; r_status = S_DONE
    sti
    retf

init:
    ; Report resident size FIRST, while ES:BX still points at the request header
    ; (the setup below clobbers BX). r_endaddr = drv_seg:resident_end covers the
    ; driver's code + tables, which stay resident under the monitor permanently.
    mov word [es:bx+14], resident_end
    mov word [es:bx+16], cs
    mov word [es:bx+3], 0x0100    ; r_status = S_DONE

    mov [drv_seg], cs
    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [base_lin], eax           ; base = CS<<4

    add eax, tables               ; pd_lin = page-align(base + tables)
    add eax, 0xFFF
    and eax, 0xFFFFF000
    mov [pd_lin], eax

    mov eax, [base_lin]           ; code selector (0x08) base = base
    mov [gdt + 0x08 + 2], ax
    shr eax, 16
    mov [gdt + 0x08 + 4], al
    mov [gdt + 0x08 + 7], ah

    mov eax, [base_lin]           ; FS data selector (0x20) base = base
    mov [gdt + 0x20 + 2], ax
    shr eax, 16
    mov [gdt + 0x20 + 4], al
    mov [gdt + 0x20 + 7], ah

    mov eax, [base_lin]           ; TSS descriptor (0x18) base = base + tss
    add eax, tss
    mov [gdt + 0x18 + 2], ax
    shr eax, 16
    mov [gdt + 0x18 + 4], al
    mov [gdt + 0x18 + 7], ah

    mov eax, [base_lin]           ; gdtr base = base + gdt
    add eax, gdt
    mov [gdtr + 2], eax

    mov eax, [base_lin]           ; idtr base = base + idt
    add eax, idt
    mov [idtr + 2], eax

    push es                       ; zero the TSS (ES still = request header seg here)
    push di
    push cs
    pop es                        ; ES = our segment so STOSW targets our TSS
    mov di, tss
    mov cx, 0x90 / 2
    xor ax, ax
    rep stosw
    pop di
    pop es
    mov eax, [base_lin]           ; ESP0 = monitor stack top in driver memory
    add eax, mon_stack_top
    mov [tss + 4], eax
    mov ebx, eax                  ; carry monitor ESP into PM (survives PT build)
    mov word  [tss + 8], 0x0010   ; SS0 = flat data selector
    mov word  [tss + 0x66], 0x0068 ; I/O-map base (all-zero bitmap = permissive)

    mov ebp, [pd_lin]             ; carry pd_lin + drv_seg into PM
    movzx esi, word [drv_seg]

    lgdt [gdtr]
    lidt [idtr]
    mov eax, cr0
    or eax, 1                     ; PE
    mov cr0, eax
    jmp dword 0x08:pm_init        ; code sel base = base -> linear base+pm_init

align 8
gdt:
    dq 0
    dq 0x00CF9B000000FFFF         ; [08] code, base patched
    dq 0x00CF93000000FFFF         ; [10] data, base 0 (flat)
    dq 0x0000890000000088         ; [18] TSS, base patched, limit 0x88
    dq 0x00CF93000000FFFF         ; [20] data, base patched (= base, driver data)
gdtr:
    dw 0x27                       ; 5 descriptors
    dd 0

; IDT (static gates; offsets are driver-relative, selector = PM code 0x08).
; Only the vectors that fire in M0 are present: 8 = IRQ0 timer, 9 = IRQ1
; keyboard, 13 = #GP (sensitive-instruction trap). base patched at runtime.
align 8
idt:
    times 8*8 db 0                ; 0..7
    dw irq8, 0x0008               ; 8  IRQ0 timer (offset-high = 0, driver < 64K)
    db 0, 0x8E
    dw 0
    dw irq9, 0x0008             ; 9  IRQ1 keyboard
    db 0, 0x8E
    dw 0
    times 3*8 db 0               ; 10..12
    dw monitor, 0x0008          ; 13 #GP -> sensitive-instruction monitor
    db 0, 0x8E
    dw 0
    times 18*8 db 0             ; 14..31
idt_end:
idtr:
    dw idt_end - idt - 1
    dd 0

bits 32
pm_init:                          ; EBP=pd_lin, ESI=drv_seg, EBX=monitor ESP0
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, ebx                  ; monitor ring-0 stack (driver-resident)
    lea eax, [ebp + 0x1000]       ; PD[0] -> PT (= PD + 0x1000)
    or eax, 7
    mov [ebp], eax
    lea edi, [ebp + 0x1000]       ; PT: 1024 identity entries (0..4 MiB)
    mov eax, 7
    mov ecx, 1024
.pt:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .pt
    mov cr3, ebp
    mov eax, cr0
    or eax, 0x80000000            ; paging on
    mov cr0, eax
    mov ax, 0x18
    ltr ax

    ; Return the running kernel into V86 at the EXECRH post-INIT code. The frame
    ; is the saved real-mode context; ESP = saved SP + 4 (past the far-return
    ; address the kernel's `call far` pushed). EXECRH then runs `sti; cld; pop
    ; ds; pop si; pop bp; ret 8` in V86 and DOS finishes booting virtualized.
    mov ax, 0x20
    mov fs, ax
    movzx eax, word [fs:k_gs]
    push eax
    movzx eax, word [fs:k_fs]
    push eax
    movzx eax, word [fs:k_ds]
    push eax
    movzx eax, word [fs:k_es]
    push eax
    movzx eax, word [fs:k_ss]
    push eax
    movzx eax, word [fs:k_sp]
    add eax, 4
    push eax
    push dword 0x00020202         ; EFLAGS: VM | IF(real) | bit1, IOPL 0
    movzx eax, word [fs:k_cs]
    push eax
    movzx eax, word [fs:k_ip]
    push eax
    iretd

; ============================================================================
; Ring-0 monitor. Entered from V86 through the IDT. deliver_exception has
; nulled DS/ES/FS/GS and switched to the driver-resident ring-0 stack; the
; guest's general registers are LIVE (the CPU saves none), so every handler
; brackets its work with pushad/popad. EBP points at the frame's saved EIP:
;   [ebp+0]=EIP [ebp+4]=CS [ebp+8]=EFLAGS [ebp+12]=V86 ESP [ebp+16]=V86 SS ...
; ============================================================================

; ---- #GP (vector 13): a sensitive instruction faulted. Has an error code. ----
monitor:
    pushad
    mov ax, 0x10                  ; flat 4 GiB DS to reach the guest's high stacks
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32 + 4]       ; skip pushad(32) + error code(4)
    movzx eax, word [ebp+4]       ; guest CS
    shl eax, 4
    movzx ebx, word [ebp]         ; guest IP
    add eax, ebx                  ; eax = linear addr of the faulting opcode
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
    mov al, dl                    ; unhandled sensitive instruction: signal its opcode
    jmp signal32
.cli:
    mov byte [fs:vif], 0
    inc word [ebp]
    jmp .done_gp
.sti:
    mov byte [fs:vif], 1
    inc word [ebp]
    call maybe_deliver            ; STI may release a pending IRQ
    jmp .done_gp
.pushf:
    mov ax, [ebp+8]               ; frame EFLAGS
    and ax, 0xFDFF                ; IF := VIF for the pushed image
    cmp byte [fs:vif], 0
    je .pf_store
    or ax, 0x0200
.pf_store:
    mov ebx, [ebp+16]            ; guest SS
    shl ebx, 4
    sub word [ebp+12], 2         ; guest SP -= 2
    movzx ecx, word [ebp+12]
    mov [ebx+ecx], ax
    inc word [ebp]               ; PUSHF is 1 byte
    jmp .done_gp
.popf:
    mov ebx, [ebp+16]           ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]    ; guest SP
    mov ax, [ebx+ecx]           ; popped flags
    add word [ebp+12], 2
    test ax, 0x0200             ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    or ax, 0x0200               ; frame keeps real IF = 1
    mov word [ebp+8], ax        ; update guest flags (VM in high word preserved)
    inc word [ebp]              ; POPF is 1 byte
    call maybe_deliver          ; POPF may re-enable interrupts
    jmp .done_gp
.intn:
    movzx ebx, byte [eax+1]      ; INT vector operand
    add word [ebp], 2            ; return IP = past INT n
    call reflect_vector
    jmp .done_gp
.iret_op:
    mov ebx, [ebp+16]           ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]    ; guest SP
    mov ax, [ebx+ecx]           ; pop IP
    mov word [ebp], ax
    add word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov ax, [ebx+ecx]           ; pop CS
    mov word [ebp+4], ax
    add word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov ax, [ebx+ecx]           ; pop FLAGS
    add word [ebp+12], 2
    test ax, 0x0200            ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    or ax, 0x0200             ; frame keeps real IF = 1
    mov word [ebp+8], ax
    call maybe_deliver         ; IRET may re-enable interrupts
    jmp .done_gp
.done_gp:
    popad
    add esp, 4                   ; discard the #GP error code
    iretd

; ---- IRQ0 timer (vector 8) / IRQ1 keyboard (vector 9). No error code. ----
irq8:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    cmp byte [fs:vif], 0
    jne .go
    or byte [fs:vip], 1          ; VIF clear: coalesce pending, but EOI now so the
    mov al, 0x20                 ; PIC keeps delivering (deliver on the next STI/POPF)
    out 0x20, al
    popad
    iretd
.go:
    mov ebx, 8
    call reflect_vector
    popad
    iretd
irq9:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    cmp byte [fs:vif], 0
    jne .go
    or byte [fs:vip], 2          ; coalesce pending + EOI now (see irq8)
    mov al, 0x20
    out 0x20, al
    popad
    iretd
.go:
    mov ebx, 9
    call reflect_vector
    popad
    iretd

; Reflect an interrupt into the guest's real-mode IVT handler.
;   in: EBX = vector, EBP = &frame.eip, FS = driver data.  clobbers eax,ecx,edx,edi
reflect_vector:
    mov edx, [ebp+16]            ; guest SS
    shl edx, 4                   ; edx = guest stack base (linear)
    mov ax, [ebp+8]             ; guest flags, IF := VIF
    and ax, 0xFDFF
    cmp byte [fs:vif], 0
    je .rf
    or ax, 0x0200
.rf:
    sub word [ebp+12], 2         ; push FLAGS
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov ax, [ebp+4]             ; push CS
    sub word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov ax, [ebp]              ; push return IP
    sub word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov edi, ebx
    shl edi, 2                  ; vec*4 -> IVT entry (via null DS, base 0)
    movzx eax, word [edi]
    mov word [ebp], ax          ; guest IP = IVT[vec] offset
    movzx eax, word [edi+2]
    mov word [ebp+4], ax        ; guest CS = IVT[vec] segment
    mov byte [fs:vif], 0        ; entering the ISR clears VIF
    ret

; If VIF is set and an IRQ is pending, deliver the highest-priority one.
;   in: EBP = &frame.eip, FS = driver data.  clobbers eax,ebx,ecx,edx,edi
maybe_deliver:
    cmp byte [fs:vif], 0
    je .none
    movzx ebx, byte [fs:vip]
    test bl, bl
    jz .none
    test bl, 1
    jz .try9
    and byte [fs:vip], 0xFE
    mov ebx, 8
    jmp reflect_vector           ; tail: ret returns to maybe_deliver's caller
.try9:
    and byte [fs:vip], 0xFD
    mov ebx, 9
    jmp reflect_vector
.none:
    ret

; Debug failure signal via the unit-tester exit port (AL = code).
signal32:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

align 16
tss:
    times 0x90 db 0

align 4
mon_stack:
    times 0x400 db 0
mon_stack_top:

align 4096
tables:
    times 0x3000 db 0
resident_end:
