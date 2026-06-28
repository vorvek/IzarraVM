; cstart.asm - freestanding C runtime stub for the Dhrystone .EXE.
;
; This benchmark is a small-model .EXE, not a .COM. A .COM is a flat binary
; with CS=DS=ES=SS, so the linker resolves every global into the one segment;
; but Open Watcom places the C globals in DGROUP at offsets the loader can only
; satisfy through MZ relocations, which a .COM has none of. Built as a .COM the
; global variables mis-relocate: data writes land in the code segment and
; corrupt it. Built as a .EXE the loader applies the relocations, DGROUP is a
; real data segment, and the globals resolve where the code expects them. This
; file is linked first (start=cstart_) and provides:
;
;   cstart_   the .EXE entry: points DS=ES at DGROUP, then calls the C driver,
;             with a CMD_EXIT safety net if the driver ever returns.
;   report_   the device report-and-exit path the C driver tail-calls.
;   strcpy_   freestanding strcpy (Dhrystone calls it in the measurement loop).
;   strcmp_   freestanding strcmp (Dhrystone calls it in the measurement loop).
;
; Open Watcom decorates cdecl/register names with a trailing underscore, hence
; the names below.

.8086

; Lotura unit-tester ports (see crates/izarravm-machine/src/unittester.rs).
PORT_INDEX   equ 0E4h
PORT_DATA    equ 0E5h
PORT_COMMAND equ 0E6h
CMD_EXIT     equ 3

; Register-file offsets.
REG_EXIT_OFF equ 12
RES_ITER_OFF equ 17
; RES_AUX_OFF and RES_STAT_OFF document the register layout for reference only.
; The report path reaches those offsets through the data-port post-increment
; after selecting RES_ITER_OFF, so it never names them; they are not load-bearing.
RES_AUX_OFF  equ 21
RES_STAT_OFF equ 25

; Standard Open Watcom small-model segments, declared so DGROUP resolves and
; merges with the C objects at link. A .EXE (unlike a .COM) keeps code and data
; in separate segments with relocations, so DS must be pointed at DGROUP at
; startup. That is the whole reason this benchmark is a .EXE: global variables
; then resolve into the data segment instead of overwriting the code.
CONST   segment word public 'DATA'
CONST   ends
CONST2  segment word public 'DATA'
CONST2  ends
_DATA   segment word public 'DATA'
_DATA   ends
_BSS    segment word public 'BSS'
_BSS    ends
STACK   segment para stack 'STACK'
    dw 2048 dup(?)
STACK   ends
DGROUP  group CONST, CONST2, _DATA, _BSS

_TEXT segment word public 'CODE'
    assume cs:_TEXT, ds:DGROUP, ss:DGROUP
    public cstart_, report_, strcpy_, strcmp_
    extern dhry_main_:near
cstart_:
    cli
    cld
    mov ax, DGROUP
    mov ds, ax
    mov es, ax
    call dhry_main_
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
strcpy_:
    push si
    push di
    mov di, ax
    mov si, dx
sc1:
    mov al, [si]
    mov [di], al
    inc si
    inc di
    test al, al
    jnz sc1
    pop di
    pop si
    ret
strcmp_:
    push si
    push di
    mov si, ax
    mov di, dx
sm1:
    mov al, [si]
    mov ah, [di]
    cmp al, ah
    jne sm2
    test al, al
    jz sm3
    inc si
    inc di
    jmp sm1
sm2:
    mov bl, ah
    xor ah, ah
    xor bh, bh
    sub ax, bx
    pop di
    pop si
    ret
sm3:
    xor ax, ax
    pop di
    pop si
    ret
_TEXT ends
end cstart_
