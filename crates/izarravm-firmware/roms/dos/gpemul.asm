; gpemul.com — TOKAEMM V86 privileged-0F emulation fixture (386MAX-surface
; port). A V86 task is CPL 3, so MOV r32,CRn / MOV CRn,r32 / CLTS / LMSW all
; #GP into the monitor, which must EMULATE them transparently (the way DOS16M
; and other extenders probe CR0), not reflect a fault. Verifies:
;   - MOV EAX,CR0 returns a CR0 with PE|PG set (we run in V86 under paging);
;   - MOV CR0,EAX with PE|PG cleared in the source is forced back on (the
;     monitor never lets a V86 client un-protect the live machine) yet a
;     benign toggled bit (TS) round-trips;
;   - CLTS clears TS;
;   - MOV EAX,CR3 and MOV EAX,CR2 read without faulting;
;   - LMSW with PE cleared in the image keeps PE set.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin gpemul.asm -o gpemul.com
cpu 386
org 0x100
%define OK 0xA5

start:
    ; 1. MOV EAX, CR0 — must read PE(bit0) and PG(bit31) set
    mov eax, cr0
    test eax, 1
    jz f_cr0read
    test eax, 0x80000000
    jz f_cr0read
    mov [saved_cr0], eax

    ; 2. MOV CR0, EAX with PE+PG cleared and TS set in the source; the monitor
    ;    forces PE|PG back on, so reading CR0 again must show PE|PG|TS.
    mov eax, [saved_cr0]
    or eax, 8                     ; set TS (bit 3), a benign toggle
    and eax, 0x7FFFFFFE           ; clear PG + PE in the value we write
    mov cr0, eax
    mov eax, cr0
    test eax, 1                   ; PE forced back on?
    jz f_cr0write
    test eax, 0x80000000          ; PG forced back on?
    jz f_cr0write
    test eax, 8                   ; TS actually took?
    jz f_cr0write

    ; 3. CLTS — clears TS
    clts
    mov eax, cr0
    test eax, 8
    jnz f_clts

    ; 4. MOV EAX, CR3 (page-directory base) and CR2 read without faulting
    mov eax, cr3
    test eax, 0xFFFFF000          ; a real PD base is nonzero
    jz f_cr3
    mov eax, cr2                  ; just must not fault

    ; 4b. Aliasing check: the monitor uses ESI/EDI as scratch while emulating,
    ;     so a MOV into ESI (rm=6) or EDI (rm=7) must still land in the guest's
    ;     register (written to the pushad slot, not lost to the live scratch).
    xor esi, esi
    mov esi, cr0                  ; dest = ESI: rm collides with monitor scratch
    test esi, 1                   ; must carry CR0's PE, not stay zero
    jz f_alias
    xor edi, edi
    mov edi, cr3                  ; dest = EDI: rm collides with monitor scratch
    test edi, 0xFFFFF000
    jz f_alias

    ; 5. LMSW with PE cleared in the image: PE stays set
    mov ax, [saved_cr0]
    and ax, 0xFFFE                ; clear PE in the MSW image
    lmsw ax
    mov eax, cr0
    test eax, 1
    jz f_lmsw

    ; restore a clean CR0 (PE|PG, TS clear) before exit
    mov eax, [saved_cr0]
    mov cr0, eax

    mov al, OK
    jmp sig

f_cr0read:  mov al, 0xE1
            jmp sig
f_cr0write: mov al, 0xE2
            jmp sig
f_clts:     mov al, 0xE3
            jmp sig
f_cr3:      mov al, 0xE4
            jmp sig
f_alias:    mov al, 0xE6
            jmp sig
f_lmsw:     mov al, 0xE5

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
saved_cr0: dd 0
