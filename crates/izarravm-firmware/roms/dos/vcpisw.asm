; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; vcpisw.com: TOKAEMM VCPI mode-switch fixture, a minimal real VCPI
; CLIENT. Runs in V86 under a bare DEVICE=C:\DOS\TOKAEMM.SYS and walks the
; whole client lifecycle the extenders use (the JEMM VCPI.ASM:359-402 flow):
;
;   1. DE00 presence, DE03 free-count baseline (V86).
;   2. DE01: hand the server a 4K-aligned page-table buffer + three GDT
;      slots; keep the returned PM entry point.
;   3. Build the client context: PD (PD[0] -> the DE01-filled PT0), a GDT
;      with 16-bit code/data mirrors of this .COM's segment + a TSS
;      descriptor + the three server slots, a zeroed TSS, and the DE0C
;      switch structure (CR3/GDTR/IDTR/LDTR=0/TR/CS:EIP).
;   4. INT 67h AX=DE0Ch with interrupts off: the server switches to OUR
;      paging + tables and far-jumps to pm_landing at CPL 0 (16-bit PM).
;   5. In PM: verify PE=1, registers carried across, then far-call the
;      server's USE32 PM entry: DE03 (must equal the V86 baseline), DE04
;      alloc (4K-aligned, above 1MB), DE05 free (AH=0).
;   6. Far-call the entry with AX=DE0Ch and the 9-dword stack frame: the
;      server rebuilds ITS tables and IRETDs us back to V86 at v86_landing.
;   7. Back in V86: verify the marker register survived, then exit 0xA5.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
; Any fault inside PM (bad switch, bad entry) never reaches sig: the run
; times out or the machine crashes -- both fail the e2e loudly.
;
; Build: nasm -f bin vcpisw.asm -o vcpisw.com
cpu 386
XMS_TEST_KB equ 64            ; 64 KB = 16 x 4 KB pages
org 0x100
%define OK 0xA5

start:
    cld
    ; ---- runtime linear layout: page-align the table area ----
    ; linear base of this .COM image; the V86 return anchors (segment + the
    ; stack the way-back IRETD restores)
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
    ; 16-bit offsets of those pages within our segment
    sub ebx, eax
    mov [pd_off], bx
    add bx, 0x1000
    mov [pt_off], bx

    ; ---- 1. presence + baseline ----
    mov ax, 0xDE00
    int 0x67
    or ah, ah
    jnz f_pres
    cmp bx, 0x0100
    jne f_pres
    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_pres
    mov [free_v86], edx

    ; Keep a 64 KB XMS block locked throughout the VCPI mode switch. XMS and
    ; VCPI now share ONE pool (see tokaemm.asm), so the allocation MUST show up
    ; in the VCPI free count -- it used to assert the count was unchanged, which
    ; was the disjoint-pool contract. `free_v86` is then rebased to the
    ; with-block-held count, which is the baseline every later DE03 compares to.
    ; A page DE04 hands out still must not overlap the block: one pool, but
    ; never the same page twice.
    mov ax, 0x4300
    int 0x2F
    cmp al, 0x80
    jne f_xms
    mov ax, 0x4310
    int 0x2F
    mov [xms_entry], bx
    mov [xms_entry+2], es
    mov ah, 0x09
    mov dx, XMS_TEST_KB
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov [xms_handle], dx
    mov ah, 0x0C
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov [xms_base], bx
    mov [xms_base+2], dx
    mov eax, XMS_TEST_KB
    shl eax, 10
    add eax, [xms_base]
    mov [xms_end], eax
    mov ax, 0xDE03
    int 0x67
    mov ecx, [free_v86]
    sub ecx, XMS_TEST_KB / 4      ; 4 KB pages the block consumed
    cmp edx, ecx
    jne f_xms
    mov [free_v86], edx           ; rebase: baseline while the block is held
    push cs
    pop es                         ; DE01's page-table buffer lives in this COM

    ; ---- 2. DE01: page-table buffer at ES:DI = pt page, GDT trio at gdt+0x20
    mov di, [pt_off]
    mov si, gdt + 0x20
    mov ax, 0xDE01
    int 0x67
    or ah, ah
    jnz f_if
    mov [entry_off], ebx          ; PM entry offset in server code segment

    ; ---- 3. client context ----
    ; PD[0] = PT0 | present/rw/user; rest of the PD stays zero
    push es
    mov ax, ds
    mov es, ax
    mov di, [pd_off]              ; zero the PD page first
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
    ; GDTR/IDTR pseudo-descriptors + their first-MB linear pointers
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
    mov ebp, 0x1BADB002           ; marker: must survive BOTH switches
    cli
    mov esi, [lin_base]
    add esi, swst                 ; ESI = switch-structure linear
    mov ax, 0xDE0C
    int 0x67
    ; never reached: DE0C transfers to pm_landing or the run times out
f_hang:
    mov al, 0xEF
    jmp sig

; ---- 5/6. protected mode, CPL 0, 16-bit code segment, IF=0 ----
pm_landing:
    mov ax, 0x10                  ; our PM data/stack mirror
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, pmstack_top          ; FULL ESP load: the server left its own
                                  ; ring-0 ESP here (high word nonzero), and
                                  ; the DE0C-back frame is read with 32-bit
                                  ; addressing -- a bare `mov sp` would leave
                                  ; garbage in the high word (the spec's
                                  ; SS:ESP-in-first-MB contract implies a
                                  ; clean full ESP)
    cmp ebp, 0x1BADB002           ; registers carried through the switch
    jne pm_f_regs
    smsw ax                       ; PE must be set
    test al, 1
    jz pm_f_pe

    ; far-call the server entry: DE03 must match the V86 baseline
    mov ax, 0xDE03
    call far dword [entry_ptr]
    or ah, ah
    jnz pm_f_de03
    cmp edx, [free_v86]
    jne pm_f_de03

    ; DE04 alloc: 4K-aligned, above 1 MB; DE05 free: clean status
    mov ax, 0xDE04
    call far dword [entry_ptr]
    or ah, ah
    jnz pm_f_alloc
    test edx, 0xFFF
    jnz pm_f_alloc
    cmp edx, 0x100000
    jb pm_f_alloc
    cmp edx, [xms_base]
    jb .outside_xms
    cmp edx, [xms_end]
    jb pm_f_alloc
.outside_xms:
    mov ax, 0xDE05
    call far dword [entry_ptr]
    or ah, ah
    jnz pm_f_free

    ; ---- DE0C back to V86: push the 9-dword frame, DS = the flat server
    ; selector (spec: the selector mapping the whole linear space, CS+8 of
    ; the server trio = our GDT slot 0x28), then far-call the entry.
    mov ebp, 0x0D06F00D           ; marker across the return switch
    xor eax, eax                  ; the V86 values we push are the REAL-MODE
    mov ax, [rm_seg]              ; segment saved at start (CS here is
                                  ; selector 0x08, not a V86 segment)
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
    push eax                      ; EIP
    mov ax, 0x28                  ; DS = flat (server slot CS+8)
    mov ds, ax
    mov ax, 0xDE0C
    call far dword [cs:entry_ptr]
    ; never returns
pm_f_regs:
    mov al, 0xE8
    jmp pm_sig
pm_f_pe:
    mov al, 0xE9
    jmp pm_sig
pm_f_de03:
    mov al, 0xEA
    jmp pm_sig
pm_f_alloc:
    mov al, 0xEB
    jmp pm_sig
pm_f_free:
    mov al, 0xEC
    jmp pm_sig
pm_sig:                           ; signal from PM: the exit ports are wide
    mov ah, al                    ; open (I/O bitmap governs V86 only; PM
    mov al, 12                    ; CPL 0 has IOPL rights)
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

; ---- 7. back in V86 ----
v86_landing:
    mov ax, cs                    ; restore our own data addressing
    mov ds, ax
    cmp ebp, 0x0D06F00D           ; registers carried through the return
    jne f_ret
    mov ax, 0xDE03                ; pool balanced after the PM alloc/free
    int 0x67
    or ah, ah
    jnz f_bal
    cmp edx, [free_v86]
    jne f_bal
    mov ah, 0x0D                 ; unlock and release the 64 KB XMS block
    mov dx, [xms_handle]
    call far [xms_entry]
    or ax, ax
    jz f_xms
    mov ah, 0x0A
    mov dx, [xms_handle]
    call far [xms_entry]
    or ax, ax
    jz f_xms
    sti                           ; virtualized STI: must trap cleanly
    mov al, OK
    jmp sig

f_pres:   mov al, 0xE1
          jmp sig
f_if:     mov al, 0xE2
          jmp sig
f_ret:    mov al, 0xE5
          jmp sig
f_bal:    mov al, 0xE6
          jmp sig
f_xms:    mov al, 0xE7

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
free_v86:  dd 0
xms_entry: dd 0
xms_base:  dd 0
xms_end:   dd 0
xms_handle: dw 0
xms_largest: dw 0
entry_ptr:                       ; fword: the server PM entry (USE32 far ptr)
entry_off: dd 0
entry_sel: dw 0x20               ; server code = first of the DE01 trio
pd_off:    dw 0
pt_off:    dw 0
rm_seg:    dw 0                  ; set at start: our real-mode segment
rm_sp:     dw 0                  ; the V86 SP to restore on the way back

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
