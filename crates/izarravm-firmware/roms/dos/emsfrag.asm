; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; emsfrag.com: EMS allocation out of a deliberately fragmented pool.
;
; Once Task 6 lands, EMS loses its static 3 MB partition and backs each
; handle's logical pages one at a time off the same shared arena XMS and VCPI
; already draw from (386MAX's ALLOCEMS: pop pages off a chain, no contiguity
; requirement). Today, EMS still owns that static partition outright
; (EMS_MAX_PAGES = 192 16 KB pages, tokaemm.asm ef_alloc/ems_find_run) and a
; handle's pages must land in ONE contiguous run.
;
; A first draft of this fixture fragmented the shared XMS/VCPI arena (via
; INT 2Fh 4300/4310 and XMS AH=08h/09h) and then asked EMS for pages,
; expecting the arena's holes to starve the request. That cannot work: EMS's
; static partition sits physically above the arena (tokaemm.asm INIT computes
; xms_pool_end = ems_phys_base) and ef_alloc never calls arena_alloc -- it
; only walks ems_table. Proof, not assertion: the sibling emmprobe.asm fixture
; already demonstrates this split. It grabs every free XMS kilobyte and checks
; whether the EMS free count moves; today it fails at its own 0xE4 ("EMS free
; did not move") specifically BECAUSE EMS is not on the arena yet. Fragmenting
; the arena therefore proves nothing about EMS's allocator -- it only proves
; what emmprobe.asm already proves, and does not touch the memory ef_alloc
; actually searches.
;
; So this fixture fragments EMS's OWN pool through EMS's OWN interface
; (INT 67h AH=43h/45h), which is the only interface wired to what ef_alloc
; searches today, and which after Task 6 will draw from the arena instead.
;
; TASK 6 MUST EDIT THIS FIXTURE, not merely watch it go green. Three things
; below are pinned to the pre-fix world. Each fails loudly instead of passing
; for the wrong reason, which is the intended failure mode, but each has to be
; re-derived before this can ever report 0xA5:
;   0xE4  the baseline pins the pool at 192 pages. Task 6 derives ems_pages
;         from the arena (about 1456 on the 24 MB profile). Read the total from
;         AH=42h and split it across EMS_HANDLES instead. Do NOT just delete
;         the check: draining EXACTLY the pool is what leaves no contiguous
;         tail, and a tail satisfies the request without any fragmentation.
;   0xE6  the drain is 32 * 6, which empties a 192-page pool and nothing else.
;   0xE9  the premise probe requires AH=43h(7) to FAIL, which holds only while
;         the allocator demands contiguity. After Task 6 it SUCCEEDS, so the
;         probe has to become a discriminator (fails -> pre-fix, expect 0xEA
;         below; succeeds -> post-fix, release it and require the real request
;         to succeed) rather than the hard gate it is today.
;
; Shape: EMS_MAX_PAGES is a fixed 192-page pool and EMS_HANDLES is a fixed 32
; slots (both from tokaemm.asm), and 32 * 6 = 192 exactly, so a drain of 32
; handles of 6 pages each empties the pool completely with no leftover
; contiguous tail -- the exact flaw both superseded designs missed: an
; untouched remainder anywhere satisfies the request without exercising
; fragmentation at all. Freeing every other handle then leaves 16 free runs of
; 6 pages (96 KB) each, none of which can satisfy an 8-page (128 KB) request,
; while 16 * 6 = 96 pages are free in total -- twelve times what is asked for.
;
; The premise (no run >= 7 pages survives the punch) is proved by an actual
; INT 67h AH=43h(7) probe, not by trusting the arithmetic above: if a 7-page
; run existed, the probe would succeed and the fixture stops with a premise
; code distinct from the defect code, rather than silently drawing the wrong
; conclusion from the real 8-page request that follows.
;
; Signals 0xA5 (success) via the unit-tester exit port; 0xEn names the step.
; 0xEA is the headline code: EMS refused an 8-page request while 96 pages
; were free, because none of them were contiguous -- the defect Task 6 fixes.
;
; Build: nasm -f bin emsfrag.asm -o emsfrag.com
cpu 386
EMS_HOLE_PAGES    equ 6               ; pages per drain handle == the max single
                                      ; free run once every other is freed
EMS_DRAIN_HANDLES equ 32              ; == tokaemm.asm EMS_HANDLES; also drains
                                      ; the pool exactly (32*6 == EMS_MAX_PAGES)
WANT_PAGES        equ 8               ; 128 KB; > EMS_HOLE_PAGES so no single
                                      ; hole can satisfy it, and comfortably
                                      ; less than the 96 pages left free
FRAME_SEG         equ 0xE000          ; tokaemm.asm EMS_FRAME_SEG (fixed frame)
org 0x100
%define OK 0xA5

start:
    ; --- baseline: the pool must be exactly the fixed shape this fixture
    ; drains, and nothing must already hold a page -----------------------
    mov ah, 0x42                  ; get page counts: BX=free, DX=total
    int 0x67
    or ah, ah
    jnz f_noems
    cmp bx, dx
    jne f_baseline_dirty          ; something already holds EMS pages
    cmp dx, EMS_DRAIN_HANDLES * EMS_HOLE_PAGES
    jne f_baseline_shape          ; pool is not the fixed 192-page shape

    ; --- drain: 32 handles of 6 pages each, empties the pool exactly ----
    xor si, si
.drain:
    mov ah, 0x43
    mov bx, EMS_HOLE_PAGES
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
    ; equal-size handles + alternating release == 16 isolated 6-page holes,
    ; none adjacent (their neighbours are still-allocated 6-page walls), so
    ; no leftover contiguous run survives anywhere in the pool.
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
    cmp bx, WANT_PAGES
    jb f_premise_total

    ; a run one page bigger than a single hole must NOT be allocatable, or
    ; the punch did not create the fragmentation this fixture depends on.
    mov ah, 0x43
    mov bx, EMS_HOLE_PAGES + 1
    int 0x67
    or ah, ah
    jz f_premise_run              ; succeeded: a bigger run exists than meant
    cmp ah, 0x88
    jne f_emm_err                 ; some other EMM status, not the expected one

    ; --- the real test: today this must be refused; after Task 6 it must
    ; succeed by scattering the 8 logical pages across the 16 free holes --
    mov ah, 0x43
    mov bx, WANT_PAGES
    int 0x67
    or ah, ah
    jz .allocated
    cmp ah, 0x88
    je f_refused                  ; THE DEFECT: contiguous-only allocator
    jmp f_emm_err

.allocated:
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
    cmp si, WANT_PAGES
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
    cmp si, WANT_PAGES
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
f_baseline_dirty:    mov al, 0xE3   ; something already held EMS pages
                     jmp sig
f_baseline_shape:    mov al, 0xE4   ; total pages != 192; drain math would lie
                     jmp sig

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
f_premise_run:       mov al, 0xE9   ; a bigger-than-hole run survived the punch
                     jmp sig        ; (NOT the defect: the fixture's own setup)

; --- THE DEFECT: this is the code Task 6 exists to stop producing -------
f_refused:           mov al, 0xEA   ; EMS refused 8 pages with 96 free: no
                     jmp sig        ; single free run was big enough

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

ems_handle:  dw 0
ems_handles: times EMS_DRAIN_HANDLES dw 0
