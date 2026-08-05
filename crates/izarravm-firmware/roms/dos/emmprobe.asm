; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; emmprobe.com: the DOS/16M pool-overlap probe, as a fixture.
;
; DOS/16M (and so DOS/4GW, and so DOOM.EXE) decides whether XMS and the other
; memory interfaces share one pool by measuring: read a free count, allocate
; EVERY free XMS kilobyte, read the count again, release. If the second reading
; did not move it concludes the pools are disjoint and KEEPS the XMS block,
; leaving the VCPI pool empty -- which is exactly how DOOM died with
; "DOS/16M error: [23] no memory for VCPI page table".
;
; This fixture asserts the manager tells the truth in both directions: both the
; EMS and the VCPI free counts collapse while XMS holds everything, and both
; recover when it is released. Run it with EMS ENABLED; NOEMS never had the bug.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
;
; Build: nasm -f bin emmprobe.asm -o emmprobe.com
cpu 386
org 0x100
%define OK 0xA5
; XMS free must be well clear of the two baselines below for a collapse to mean
; anything. A floor, not a pin: the 24 MB profile leaves about 20 MB free here,
; so this only rejects a machine too small to be testing anything.
XMS_MIN_KB equ 2048

start:
    ; --- XMS entry point ---------------------------------------------------
    mov ax, 0x4300
    int 0x2F
    cmp al, 0x80
    jne f_noxms
    mov ax, 0x4310
    int 0x2F
    mov [xms_entry], bx
    mov [xms_entry+2], es

    ; --- baseline: EMS free pages (AH=42h) and VCPI free pages (DE03) ------
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_noems
    mov [ems_free0], bx
    cmp bx, 16                    ; a meaningful EMS pool must exist, or this
    jb f_noems                    ; fixture is not testing anything
    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_novcpi
    mov [vcpi_free0], edx
    cmp edx, 256
    jb f_novcpi

    ; --- grab every free XMS kilobyte -------------------------------------
    ; ONE allocation only takes every free kilobyte while the arena is a single
    ; unbroken run, so require largest == total first. Nothing else holds an
    ; arena page today, but task 6 backs EMS pages from it and task 9 puts the
    ; monitor structures in it; from then on a hole would leave memory behind,
    ; the counts below would stay high, and 0xE4 would blame the driver for
    ; someone else's allocation.
    mov ah, 0x08                  ; AX = largest free KB, DX = total free KB
    call far [xms_entry]
    or ax, ax
    jz f_xms_query
    cmp ax, dx
    jne f_xms_frag                ; a hole: one alloc cannot take the arena
    cmp ax, XMS_MIN_KB
    jb f_xms_low                  ; too little free to prove anything either way
    mov [xms_grab_kb], ax
    mov dx, ax                    ; capture before AH is overwritten below
    mov ah, 0x09                  ; allocate exactly the largest block
    call far [xms_entry]
    or ax, ax
    jz f_xms_alloc
    mov [xms_handle], dx

    ; --- both counts must now read (near) zero ----------------------------
    ; Absolute page-count ceilings, not a proportion of the baseline (Task 6
    ; step 12). A "drop below 1/16 of baseline" bar scales the WRONG way --
    ; tolerance grows with installed RAM, so on a big enough machine a
    ; regression that leaves a real megabyte of pool unshared would still
    ; read as a pass. Measured directly with the tracer against the Task 6
    ; driver, 2026-08-05, 24 MB profile (IZARRAVM_INT_TRACE=67, one XMS grab
    ; that empties the arena's one remaining contiguous run per the guard
    ; above): EMS post-grab BX and VCPI post-grab EDX both read EXACTLY 0 --
    ; a single grab of the one free run leaves nothing behind at any
    ; granularity, which is what the shared bitmap makes possible now. The
    ; ceilings below are that measured 0 plus a small fixed margin, matched
    ; in KB across the two units (8 EMS pages = 128 KB, 32 VCPI pages = 128
    ; KB) rather than scaled by pool size, so growing the arena cannot loosen
    ; this test. An INT 67h status failure here is an EMM error, not the
    ; defect -- keep it out of the codes that name the defect.
    EMS_RESIDUE_MAX  equ 8         ; 16 KB pages;  128 KB margin above the 0
    VCPI_RESIDUE_MAX equ 32        ; 4 KB pages;   measured residue (see above)
    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_emm_err
    cmp bx, EMS_RESIDUE_MAX
    jae f_ems_hold                ; EMS free did not move -> the defect

    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_emm_err
    cmp edx, VCPI_RESIDUE_MAX
    jae f_vcpi_hold               ; VCPI free did not move

    ; --- release, and both counts must come back exactly -------------------
    mov ah, 0x0A
    mov dx, [xms_handle]
    call far [xms_entry]
    or ax, ax
    jz f_release

    mov ah, 0x42
    int 0x67
    or ah, ah
    jnz f_emm_err
    cmp bx, [ems_free0]
    jne f_ems_back                ; did not recover exactly

    mov ax, 0xDE03
    int 0x67
    or ah, ah
    jnz f_emm_err
    cmp edx, [vcpi_free0]
    jne f_vcpi_back

    mov al, OK
    jmp sig

f_noxms:     mov al, 0xE1
             jmp sig
f_noems:     mov al, 0xE2
             jmp sig
f_novcpi:    mov al, 0xE3
             jmp sig
f_ems_hold:  mov al, 0xE4        ; THE DEFECT: EMS free unmoved under the grab
             jmp sig
f_vcpi_hold: mov al, 0xE5
             jmp sig
f_release:   mov al, 0xE6
             jmp sig
f_ems_back:  mov al, 0xE7
             jmp sig
f_vcpi_back: mov al, 0xE8
             jmp sig
; 0xE9-0xED run BEFORE 0xE4-0xE8 in the program despite the higher numbers:
; 0xE4 is the headline "the defect" code, named by value in the shared-pool
; plan and in this fixture's e2e failure message, so every step that can only
; fail earlier was pushed past it rather than renumber 0xE4 out from under
; them. (The e2e assertion itself is on 0xA5; only the prose names 0xE4.)
f_xms_query: mov al, 0xE9
             jmp sig
f_xms_frag:  mov al, 0xEA
             jmp sig
f_xms_low:   mov al, 0xEB
             jmp sig
f_xms_alloc: mov al, 0xEC
             jmp sig
f_emm_err:   mov al, 0xED
             jmp sig

; AL = exit code -> unit-tester exit port, then stop the machine.
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
xms_entry:   dd 0
vcpi_free0:  dd 0
xms_handle:  dw 0
xms_grab_kb: dw 0
ems_free0:   dw 0
