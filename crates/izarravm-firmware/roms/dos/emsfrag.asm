; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; emsfrag.com: EMS allocation out of a deliberately fragmented pool.
;
; Task 6 backs each handle's logical pages one at a time off the same shared
; arena XMS and VCPI draw from (386MAX's ALLOCEMS: pop pages off a chain, no
; contiguity requirement) instead of the old static 3 MB partition
; (EMS_MAX_PAGES = 192 16 KB pages) that required a handle's pages to land in
; ONE contiguous run.
;
; A first draft of this fixture fragmented the shared XMS/VCPI arena (via
; INT 2Fh 4300/4310 and XMS AH=08h/09h) and then asked EMS for pages,
; expecting the arena's holes to starve the request. That could never work on
; the pre-fix driver: EMS's static partition sat physically above the arena
; and ef_alloc never called arena_alloc. So this fixture fragments EMS's OWN
; pool through EMS's OWN interface (INT 67h AH=43h/45h) instead, which stays
; meaningful on both sides of the change.
;
; Draining the pool used to mean 32 handles of a fixed 6 pages (32*6 ==
; EMS_MAX_PAGES == 192, chosen so the drain empties the pool with nothing left
; over). Post-fix, EMS draws from the WHOLE shared arena -- about 1456 pages on
; the 24 MB profile, not 192 -- and that total is no longer a compile-time
; constant: the pool is not even guaranteed to start fully free (COMMAND.COM's
; own XMS-swap block for running this child, if the shell was built with
; XMS-Swap support, legitimately holds part of it, and that is now NORMAL
; because EMS and XMS share one pool). So the baseline reads the pool's actual
; FREE count (AH=42h, BX) and derives the drain geometry from that instead of
; from a hardcoded 192:
;   hole_pages = free / EMS_HANDLES (32), remainder = free mod EMS_HANDLES.
;   Handle 0 (an EVEN index, so the punch below never frees it) also carries
;   the remainder, so every ODD-indexed "hole" handle is exactly hole_pages --
;   uniform hole sizes are what let a single "hole_pages + 1" probe below mean
;   anything.
;
; The premise probe (does a run bigger than one hole exist?) used to be a hard
; gate: it had to FAIL, because the old allocator demanded a contiguous run and
; no single 6-page hole could satisfy a 7-page ask. Once backing is
; non-contiguous that stops being true: EMS only ever takes pages one at a
; time now (ems_page_alloc, 16 granules per call), so "hole_pages + 1 pages"
; is satisfiable the instant hole_pages+1 free 16 KB units exist ANYWHERE in
; the pool -- which they do, since the 16 punched holes total far more than
; that. So the probe is a DISCRIMINATOR, not a gate:
;   fails (0x88)  -> the driver still demands contiguity; a bigger want_pages
;                    request cannot possibly do better, so signal the defect
;                    (0xEA) directly without even trying it.
;   succeeds      -> backing is non-contiguous. Free the probe's handle back
;                    (it only existed to prove the point) and make the real,
;                    dedicated want_pages request, then prove per-page
;                    signatures survive it.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
; 0xEA is the headline code: EMS refused a request while enough total pages
; were free, because none of them were contiguous -- the defect Task 6 fixes.
;
; Build: nasm -f bin emsfrag.asm -o emsfrag.com
cpu 386
EMS_DRAIN_HANDLES equ 32              ; == tokaemm.asm EMS_HANDLES
FRAME_SEG         equ 0xE000          ; tokaemm.asm EMS_FRAME_SEG (fixed frame)
org 0x100
%define OK 0xA5

start:
    ; --- baseline: read the pool's ACTUAL free count. That is the working
    ; total for this fixture, not the fixed grand total (AH=42h's DX): the
    ; shared arena is not guaranteed to start fully free now that EMS draws on
    ; the same pool XMS does, and that is expected, not a setup error. -------
    mov ah, 0x42                  ; get page counts: BX=free, DX=total
    int 0x67
    or ah, ah
    jnz f_noems
    cmp bx, dx
    ja f_baseline_dirty           ; free > total is an impossible/corrupt state
    cmp bx, EMS_DRAIN_HANDLES * 2 ; need >=2 pages/handle or the hole/probe
    jb f_baseline_shape           ; geometry below is not meaningful

    mov ax, bx                    ; AX = free pages at baseline
    xor dx, dx
    mov cx, EMS_DRAIN_HANDLES
    div cx                        ; AX = free / 32, DX = free mod 32
    mov [hole_pages], ax
    mov [remainder], dx

    mov ax, [hole_pages]          ; want_pages ~= 1.5x a hole: bigger than any
    mov cx, ax                    ; single hole, comfortably under the total
    shr cx, 1                     ; free left after the punch (16 holes' worth)
    add ax, cx
    inc ax
    mov [want_pages], ax

    ; --- drain: EMS_DRAIN_HANDLES handles. Index 0 (even, so the punch below
    ; never frees it) also carries the remainder, so every ODD index is
    ; exactly hole_pages -- the uniform hole size the discriminator needs. ---
    xor si, si
.drain:
    mov bx, [hole_pages]
    or si, si
    jnz .drain_alloc
    add bx, [remainder]
.drain_alloc:
    mov ah, 0x43
    int 0x67
    or ah, ah
    jnz f_drain
    mov bx, si
    add bx, bx
    mov [ems_handles + bx], dx
    inc si
    cmp si, EMS_DRAIN_HANDLES
    jb .drain

    mov ah, 0x42                  ; confirm nothing is left: BX must be 0
    int 0x67
    or ah, ah
    jnz f_emm_err
    or bx, bx
    jnz f_drain_incomplete

    ; --- punch holes: free every other handle (odd table index) --------
    ; equal-size handles + alternating release == 16 isolated hole_pages
    ; holes, none adjacent (their neighbours are still-allocated walls).
    mov si, 1
.punch:
    mov bx, si
    add bx, bx
    mov dx, [ems_handles + bx]
    mov ah, 0x45
    int 0x67
    or ah, ah
    jnz f_punch
    mov word [ems_handles + bx], 0
    add si, 2
    cmp si, EMS_DRAIN_HANDLES
    jb .punch

    ; --- prove the premise BEFORE drawing any conclusion -----------------
    mov ah, 0x42                  ; total free must be enough for the ask
    int 0x67
    or ah, ah
    jnz f_emm_err
    cmp bx, [want_pages]
    jb f_premise_total

    ; --- discriminator: one page bigger than a single hole ---------------
    mov ah, 0x43
    mov bx, [hole_pages]
    inc bx
    int 0x67
    or ah, ah
    jz .probe_ok
    cmp ah, 0x88
    je f_refused                  ; THE DEFECT: contiguous-only allocator
    jmp f_premise_status          ; some other EMM status, not one of the two
                                   ; the discriminator knows how to read
.probe_ok:
    mov ah, 0x45                  ; release the probe's handle (DX, still set
    int 0x67                      ; from the successful 43h above): it only
    or ah, ah                     ; existed to prove the point
    jnz f_emm_err

    ; --- the real test: scatter want_pages logical pages across the holes -
    mov ah, 0x43
    mov bx, [want_pages]
    int 0x67
    or ah, ah
    jnz f_emm_err
    mov [ems_handle], dx

    ; --- prove the mapping: signature each logical page, then verify -----
    xor si, si
.write:
    mov ah, 0x44                  ; map logical SI into physical slot 0
    xor al, al
    mov bx, si
    mov dx, [ems_handle]
    int 0x67
    or ah, ah
    jnz f_map
    mov ax, FRAME_SEG
    mov es, ax
    mov ax, si
    add ax, 0x5A00                ; a per-page signature
    mov [es:0], ax
    mov [es:0x3FFE], ax           ; and again at the page's last word
    inc si
    cmp si, [want_pages]
    jb .write

    xor si, si
.read:
    mov ah, 0x44
    xor al, al
    mov bx, si
    mov dx, [ems_handle]
    int 0x67
    or ah, ah
    jnz f_map
    mov ax, FRAME_SEG
    mov es, ax
    mov ax, si
    add ax, 0x5A00
    cmp [es:0], ax
    jne f_verify
    cmp [es:0x3FFE], ax
    jne f_verify
    inc si
    cmp si, [want_pages]
    jb .read

    ; --- release everything ----------------------------------------------
    mov ah, 0x45
    mov dx, [ems_handle]
    int 0x67
    or ah, ah
    jnz f_free

    xor si, si
.cleanup:
    mov bx, si
    add bx, bx
    mov dx, [ems_handles + bx]    ; the 16 still-held "wall" handles (even
    mov ah, 0x45                  ; indices); the odd ones were freed above
    int 0x67
    or ah, ah
    jnz f_free
    add si, 2
    cmp si, EMS_DRAIN_HANDLES
    jb .cleanup

    mov al, OK
    jmp sig

; --- EMM status errors: the manager itself answered abnormally ----------
f_noems:             mov al, 0xE1
                     jmp sig
f_emm_err:           mov al, 0xE2
                     jmp sig

; --- baseline shape: the pool is not what this fixture was built for ----
f_baseline_dirty:    mov al, 0xE3   ; free > total: impossible/corrupt state
                     jmp sig
f_baseline_shape:    mov al, 0xE4   ; free pool too small for this fixture's
                     jmp sig        ; per-handle geometry (< 2 pages/handle)

; --- drain: could not empty the pool as designed -------------------------
f_drain:             mov al, 0xE5   ; a drain-loop alloc failed early
                     jmp sig
f_drain_incomplete:  mov al, 0xE6   ; drained all handles but free != 0
                     jmp sig

; --- hole-punch: could not free a drained handle -------------------------
f_punch:             mov al, 0xE7
                     jmp sig

; --- premise: the fixture's own setup did not achieve its precondition --
f_premise_total:     mov al, 0xE8   ; not enough total free after punching
                     jmp sig
f_premise_status:    mov al, 0xE9   ; discriminator probe returned neither
                     jmp sig        ; success nor 0x88

; --- THE DEFECT: this is the code Task 6 exists to stop producing -------
f_refused:           mov al, 0xEA   ; EMS refused hole_pages+1 pages while
                     jmp sig        ; enough were free: no single run big enough

; --- reachable only once EMS actually grants the fragmented request -----
f_map:               mov al, 0xEB
                     jmp sig
f_verify:            mov al, 0xEC   ; a logical page did not read back its
                     jmp sig        ; own data: the mapping/backing mislinked
f_free:              mov al, 0xED
                     jmp sig

; AL = exit code -> unit-tester exit port, then stop the machine.
; Every label above jumps here explicitly: no code below depends on falling
; through to the next one, so inserting a label cannot silently retag a code.
sig:
    mov ah, al
    mov al, 12
    out 0xE4, al                  ; REG_EXIT
    mov al, ah
    out 0xE5, al                  ; code
    mov al, 3
    out 0xE6, al                  ; CMD_EXIT
.h: jmp .h

hole_pages:  dw 0
remainder:   dw 0
want_pages:  dw 0
ems_handle:  dw 0
ems_handles: times EMS_DRAIN_HANDLES dw 0
