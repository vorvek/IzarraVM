; umbtest.com — SP-4b M3 UMB e2e fixture. Runs in V86 under TOKAEMM + DOS=UMB.
;
; Proves the full integration: with DOS=UMB, FreeDOS calls TOKAEMM's XMS 10h at
; SYSINIT and links our paged upper region into its MCB chain. This program sets
; the high-first allocation strategy, AH=48h-allocates a block, asserts it landed
; in upper memory (segment >= 0xC800), and writes/reads a pattern to prove real RAM
; is there via the page-remap (without it the segment reads open-bus 0xFF). Then
; frees and signals 0xA5. A non-0xA5 code names the failed step.
;
; Build: nasm -f bin umbtest.asm -o umbtest.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; 1. link UMBs into the MCB chain (INT 21h AX=5803 BX=0001)
    mov ax, 0x5803
    mov bx, 0x0001
    int 0x21
    ; 2. allocation strategy = high memory first, then low (AX=5801 BX=0080)
    mov ax, 0x5801
    mov bx, 0x0080
    int 0x21
    ; 3. allocate 0x40 paragraphs (1 KB)
    mov ah, 0x48
    mov bx, 0x0040
    int 0x21
    jc fail_alloc
    mov [blk], ax
    ; 4. assert the block landed in upper memory (segment >= 0xC800)
    cmp ax, 0x0C800
    jb fail_notumb
    ; 5. write a 256-byte pattern to seg:0 and read it back — proves real RAM behind
    ;    the UMB (the page-remap); without it the segment reads open bus (0xFF).
    mov es, ax
    xor di, di
    mov cx, 256
    mov al, 0x5A
    cld
    rep stosb
    xor si, si
    mov cx, 256
.vloop:
    mov al, [es:si]
    cmp al, 0x5A
    jne fail_ram
    inc si
    loop .vloop
    ; 6. free the block
    mov es, [blk]
    mov ah, 0x49
    int 0x21
    jc fail_free
    mov al, OK
    jmp sig

fail_alloc:  mov al, 0xE0
             jmp sig
fail_notumb: mov al, 0xE1
             jmp sig
fail_ram:    mov al, 0xE2
             jmp sig
fail_free:   mov al, 0xE3

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

blk: dw 0
