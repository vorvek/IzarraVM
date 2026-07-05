; gpreflct.com — TOKAEMM V86 #GP-reflection fixture (VCPI M4). A real-monitor
; contract check: a V86 program that hooks INT 0Dh and executes a privileged
; instruction the monitor does not emulate must receive its own fault
; reflection, with fault semantics (the stacked return IP points AT the
; faulting instruction), and must be able to skip-and-resume from its
; handler. This is exactly the DOS16M (DOS4G loader) startup dance: it runs
; an o32 LGDT during preparation under any V86 monitor and services the
; reflected #GP itself. The instruction used here is the literal shape from
; the Doom probe: 66 0F 01 /2 (o32 LGDT [mem]).
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin gpreflct.asm -o gpreflct.com
cpu 386
org 0x100
%define OK 0xA5

start:
    xor ax, ax
    mov es, ax
    cli
    mov word [es:0x0D*4], gp_handler
    mov [es:0x0D*4+2], cs
    sti
    mov byte [gp_hit], 0

lgdt_site:
    o32 lgdt [gdtval]             ; privileged in V86 -> #GP -> the monitor
                                  ; must reflect INT 0Dh to our handler
resume:
    cmp byte [gp_hit], 1          ; the handler ran exactly once and skipped
    jne f_nohit                   ; us past the instruction
    mov al, OK
    jmp sig

; INT 0Dh handler. Stack: IP, CS, FLAGS (real-mode frame). Fault semantics:
; the return IP must point AT lgdt_site; we skip past the instruction
; (6 bytes: 66 0F 01 16 + disp16) by loading the resume label directly.
gp_handler:
    push bp
    mov bp, sp
    push ax
    mov ax, [bp+2]
    cmp ax, lgdt_site
    jne .badip
    mov word [bp+2], resume
    mov byte [cs:gp_hit], 1
    pop ax
    pop bp
    iret
.badip:                           ; wrong IP: fail from inside the handler
    mov al, 0xE2
    jmp sig

f_nohit:  mov al, 0xE1

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

align 4
gp_hit: db 0
align 8
gdtval: dw 0x0027                 ; a harmless pseudo-descriptor image (never
        dd 0                      ; actually loaded: the LGDT always faults)
