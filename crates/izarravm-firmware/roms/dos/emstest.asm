; emstest.com — SP-4b M2 EMS e2e fixture. Runs in V86 under TOKAEMM loaded with
; DEVICE=C:\TOKAEMM.SYS RAM (the page frame provisioned).
;
; version -> frame segment -> page counts -> allocate 4 pages -> map logical
; pages through the frame slots, writing distinct patterns and reading them
; back through OTHER slots (the runtime-remap proof: the same backing page is
; visible wherever it is mapped) -> then signal 0xA5 (success) via the
; unit-tester exit port. Any other code names the step that broke (0xEn).
;
; Build: nasm -f bin emstest.asm -o emstest.com
cpu 386
org 0x100
%define OK 0xA5
%define PAT_A 0xA55A1234
%define PAT_B 0x0FF0C3C3

start:
    ; 1. version (46h): AL = BCD 4.0
    mov ah, 0x46
    int 0x67
    or ah, ah
    jnz f_ver
    cmp al, 0x40
    jne f_ver

    ; 2. page frame segment (41h): BX = 0xE000
    mov ah, 0x41
    int 0x67
    or ah, ah
    jnz f_frame
    cmp bx, 0xE000
    jne f_frame

    ; 3. page counts (42h): total = free = 256 (4 MB pool on the 16 MB box)
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_counts
    cmp dx, 256
    jne f_counts
    cmp bx, 256
    jne f_counts

    ; 4. allocate 4 logical pages (43h) -> DX = handle
    mov ah, 0x43
    mov bx, 4
    int 0x67
    or ah, ah
    jnz f_alloc
    mov [handle], dx

    ; 5. map logical 0 -> slot 0 (44h); write pattern A through the frame
    mov ah, 0x44
    xor al, al
    xor bx, bx
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_map0
    mov ax, 0xE000
    mov es, ax
    mov dword [es:0], PAT_A
    cmp dword [es:0], PAT_A
    jne f_map0

    ; 6. map logical 1 -> slot 0; write pattern B
    mov ah, 0x44
    xor al, al
    mov bx, 1
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_map1
    mov dword [es:0], PAT_B
    cmp dword [es:0], PAT_B
    jne f_map1

    ; 7. map logical 0 -> slot 1: pattern A must be visible at E400 (the
    ;    remap proof — the backing page moved to a different frame window)
    mov ah, 0x44
    mov al, 1
    xor bx, bx
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_remap
    mov ax, 0xE400
    mov es, ax
    cmp dword [es:0], PAT_A
    jne f_remap

    ; 8. map logical 1 -> slot 1: pattern B follows it
    mov ah, 0x44
    mov al, 1
    mov bx, 1
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_remap2
    cmp dword [es:0], PAT_B
    jne f_remap2

    mov al, OK
    jmp sig

f_ver:    mov al, 0xE1
          jmp sig
f_frame:  mov al, 0xE2
          jmp sig
f_counts: mov al, 0xE3
          jmp sig
f_alloc:  mov al, 0xE4
          jmp sig
f_map0:   mov al, 0xE5
          jmp sig
f_map1:   mov al, 0xE6
          jmp sig
f_remap:  mov al, 0xE7
          jmp sig
f_remap2: mov al, 0xE8

sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                 ; REG_EXIT
    mov al, ah
    out 0xE5, al                 ; code
    mov al, 3
    out 0xE6, al                 ; CMD_EXIT
.h: jmp .h

handle: dw 0
