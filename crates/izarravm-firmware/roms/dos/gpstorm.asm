; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; gpstorm.com: TOKAEMM ring-0 #GP diagnostic fixture. Forces the monitor's
; own IRETD to fault at ring 0 and asserts the monitor REPORTS instead of
; storming.
;
; The eXoDOS stage-1 triage (2026-08-17, finding G1) caught the monitor in a
; self-sustaining #GP storm: its IRETD back to V86 faulted at ring 0, the
; old vec13_entry LAYER 1 classified the resulting error-code frame by
; error-code VALUE, and every later iteration either reflected through a
; frame with no V86 SS:SP or IRETD'd a three-dword pop off a four-dword
; frame. ESP then walked through the driver's own structures until the
; fault delivery itself died (baroll, SpacPlum, MontyNrm).
;
; This client reproduces the storm's iteration 0 through a sanctioned
; guest-reachable door, with no dependence on any CPU defect: the VCPI
; PM->V86 switch (DE0C from protected mode) takes the return frame from the
; CLIENT's stack and IRETDs straight off it. A frame whose EIP dword is
; above 0xFFFF makes that IRETD raise #GP(0) at ring 0 inside the monitor
; (386 PRM STACK-RETURN-TO-V86: "instruction pointer not within code
; segment limits"), before any V86 state commits. The steps:
;
;   1. DE00 presence.
;   2. DE01: page-table buffer + the three server GDT slots.
;   3. Client context: PD, GDT (code16/data16/TSS + server trio), the DE0C
;      switch structure -- the vcpisw.asm scaffolding, minus everything not
;      needed to reach protected mode once.
;   4. INT 67h AX=DE0Ch: the server far-jumps to pm_landing at CPL 0.
;   5. Far-call the server PM entry with AX=DE0Ch and a 9-dword frame whose
;      EIP slot is 0x00010000 | v86_landing. The monitor's IRETD faults.
;
; PASS: the monitor signals its ring-0 #GP diagnostic (exit code 0xD3).
; The Rust test asserts that exit code; this fixture itself can only signal
; FAILURE paths:
;   0xE1 no VCPI / bad version     0xE2 DE01 refused
;   0xE5 v86_landing reached: the poisoned IRETD returned to V86 anyway,
;        so the trigger this fixture exists for never fired.
;
; Build: nasm -f bin gpstorm.asm -o gpstorm.com
cpu 386
org 0x100

start:
    cld
    mov [rm_seg], cs
    mov [rm_sp], sp
    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [lin_base], eax
    ; aligned area start (linear + our offset, rounded up to 4K)
    lea ebx, [eax + area]
    add ebx, 0xFFF
    and ebx, 0xFFFFF000
    mov [pd_phys], ebx            ; PD page
    lea ecx, [ebx + 0x1000]
    mov [pt_phys], ecx            ; PT0 page (the DE01 buffer)
    sub ebx, eax
    mov [pd_off], bx
    add bx, 0x1000
    mov [pt_off], bx

    ; ---- 1. presence ----
    mov ax, 0xDE00
    int 0x67
    or ah, ah
    jnz f_pres
    cmp bx, 0x0100
    jne f_pres
    push cs
    pop es                        ; DE01's page-table buffer lives in this COM

    ; ---- 2. DE01: page-table buffer at ES:DI, GDT trio at DS:SI ----
    mov di, [pt_off]
    mov si, gdt + 0x20
    mov ax, 0xDE01
    int 0x67
    or ah, ah
    jnz f_if
    mov [entry_off], ebx          ; PM entry offset in server code segment

    ; ---- 3. client context (the vcpisw.asm shapes) ----
    ; PD[0] = PT0 | present/rw/user; rest of the PD stays zero
    push es
    mov ax, ds
    mov es, ax
    mov di, [pd_off]
    mov cx, 0x1000/2
    xor ax, ax
    rep stosw
    pop es
    mov bx, [pd_off]
    mov eax, [pt_phys]
    or eax, 7
    mov [bx], eax
    ; GDT slot 1 (0x08): 16-bit code, base = lin_base, limit 0xFFFF
    mov eax, [lin_base]
    mov word [gdt+0x08], 0xFFFF
    mov [gdt+0x08+2], ax
    shr eax, 16
    mov [gdt+0x08+4], al
    mov byte [gdt+0x08+5], 0x9B
    mov byte [gdt+0x08+6], 0      ; D=0 (16-bit), G=0
    mov [gdt+0x08+7], ah
    ; GDT slot 2 (0x10): 16-bit data mirror
    mov eax, [lin_base]
    mov word [gdt+0x10], 0xFFFF
    mov [gdt+0x10+2], ax
    shr eax, 16
    mov [gdt+0x10+4], al
    mov byte [gdt+0x10+5], 0x93
    mov byte [gdt+0x10+6], 0
    mov [gdt+0x10+7], ah
    ; GDT slot 3 (0x18): the TSS
    mov eax, [lin_base]
    add eax, tss
    mov word [gdt+0x18], 0x67
    mov [gdt+0x18+2], ax
    shr eax, 16
    mov [gdt+0x18+4], al
    mov byte [gdt+0x18+5], 0x89   ; available 32-bit TSS
    mov byte [gdt+0x18+6], 0
    mov [gdt+0x18+7], ah
    ; (slots 4-6 at gdt+0x20 were filled by DE01)
    mov word [gdtr_pd], 0x37      ; 7 slots - 1
    mov eax, [lin_base]
    add eax, gdt
    mov [gdtr_pd+2], eax
    mov word [idtr_pd], 0         ; no IDT: IF stays 0 for the whole PM leg
    mov dword [idtr_pd+2], 0
    ; the DE0C switch structure
    mov eax, [pd_phys]
    mov [swst+0], eax             ; CR3
    mov eax, [lin_base]
    add eax, gdtr_pd
    mov [swst+4], eax             ; &GDTR value (first-MB linear)
    mov eax, [lin_base]
    add eax, idtr_pd
    mov [swst+8], eax             ; &IDTR value
    mov word [swst+0x0C], 0       ; LDTR = null
    mov word [swst+0x0E], 0x18    ; TR
    mov eax, pm_landing
    mov [swst+0x10], eax          ; entry EIP (16-bit offset, zero-extended)
    mov word [swst+0x14], 0x08    ; entry CS = client 16-bit code

    ; ---- 4. switch to protected mode ----
    cli
    mov esi, [lin_base]
    add esi, swst                 ; ESI = switch-structure linear
    mov ax, 0xDE0C
    int 0x67
    ; never reached: DE0C transfers to pm_landing or the run times out

; ---- 5. protected mode, CPL 0, 16-bit code segment, IF=0 ----
pm_landing:
    mov ax, 0x10                  ; our PM data/stack mirror
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, pmstack_top          ; FULL ESP load (see vcpisw.asm)
    ; DE0C back to V86, 9-dword frame -- but the EIP slot carries bit 16.
    ; The monitor rebuilds its own context and IRETDs off this frame; the
    ; out-of-limit EIP makes that IRETD #GP(0) at ring 0 under the server
    ; IDT. Everything else in the frame is well-formed, so the ONLY defect
    ; the monitor can be reporting is the one this fixture plants.
    xor eax, eax
    mov ax, [rm_seg]
    push eax                      ; GS
    push eax                      ; FS
    push eax                      ; DS
    push eax                      ; ES
    push eax                      ; SS
    movzx eax, word [rm_sp]
    push eax                      ; ESP (the V86 stack we left)
    push dword 0                  ; EFLAGS slot (server fills)
    movzx eax, word [rm_seg]
    push eax                      ; CS
    mov eax, v86_landing
    or eax, 0x00010000            ; runtime OR: `|` on a -f bin label is
    push eax                      ; rejected as non-scalar. EIP: out of V86
                                  ; code-segment limits
    mov ax, 0x28                  ; DS = flat (server slot CS+8)
    mov ds, ax
    mov ax, 0xDE0C
    call far dword [cs:entry_ptr]
    ; never returns: the monitor's IRETD faults at ring 0

; ---- only reachable if the poisoned IRETD succeeded ----
v86_landing:
    mov ax, cs
    mov ds, ax
    mov al, 0xE5
    jmp sig

f_pres:   mov al, 0xE1
          jmp sig
f_if:     mov al, 0xE2

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

; ---- state ----
align 4
lin_base:  dd 0
pd_phys:   dd 0
pt_phys:   dd 0
entry_ptr:                       ; fword: the server PM entry (USE32 far ptr)
entry_off: dd 0
entry_sel: dw 0x20               ; server code = first of the DE01 trio
pd_off:    dw 0
pt_off:    dw 0
rm_seg:    dw 0                  ; set at start: our real-mode segment
rm_sp:     dw 0

gdtr_pd:   times 6 db 0
idtr_pd:   times 6 db 0
swst:      times 0x16 db 0       ; the DE0C switch structure

align 8
gdt:       times 8*7 db 0        ; null + code16 + data16 + TSS + server trio
tss:       times 0x68 db 0

           times 128 db 0
pmstack_top:

; page-alignment slack + PD page + PT0 page
area:      times 0x1000 + 0x1000 + 0x1000 db 0
