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

    ; --- cross-interface consistency: all three describe ONE pool ----------
    ; This is the check that was missing, not a nice-to-have: every existing
    ; assertion in this fixture derives its own baseline from whatever EMS/
    ; VCPI report and only checks that the SAME number moves and recovers, so
    ; a manager that reports a consistently WRONG figure -- corrupted but
    ; internally self-consistent -- passes every one of them. A real instance
    ; of exactly this shipped and was caught only by an unrelated benchmark:
    ; a register clobber made EMS's `AH=42h` answer 26 pages (416 KB) against
    ; an actual pool near 22,000 KB, and both this fixture and emsfrag passed
    ; anyway, because each only checked bx against ITS OWN prior reading.
    ;
    ; The fix is to check the three interfaces against EACH OTHER, which is
    ; what this fixture's premise (`DOS/16M error: ... no memory for VCPI
    ; page table`) is actually about: XMS `AH=08h` DX (total free KB), VCPI
    ; `DE03` EDX (total free 4 KB pages) and EMS `AH=42h` BX (total free
    ; 16 KB pages) all describe the same shared arena, so `VCPI*4` and
    ; `EMS*16` must each land close under the XMS KB figure.
    ;
    ; "Close under", not exact, and NOT a hardcoded page count (that re-pins
    ; the fixture to one profile, the trap emsfrag's own baseline just
    ; escaped): arena_query32 rounds each free SPAN's usable region up to the
    ; type's boundary at the start and down to a multiple of it at the end,
    ; so a coarser type can under-report a finer one -- never over-report --
    ; by up to 2*(boundary-1) KB PER FREE SPAN (boundary in KB: VCPI 4, EMS
    ; 16). At this point in the program at most the shell's own startup usage
    ; has touched the pool, so more than a handful of spans would be
    ; surprising; the tolerances below assume up to 8, an order of magnitude
    ; of headroom over the realistic 1-3, while staying two orders of
    ; magnitude below the ~21,000 KB gap the corruption above actually
    ; produced. A single unsigned compare on (xms_kb - interface_kb) catches
    ; both directions at once: an interface reporting MORE than XMS wraps
    ; that subtraction to a huge unsigned value, which fails the same "> TOL"
    ; test a too-large shortfall does.
    VCPI_TOL_KB equ 2 * (4  - 1) * 8   ; = 48  KB (6 KB/span  * 8 spans)
    EMS_TOL_KB  equ 2 * (16 - 1) * 8   ; = 240 KB (30 KB/span * 8 spans)
    mov ah, 0x08                  ; DX = total free KB (AX = largest, unused
    call far [xms_entry]          ; here -- this check only needs the total)
    movzx edx, dx
    mov [xms_total0], edx
    mov eax, [vcpi_free0]
    shl eax, 2                    ; VCPI 4 KB pages -> KB
    mov ecx, [xms_total0]
    sub ecx, eax                  ; wraps huge if eax > ecx (VCPI over-reports)
    cmp ecx, VCPI_TOL_KB
    ja f_xagree_vcpi0
    movzx eax, word [ems_free0]
    shl eax, 4                    ; EMS 16 KB pages -> KB
    mov ecx, [xms_total0]
    sub ecx, eax
    cmp ecx, EMS_TOL_KB
    ja f_xagree_ems0

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

    ; --- cross-interface consistency again, post-release -------------------
    ; The two checks just above prove EMS and VCPI recovered to their OWN
    ; prior readings; they say nothing about whether XMS's own total also
    ; came back, since nothing else re-reads it after the grab. bx/edx are
    ; already proven == ems_free0/vcpi_free0 by the two checks above, so
    ; reusing those instead of re-reading 42h/DE03 a third time is exact, not
    ; an approximation.
    mov ah, 0x08
    call far [xms_entry]          ; DX = total free KB, post-release
    movzx edx, dx
    mov [xms_total0], edx
    mov eax, [vcpi_free0]
    shl eax, 2
    mov ecx, [xms_total0]
    sub ecx, eax
    cmp ecx, VCPI_TOL_KB
    ja f_xagree_vcpi1
    movzx eax, word [ems_free0]
    shl eax, 4
    mov ecx, [xms_total0]
    sub ecx, eax
    cmp ecx, EMS_TOL_KB
    ja f_xagree_ems1

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
; Cross-interface consistency (new): baseline codes stop at 0xEF, post-
; release at 0xF1, deliberately past every code above rather than woven in
; among them, so a failure here reads unambiguously as "the three interfaces
; disagree with each other," never as one more step in the grab/release
; sequence.
f_xagree_vcpi0: mov al, 0xEE      ; baseline: VCPI*4 KB vs XMS total KB
                jmp sig
f_xagree_ems0:  mov al, 0xEF      ; baseline: EMS*16 KB vs XMS total KB
                jmp sig
f_xagree_vcpi1: mov al, 0xF0      ; post-release: same check, same reason
                jmp sig
f_xagree_ems1:  mov al, 0xF1
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
xms_total0:  dd 0                 ; reused at baseline and at post-release
xms_handle:  dw 0
xms_grab_kb: dw 0
ems_free0:   dw 0
