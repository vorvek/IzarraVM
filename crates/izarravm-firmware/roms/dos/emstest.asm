; emstest.com — SP-4b M2 EMS e2e fixture. Runs in V86 under TOKAEMM loaded with
; DEVICE=C:\TOKAEMM.SYS RAM (the page frame provisioned).
;
; version -> frame segment -> page counts -> allocate 4 pages -> map logical
; pages through the frame slots, writing distinct patterns and reading them
; back through OTHER slots (the runtime-remap proof: the same backing page is
; visible wherever it is mapped) -> save context -> unmap -> restore context
; (the mapping comes back) -> free and watch the counts recover -> then signal
; 0xA5 (success) via the unit-tester exit port. Any other code names the step
; that broke (0xEn).
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

    ; 9. save the mapping context (47h) under the handle
    mov ah, 0x47
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_save

    ; 10. unmap slot 1 (44h, logical 0xFFFF): E400 falls back to the dormant
    ;     UMB backing — pattern B must no longer be visible there
    mov ah, 0x44
    mov al, 1
    mov bx, 0xFFFF
    int 0x67
    or ah, ah
    jnz f_unmap
    cmp dword [es:0], PAT_B
    je f_unmap

    ; 11. restore the context (48h): slot 1 maps logical 1 again -> pattern B
    mov ah, 0x48
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_restore
    cmp dword [es:0], PAT_B
    jne f_restore

    ; 12. counts reflect the allocation (42h): free = 252 of 256
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_counts2
    cmp bx, 252
    jne f_counts2

    ; 13. pages for the handle (4Ch) = 4; open handles (4Bh) = 1
    mov ah, 0x4C
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_pages
    cmp bx, 4
    jne f_pages
    mov ah, 0x4B
    int 0x67
    or ah, ah
    jnz f_pages
    cmp bx, 1
    jne f_pages

    ; 14. free the handle (45h); counts recover, no open handles remain
    mov ah, 0x45
    mov dx, [handle]
    int 0x67
    or ah, ah
    jnz f_free
    mov ah, 0x42
    int 0x67
    cmp bx, 256
    jne f_free
    mov ah, 0x4B
    int 0x67
    or bx, bx
    jnz f_free

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
          jmp sig
f_save:   mov al, 0xE9
          jmp sig
f_unmap:  mov al, 0xEA
          jmp sig
f_restore: mov al, 0xEB
          jmp sig
f_counts2: mov al, 0xEC
          jmp sig
f_pages:  mov al, 0xED
          jmp sig
f_free:   mov al, 0xEE

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
