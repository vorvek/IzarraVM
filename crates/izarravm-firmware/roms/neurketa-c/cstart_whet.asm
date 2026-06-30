; cstart_whet.asm - freestanding C runtime stub for the Whetstone .EXE.
;
; Mirrors cstart.asm (the Dhrystone stub): a small-model .EXE, not a .COM, for
; the same DGROUP-relocation reason (Open Watcom places the C globals in DGROUP
; at offsets only MZ relocations can satisfy; a flat .COM mis-relocates them and
; corrupts the code segment). This file is linked first (start=cstart_) and:
;
;   cstart_   the .EXE entry: points DS=ES at DGROUP, FNINIT's the FPU (Whetstone
;             is floating-point heavy), then calls the C driver, with a CMD_EXIT
;             safety net if the driver ever returns.
;   report_   the device report-and-exit path the C driver tail-calls. Identical
;             protocol to the Dhrystone stub: iterations + a 16-bit self-check.
;
; Whetstone needs no strcpy/strcmp, so unlike the Dhrystone stub this one omits
; them. The transcendentals it uses are inline-8087 #pragma aux wrappers in the C,
; so the .EXE still links with ZERO libraries.
;
; Open Watcom decorates cdecl/register names with a trailing underscore.

.8086
.387

; Lotura unit-tester ports (see crates/izarravm-machine/src/unittester.rs).
PORT_INDEX   equ 0E4h
PORT_DATA    equ 0E5h
PORT_COMMAND equ 0E6h
CMD_EXIT     equ 3

; Register-file offsets.
REG_EXIT_OFF equ 12
RES_ITER_OFF equ 17
RES_AUX_OFF  equ 21
RES_STAT_OFF equ 25

; Standard Open Watcom small-model segments, declared so DGROUP resolves and
; merges with the C objects at link.
CONST   segment word public 'DATA'
CONST   ends
CONST2  segment word public 'DATA'
CONST2  ends
_DATA   segment word public 'DATA'
    ; Open Watcom's -fpi87 codegen references __8087 (the FPU-presence marker the
    ; normal C startup defines). This freestanding stub FNINIT's the FPU itself and
    ; assumes one is present (the bench is 486+), so we just satisfy the symbol.
    public __8087
__8087  dw 3
_DATA   ends
_BSS    segment word public 'BSS'
_BSS    ends
STACK   segment para stack 'STACK'
    dw 2048 dup(?)
STACK   ends
DGROUP  group CONST, CONST2, _DATA, _BSS

_TEXT segment word public 'CODE'
    assume cs:_TEXT, ds:DGROUP, ss:DGROUP
    public cstart_, report_
    extern whet_main_:near
cstart_:
    cli
    cld
    mov ax, DGROUP
    mov ds, ax
    mov es, ax
    fninit
    call whet_main_
    xor ax, ax
    xor dx, dx
report_:
    mov cx, ax
    mov bx, dx
    mov al, RES_ITER_OFF
    out PORT_INDEX, al
    mov ax, cx
    out PORT_DATA, al
    mov al, ah
    out PORT_DATA, al
    xor al, al
    out PORT_DATA, al
    out PORT_DATA, al
    mov ax, bx
    out PORT_DATA, al
    mov al, ah
    out PORT_DATA, al
    xor al, al
    out PORT_DATA, al
    out PORT_DATA, al
    mov al, 1
    out PORT_DATA, al
    mov al, REG_EXIT_OFF
    out PORT_INDEX, al
    xor al, al
    out PORT_DATA, al
    mov al, CMD_EXIT
    out PORT_COMMAND, al
rh:
    hlt
    jmp rh
_TEXT ends
end cstart_
