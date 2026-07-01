; TOKAEMM.SYS — SP-4b M0 memory manager (bespoke, runs the system in V86).
;
; Task 3 increment A: the driver's INIT, loaded by DOS at an arbitrary segment,
; builds a page-aligned identity-paging + protected-mode environment in its OWN
; resident memory (all addressing derived from CS at runtime), enters V86, and a
; V86 stub signals 0xA5 through the unit-tester port. Proves V86 entry from the
; real SYSINIT context with dynamic, load-segment-relative addressing. It does
; NOT yet install the monitor or return to DOS (Task 3 increment B).
;
; Dynamic-addressing trick: the PM CODE selector (0x08) is based at the driver
; (base = CS<<4) so PM code runs at driver-relative offsets; the DATA selector
; (0x10) is flat (base 0) so it builds page tables at absolute linear addresses.
; pd_lin (page directory) and drv_seg are carried into PM in EBP/ESI.
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

strategy:
    mov [cs:rh_ptr], bx
    mov [cs:rh_ptr+2], es
    retf

; ---- INIT (interrupt entry). Real mode; ES:BX = request header (saved). ----
interrupt:
    cli
    push cs
    pop ds                        ; DS = CS

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

    mov eax, [base_lin]           ; TSS descriptor (0x18) base = base + tss
    add eax, tss
    mov [gdt + 0x18 + 2], ax
    shr eax, 16
    mov [gdt + 0x18 + 4], al
    mov [gdt + 0x18 + 7], ah

    mov eax, [base_lin]           ; gdtr base = base + gdt
    add eax, gdt
    mov [gdtr + 2], eax

    push di                       ; zero the TSS, then ESP0/SS0/io-map
    mov di, tss
    mov cx, 0x90 / 2
    xor ax, ax
    rep stosw
    pop di
    mov dword [tss + 4], 0x7000
    mov word  [tss + 8], 0x0010
    mov word  [tss + 0x66], 0x0068

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
gdtr:
    dw 0x1F
    dd 0
idtr:
    dw 0                          ; empty IDT (no faults in increment A)
    dd 0

bits 32
pm_init:                          ; EBP = pd_lin, ESI = drv_seg
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x7000
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
    push dword 0                  ; GS
    push dword 0                  ; FS
    push dword 0                  ; DS
    push dword 0                  ; ES
    push dword 0                  ; SS (stub uses no stack)
    push dword 0x1000             ; ESP (unused)
    push dword 0x00020002         ; EFLAGS VM | bit1, IOPL 0
    push esi                      ; CS = drv_seg (V86 base = drv_seg<<4 = base)
    push dword v86_stub           ; EIP
    iretd

bits 16
v86_stub:
    mov al, 12
    out 0xE4, al
    mov al, 0xA5
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

align 16
tss:
    times 0x90 db 0

align 4096
tables:
    times 0x3000 db 0
