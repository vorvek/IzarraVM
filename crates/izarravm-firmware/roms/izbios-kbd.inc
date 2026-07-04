; izbios-kbd.inc - keyboard foundation for the full Izarra BIOS: the shared
; INT 09h / INT 16h core (kbd-bios-core.inc, the single source also used by
; kbd-bios.asm and kbd-resident.asm) plus kb_getkey / kb_flush for the setup
; page. The BDA ring offsets (KB_BUF_*) come from izbios-defs.inc; the layout
; tables come from izarra-bios.asm's frozen include list, so the core's own
; copies of both are suppressed. Installed at IVT[09h]/IVT[16h] by core
; bring-up.

; kb_getkey: keyboard access for the setup page.
;   AH=0 -> blocking read via INT 16h: AL=ASCII, AH=scancode.
;   AH=1 -> peek: CF=1 if empty; else AL=ASCII, AH=scancode (left in the ring).
; The peek reads the BDA ring directly rather than calling INT 16h AH=1: an ISR
; cannot return a flag through IRET (which pops the caller's saved FLAGS), so the
; peek's carry result would be lost. A near return preserves CF correctly.
kb_getkey:
    cmp ah, 0x01
    je .peek
    int 0x16
    ret
.peek:
    push ds
    push bx
    mov bx, BDA_SEG
    mov ds, bx
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .empty
    mov ax, [bx]                ; AH=scancode, AL=ASCII (not consumed)
    pop bx
    pop ds
    clc
    ret
.empty:
    pop bx
    pop ds
    stc
    ret

; kb_flush: drop everything in the ring (head = tail).
kb_flush:
    push ds
    push ax
    push bx
    mov ax, BDA_SEG
    mov ds, ax
    mov bx, [KB_BUF_TAIL]
    mov [KB_BUF_HEAD], bx
    pop bx
    pop ax
    pop ds
    ret

%define KBD_CORE_OMIT_LAYOUTS 1
%include "kbd-bios-core.inc"
