; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; TOKAEMM.SYS memory manager. Runs the system in V86.
;
; The driver's INIT builds a load-relative PM/paging and
; ring-0 monitor environment in its OWN resident memory, then instead of a
; signal stub it IRETDs the *running kernel* into V86 at the SYSINIT return
; point (the EXECRH post-INIT code), so real FreeDOS keeps booting virtualized
; under the monitor. The monitor emulates the V86 sensitive instructions
; (CLI/STI/PUSHF/POPF/INT/IRET via a virtual IF) and reflects the timer (IRQ0
; -> INT 08h) and keyboard (IRQ1 -> INT 09h) hardware interrupts to the guest's
; real-mode IVT, holding them pending while VIF is clear (real DOS brackets
; IRQ-sensitive code with CLI/STI).
;
; Addressing model (all load-segment relative):
;   * PM CODE selector 0x08  base = CS<<4    (monitor runs at driver offsets)
;   * PM DATA selector 0x10  base = 0 flat   (builds page tables at linear addrs)
;   * PM DATA selector 0x20  base = CS<<4    (monitor reaches its own VIF + the
;                                             saved kernel context, via FS)
; On a V86 fault the CPU nulls DS/ES/FS/GS; the monitor reads guest memory + the
; real-mode IVT through the null DS (base 0 == flat) and its own data through FS.
;
; All four GSW modes expose at least the 386 ISA. The guest-facing XMS/EMS/UMB
; entry points keep a 16-bit ABI and use KB units internally. The 24 MB map
; keeps each KB count under 0x6000. They pass 32-bit arguments to INT 0xC0
; monitor services through driver-resident scratch dwords read through FS.
cpu 386
org 0

    dd 0xFFFFFFFF                 ; dh_next
    dw 0x8000                     ; dh_attr = char device
    dw strategy
    dw interrupt
    db 'EMMXXXX0'                 ; char-device name: all-in-one EMM386-class
                                  ; manager. LIM EMS detection compares these 8
                                  ; bytes at [IVT67-seg:000A]; XMS detection is
                                  ; INT 2Fh AX=4300 and doesn't read the name.

rh_ptr:  dd 0                     ; saved ES:BX (request header)
drv_seg: dw 0                     ; our load segment (CS)
base_lin: dd 0                    ; CS << 4
pd_lin:  dd 0                     ; page directory linear (page-aligned)

; Saved real-mode kernel context at INIT entry (for the return-to-V86 seam).
k_ss: dw 0
k_sp: dw 0
k_ds: dw 0
k_es: dw 0
k_fs: dw 0
k_gs: dw 0
k_cs: dw 0                        ; EXECRH far-return CS
k_ip: dw 0                        ; EXECRH far-return IP

vif: db 1                         ; virtual IF (guest's view; DOS boots with IF=1)
va20: db 1                        ; virtual A20 (guest's view). The REAL gate is
                                  ; forced on at INIT and never drops under V86:
                                  ; the monitor and the paged UMB/EMS backing
                                  ; live above 1 MB and a real A20-off would fold
                                  ; them onto low RAM (DOS=HIGH,UMB corruption).
                                  ; Port 0x92 is trapped via the TSS I/O bitmap
                                  ; and the guest's A20 becomes a paging illusion
                                  ; over the 1 MB..1 MB+64K window — the EMM386
                                  ; approach. (INT 15h AH=24xx / 8042 A20 paths
                                  ; are not virtualized; XMS+port 0x92 is what
                                  ; FreeDOS and period software use.)
align 2
vip: dw 0                         ; pending IRQ lines held while VIF=0 (bit N =
                                  ; line N, master 0-7 + slave 8-15)

; ---- XMS state (resident; reached via cs: overrides from V86) ----
old_2f:   dd 0                     ; previous INT 2Fh vector (chain target)
xms_pool_base: dd 0               ; first byte available to XMS EMBs
xms_pool_end:  dd 0               ; one past the dedicated XMS EMB pool
xms_category_kb: dw 0             ; combined extended category for Toka-DOS MEM
hma_available: db 0               ; fixed HMA lies inside detected physical RAM
hma_owned: db 0                   ; 1 once a guest (DOS=HIGH) claims the HMA
a20_count: dw 0                   ; XMS local-A20 enable nesting (fns 05h/06h)
xms_disp:  dw 0                   ; dispatch scratch (register-safe table jump)
xms_mv_len: dd 0                  ; 0Bh move: byte count / src linear / dst linear
xms_mv_src: dd 0                  ; (the INT 0xC0 'TM' memcpy reads these three
xms_mv_dst: dd 0                  ;  via FS; the 16-bit V86 ABI has no
                                  ;  32-bit registers to pass them in)
xms_slot_save: dw 0               ; 0Fh resize: keep the slot across find_gap (clobbers SI)
xms_rv_off: dd 0                  ; resolve input: the endpoint's 32-bit offset
xms_need_kb: dw 0                 ; arena_alloc input: KB wanted
xms_need_gran: dw 0               ; 1 KB granules reserved for that request
                                  ; (== requested KB; a 1 KB request costs 1 KB)

; 32 EMB handles. handle h (1-based) -> slot at xms_table + (h-1)*XMS_SLOT.
; slot: +0 inuse(b) +1 lock(b) +2 size_kb(w) +4 base_kb(w) +6 granules(w). Bases
; are 1 KB-aligned; linear = base_kb << 10.
XMS_HANDLES equ 32
XMS_SLOT    equ 8
xms_table: times XMS_HANDLES*XMS_SLOT db 0

; The free UMB window is 0xC8000-0xEFFFF, above the VGA BIOS and below
; system ROM), 160 KB, page-mapped at INIT to extended RAM just above the HMA. The
; guest allocator (XMS 10h/11h/12h) hands out segment runs in [0xC800, umb_win_end)
; The window ends at 0xF000, or 0xE000 when the EMS page frame is enabled.
UMB_LIN_BASE  equ 0x000C8000      ; first upper-hole linear byte
UMB_BYTES     equ 0x00028000      ; 160 KB (0xC8000..0xEFFFF)
UMB_PHYS_BASE equ 0x00110000      ; backing physical (just above the HMA)
UMB_SEG_BASE  equ 0x0C800         ; first UMB paragraph (segment); the window
                                  ; ends at the runtime umb_win_end
umb_available: db 0               ; backing fits inside detected physical RAM
; UMB sub-blocks handed out by 10h. slot: +0 inuse(b) +1 pad +2 seg(w) +4 paras(w)
UMB_SLOTS equ 8
UMB_SLOT  equ 6
umb_table: times UMB_SLOTS*UMB_SLOT db 0

; ---- EMS state (resident; reached via cs: overrides from V86) ----
; EMS pages are 16 KB and come from the SHARED arena on demand, 16 granules at a
; time (386MAX ALLOC_LIM @ALLOC_EMS). There is no EMS partition: the ceiling is
; whatever the arena holds. `EMS_MAX_PAGES` is the bitmap's own ceiling, not a
; reservation -- it sizes ems_link and nothing else.
EMS_PAGE_GRANULES equ 16
EMS_MAX_PAGES equ ARENA_GRANULES / EMS_PAGE_GRANULES   ; 1456
EMS_FRAME_SEG equ 0xE000          ; page frame segment (4 slots x 16 KB)
EMS_FRAME_LIN equ 0x000E0000
EMS_HANDLES   equ 32
; handle slot: +0 inuse(b) +1 saved(b) +2 npages(w) +4 first(w) +6 pad(w)
;              +8 saved_map(4w) +16 cache_logical(w) +18 cache_backing(w).
; Backing runs are NOT contiguous: +4 is the head of this handle's page chain in
; ems_link, and logical page L is L links along it. The cache at +16/+18 is the
; last resolved (logical, backing) pair, so a repeat map is O(1) and a forward
; sequential sweep of an L-page handle costs O(L) in total rather than O(L^2).
; +16 stores cache_logical+1 so the table's raw zeroed cold state (0) already
; reads as cold without an INIT-time sentinel fill -- storing a bare 0xFFFF
; sentinel would leave the zeroed table claiming logical page 0 is backed by
; EMS page 0, which is some OTHER handle's memory, until the first ef_alloc
; touched the slot (D4).
EMS_SLOT      equ 20
ems_on:      db 1                 ; 1 unless the command line contains NOEMS
ems_pages:   dw 0                 ; total 16 KB pages the pool spans
ems_category_kb: dw 0             ; private F0 query, pages converted to KB
ems_disp:    dw 0                 ; dispatch scratch (mirrors xms_disp)
umb_win_end: dw 0xF000            ; UMB window end segment (0xE000 with EMS on)
ems_table: times EMS_HANDLES*EMS_SLOT db 0
; live frame map: backing page index per physical slot, 0xFFFF = unmapped
ems_frame_map: dw 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF
; INT 0xC0 'PM' remap args (monitor reads via FS): slot linear base + backing
; physical base (0 = restore the INIT mapping). Separate from the xms_mv_*
; scratch so an ISR-driven remap can't race a move being staged.
ems_rm_lin:  dd 0
ems_rm_phys: dd 0
; Per-EMS-page chain link: the next page in the same handle's chain, 0xFFFF at
; the end. 386MAX threads its 16 KB pages exactly this way (PPAGELINK/PL_NEXT in
; QMAX_I67.ASM). Only entries for allocated pages are meaningful -- the arena
; bitmap, not a free chain, is what says whether a page is available (D6: see
; ems_page_alloc/ems_page_free below for why this branch keeps no free list of
; its own).
ems_link: times EMS_MAX_PAGES dw 0xFFFF

; ---- ONE extended-memory arena, shared by XMS EMBs, EMS pages and VCPI.
;
; 386MAX's model (QMAX_VMM.ASM): a single allocation bitmap over the managed
; span -- its XMSBMAP, "each byte contains the XMS allocation and boundary
; information about the corresponding 1KB block" -- and ONE allocator whose only
; per-interface difference is the boundary a block must start on (its ALLOC_LIM
; table: XMS 1 KB, VCPI 4 KB, EMS 16 KB). One bit per granule suffices here
; because this machine models no physical discontiguity and no Windows 3.0
; boundary workaround.
;
; The granule is 1 KB, so a granule index IS a kilobyte offset from the arena
; base -- which is why an XMS handle can go on storing an absolute base_kb.
;
; This replaced a 4 KB-page bitmap plus a hand-maintained `xms_free` counter.
; The counter is gone deliberately: every free count is now DERIVED from the
; bitmap by arena_query, and a derived count cannot drift from the allocation
; state the way a decremented one can. A free count that could not move is
; precisely what broke DOS/4GW -- DOS/16M probes for pool sharing by taking all
; of XMS and re-reading the other interfaces (see the design note).
;
; Indices are POOL-RELATIVE, so the bitmap is sized for the pool and not for the
; address space. BT/BTS/BTR take the full bit-string index, which the 386 treats
; as SIGNED for a memory operand; ARENA_GRANULES stays well under 32767.
ARENA_GRANULES  equ 23296         ; 22.75 MB ceiling, in 1 KB granules
ARENA_BMP_BYTES equ ARENA_GRANULES / 8      ; 2912
ARENA_PAGES     equ ARENA_GRANULES / 4      ; 4 KB pages over the same span

; 386MAX ALLOC_LIM (QMAX_VMM.ASM:116), stored as boundary-1 so the value serves
; as both the round-up addend and, complemented, the round-down mask. The
; symbols are BYTE OFFSETS into the table, so a caller passes one in BX.
ALLOC_XMS  equ 0
ALLOC_VCPI equ 2
ALLOC_EMS  equ 4
alloc_lim: dw 1-1, 4-1, 16-1

arena_base_kb:   dw 0             ; first arena kilobyte, absolute
arena_granules:  dw 0             ; live granule count (<= ARENA_GRANULES)
vcpi_cursor:     dw 0             ; next-fit 4 KB-page cursor, pool-relative
ems_cursor:      dw 0             ; next-fit 16 KB-page cursor, pool-relative
                                   ; (ems_page_alloc's own scan; see there)
arena_q_type:    db 0             ; INT 0xC0 'TQ' argument: an ALLOC_* offset
arena_q_largest: dw 0             ; ... and its two answers, in granules
arena_q_total:   dw 0
; Query memoization (D1). arena_query32 costs ~5-6 instructions per granule
; and ARENA_GRANULES is ~23,000, so an uncached walk is ~110,000-140,000
; monitor instructions.
;
; An earlier version of this comment justified the memo with a tick-loss
; story: a walk this long, held under an interrupt guard, could outlast one
; 54.9 ms IRQ0 tick and lose it to `vip`'s one-bit coalescing. That does not
; hold up under a second look. Each AH=42h/DE03/etc. call closes its own
; VIF=0 window with its own iret, so polling never accumulates one long
; window out of many short ones, and a single ~140k-instruction walk of
; bt/inc/cmp/jmp would need each instruction to average roughly 104 cycles at
; 266 MHz to fill 54.9 ms -- about 60x more than those instructions actually
; cost. (The coalescing hazard itself is real -- one-bit IRR, the PIT
; forwarder's own comment, and a regression test that has caught it all
; confirm that -- it just is not what this particular walk triggers.)
;
; The real cost is throughput, not fidelity: a program polling AH=42h once
; per frame was measured burning about 6% of guest instruction throughput on
; the walk alone. EMS AH=42h is a LIM function DOS programs call freely, so a
; plain per-call walk was a real regression there, just not the one the
; comment used to claim.
;
; Also closed off, so it does not get re-proposed later: an incrementally
; maintained running total cannot replace the walk. `total` is per free SPAN,
; rounded DOWN to the caller's type boundary before being summed (see
; arena_query32's header comment), exactly as span-structure-dependent as
; `largest` is, and degenerates to a plain running sum only for ALLOC_XMS,
; whose 1 KB boundary can never leave a sub-granule remainder to round away.
; A "just bump a counter on alloc/free" version would silently misreport
; VCPI/EMS totals the moment the arena fragments.
;
; Fix: one generation counter, bumped by every bitmap mutator (arena_mark,
; arena_release, vcpi_page_alloc, vcpi_page_free), and one cached (largest,
; total) pair per allocation type (index = ALLOC_*/2), stamped with the
; generation it was computed at. arena_query32 returns the cached pair when its
; type's stamp matches the live generation, and only re-walks the bitmap on a
; miss. The cache is still entirely DERIVED from the bitmap -- a miss recomputes
; it from scratch -- so it cannot drift the way the retired hand-maintained
; xms_free/ems_free counters could. A dword generation makes wraparound
; (needing 2^32 bitmap mutations between two queries of the same type) a
; non-concern; a word counter could theoretically alias within one long session.
;
; The stamps start at -1, NOT 0, for the same reason ems_backing_of's cache
; stores cache_logical+1 (D4): a cache's cold state has to be wrong-looking, not
; plausible. arena_gen starts at 0 and INIT never mutates the arena (it only
; sizes it), so stamps of 0 would make the very first query of each type a
; cache HIT on the never-written arena_qc_largest/arena_qc_total, answering
; "zero free" without ever walking the bitmap. That is invisible on a normal
; boot only because this build's shell allocates an XMS swap block before
; anything queries; a CONFIG.SYS driver loaded after TOKAEMM that sizes itself
; from XMS 08h would be told the machine has no extended memory. -1 cannot
; collide until the generation wraps, which is the 2^32 argument above.
arena_gen:        dd 0             ; bumped by every arena_bmp/vcpi_bmp mutation
arena_qc_gen:     dd -1, -1, -1    ; per-type cache stamp (index = ALLOC_*/2);
                                   ; -1 = never computed, see above
arena_qc_largest: dw 0, 0, 0       ; ... and its cached answers, in granules
arena_qc_total:   dw 0, 0, 0
vcpi_pic_master: dw 8             ; DE0Ah/DE0Bh: current 8259 vector bases
vcpi_pic_slave:  dw 0x70          ; (the DOS-default mapping until a client
                                  ;  records its own)

arena_bmp: times ARENA_BMP_BYTES db 0   ; ALLOCATED bit per 1 KB granule
; VCPI OWNERSHIP bit per 4 KB page. arena_bmp says "allocated"; this says
; "allocated BY VCPI". With one address range serving all three interfaces the
; range alone no longer identifies an owner, so without this DE05 would happily
; free an XMS block's or an EMS page's memory out from under it.
vcpi_bmp: times ARENA_PAGES / 8 db 0

strategy:
    mov [cs:rh_ptr], bx
    mov [cs:rh_ptr+2], es
    retf

; ---- device interrupt entry. Real mode; ES:BX = request header (saved). ----
interrupt:
    cli
    ; Snapshot the real-mode kernel context FIRST (before anything perturbs it),
    ; via CS overrides so the kernel segment registers survive untouched.
    mov [cs:k_ss], ss
    mov [cs:k_sp], sp
    mov [cs:k_ds], ds
    mov [cs:k_es], es
    mov [cs:k_fs], fs
    mov [cs:k_gs], gs
    push bp
    mov bp, sp
    mov ax, [ss:bp+2]             ; EXECRH far-return IP (original [sp])
    mov [cs:k_ip], ax
    mov ax, [ss:bp+4]             ; EXECRH far-return CS
    mov [cs:k_cs], ax
    pop bp

    push cs
    pop ds                        ; DS = CS for our own data
    les bx, [rh_ptr]              ; request header -> ES:BX
    cmp byte [es:bx+2], 0         ; command 0 = INIT?
    je init
    ; Any non-INIT command (possibly reached in V86 later): just report done.
    mov word [es:bx+3], 0x0100    ; r_status = S_DONE
    sti
    retf

init:
    ; Parse the DEVICE= tail for a whole-token "NOEMS" argument.
    ; r_bpbptr (+18) points at the raw command line, driver path first
    ; (FreeDOS init_device). Bare and RAM both leave EMS enabled, drawing pages
    ; from the shared arena on demand rather than a fixed-size pool; NOEMS wins
    ; regardless of token order because no other token sets it back.
    push ds
    lds si, [es:bx+18]
.p_path:                          ; skip the path token
    lodsb
    call cls_al                   ; -> AH: 0 ordinary, 1 separator, 2 line end
    cmp ah, 0
    je .p_path
    cmp ah, 2
    je .p_done
.p_gap:                           ; skip separators to the next token start
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap
    cmp ah, 2
    je .p_done
    and al, 0xDF                  ; token first char, upcased
    cmp al, 'N'
    jne .p_skiptok
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap                     ; token was just "N"
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'O'
    jne .p_skiptok
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'E'
    jne .p_skiptok
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'M'
    jne .p_skiptok
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'S'
    jne .p_skiptok
    lodsb                         ; the char after "NOEMS" must end the token
    call cls_al
    cmp ah, 0
    je .p_skiptok                 ; longer token (for example NOEMSX)
    mov byte [cs:ems_on], 0
    cmp ah, 1
    je .p_gap
    jmp .p_done
.p_skiptok:                       ; consume the rest of the current token
    lodsb
    call cls_al
    cmp ah, 0
    je .p_skiptok
    cmp ah, 1
    je .p_gap
.p_done:
    pop ds

    ; Signon banner. INT 29h works during device INIT, when INT 21h AH=09h is unreliable.
    mov si, banner
.bl:
    lodsb                         ; DS = CS here
    test al, al
    jz .bdone
    int 0x29
    jmp .bl
.bdone:

    ; Discover physical RAM before entering V86. E801 reports KB between 1 and
    ; 16 MB plus 64 KB blocks above 16 MB. Some BIOSes use AX/BX and others use
    ; CX/DX, so accept the first nonzero pair. AH=88h is the fallback.
    mov ax, 0xE801
    int 0x15
    jc .mem_88
    mov si, ax
    mov di, bx
    mov ax, si
    or ax, di
    jnz .mem_e801
    mov si, cx
    mov di, dx
.mem_e801:
    movzx eax, si                 ; KB from 1 MB through 16 MB
    movzx ebx, di                 ; 64 KB blocks above 16 MB
    shl eax, 10
    shl ebx, 16
    add eax, ebx
    jnz .mem_got_ext
.mem_88:
    mov ah, 0x88
    int 0x15
    jc .mem_none
    movzx eax, ax
    shl eax, 10
    jmp .mem_got_ext
.mem_none:
    xor eax, eax
.mem_got_ext:
    add eax, 0x00100000           ; convert extended bytes to physical top
    cmp eax, 0x01800000           ; this monitor maps at most 24 MB
    jbe .mem_top_ok
    mov eax, 0x01800000
.mem_top_ok:
    and eax, 0xFFFFF000
    mov edi, eax                  ; EDI = detected/capped physical top

    ; Fixed low extended-memory services are available only when their complete
    ; physical backing fits. A small machine still gets the V86 monitor, but no
    ; service advertises memory beyond the detected top.
    cmp edi, 0x00110000
    jb .no_hma
    mov byte [cs:hma_available], 1
.no_hma:
    cmp edi, UMB_PHYS_BASE + UMB_BYTES
    jb .no_umb
    mov byte [cs:umb_available], 1
    mov eax, UMB_PHYS_BASE + UMB_BYTES
    jmp .arena_base
.no_umb:
    mov word [cs:umb_win_end], UMB_SEG_BASE
    mov eax, edi                  ; empty arena when the UMB backing cannot fit
.arena_base:
    ; Keep the monitor's seven paging pages out of conventional memory when
    ; extended RAM has room.  The .SYS retains a low fallback tail for the
    ; 1 MiB profile, but normal machines reserve these aligned pages before
    ; the allocatable XMS/VCPI arena instead.
    mov edx, eax
    add edx, 0x7000
    jc .low_tables
    cmp edx, edi
    ja .low_tables
    mov [cs:pd_lin], eax
    mov eax, edx
    mov cx, resident_core_end
    jmp .tables_selected
.low_tables:
    mov cx, resident_image_end
.tables_selected:
    mov [cs:xms_pool_base], eax

    ; BIOS calls above may clobber ES:BX.  Reload the saved INIT request and
    ; report only the low core when the page tables were reserved high.
    les bx, [cs:rh_ptr]
    mov [es:bx+14], cx
    mov word [es:bx+16], cs
    mov word [es:bx+3], 0x0100    ; r_status = S_DONE

    ; The arena always reaches the RAM top now: EMS draws from it on demand
    ; instead of owning a partition above it. EMS enablement only decides
    ; whether the page frame exists, and so whether the UMB window stops at
    ; 0xE000 or runs to 0xF000.
    mov ebx, edi
    cmp byte [cs:ems_on], 0
    je .ems_layout_done
    cmp byte [cs:umb_available], 0
    jne .ems_frame_on
    mov byte [cs:ems_on], 0
    jmp .ems_layout_done
.ems_frame_on:
    mov word [cs:umb_win_end], EMS_FRAME_SEG
.ems_layout_done:
    mov [cs:xms_pool_end], ebx
    mov eax, ebx
    sub eax, 0x00100000
    jnc .xms_category_ok
    xor eax, eax
.xms_category_ok:
    shr eax, 10
    mov [cs:xms_category_kb], ax

    ; Page-align the allocatable category and hand ALL of it to the one shared
    ; arena: XMS EMBs, EMS pages and VCPI pages all allocate out of it, first
    ; come first served. Clamped to what arena_bmp can index (ARENA_GRANULES) so
    ; a machine larger than the bitmap cannot walk off the end of it.
    mov eax, [cs:xms_pool_base]
    add eax, 0xFFF
    and eax, 0xFFFFF000
    mov ebx, [cs:xms_pool_end]
    and ebx, 0xFFFFF000
    cmp eax, ebx
    jbe .arena_bounds_ok
    mov eax, ebx
.arena_bounds_ok:
    ; Clamp to the span the granule bitmap covers. ARENA_PAGES * 0x1000 ==
    ; ARENA_GRANULES * 0x400, so this byte ceiling is still correct now that
    ; arena_bmp is indexed in 1 KB granules rather than 4 KB pages.
    cmp ebx, ARENA_PAGES * 0x1000
    jbe .arena_ceiling_ok
    mov ebx, ARENA_PAGES * 0x1000
.arena_ceiling_ok:
    cmp eax, ebx                  ; a base above the ceiling yields an empty pool
    jbe .arena_span_ok
    mov eax, ebx
.arena_span_ok:
    mov [cs:xms_pool_base], eax
    mov [cs:xms_pool_end], ebx
    mov ecx, ebx
    sub ecx, eax
    shr ecx, 10                   ; span in 1 KB granules
    cmp ecx, ARENA_GRANULES       ; never index past the bitmap
    jbe .arena_span_fits
    mov ecx, ARENA_GRANULES
.arena_span_fits:
    mov [cs:arena_granules], cx
    mov edx, eax
    shr edx, 10
    mov [cs:arena_base_kb], dx    ; arena base, absolute KB
    mov word [cs:vcpi_cursor], 0
    mov word [cs:ems_cursor], 0

    ; EMS totals are DERIVED from the arena, not carved out of it. Free pages
    ; are computed per call by arena_query; only the total is fixed at INIT.
    xor ax, ax
    cmp byte [cs:ems_on], 0
    je .ems_total_done
    mov ax, cx                    ; CX = arena granules
    shr ax, 4                     ; -> whole 16 KB pages
.ems_total_done:
    mov [cs:ems_pages], ax
    shl ax, 4                     ; pages * 16 KB -> category KB
    mov [cs:ems_category_kb], ax

    ; Hook INT 2Fh (chain) + own INT 67h outright (IVT at linear 0). The EMS
    ; manager answers in BOTH modes: frameless is EMM386-NOEMS's contract.
    push ds
    xor ax, ax
    mov ds, ax
    mov eax, [ds:0x2F*4]
    mov [cs:old_2f], eax
    mov word [ds:0x2F*4], xms_2f_handler
    mov [ds:0x2F*4+2], cs
    mov word [ds:0x67*4], ems_int67
    mov [ds:0x67*4+2], cs
    pop ds

    mov [drv_seg], cs
    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [base_lin], eax           ; base = CS<<4

    cmp dword [pd_lin], 0
    jne .pd_ready
    add eax, tables               ; pd_lin = page-align(base + tables)
    add eax, 0xFFF
    and eax, 0xFFFFF000
    mov [pd_lin], eax
.pd_ready:

    mov eax, [base_lin]           ; code selector (0x08) base = base
    mov [gdt + 0x08 + 2], ax
    shr eax, 16
    mov [gdt + 0x08 + 4], al
    mov [gdt + 0x08 + 7], ah

    mov eax, [base_lin]           ; FS data selector (0x20) base = base
    mov [gdt + 0x20 + 2], ax
    shr eax, 16
    mov [gdt + 0x20 + 4], al
    mov [gdt + 0x20 + 7], ah

    mov eax, [base_lin]           ; TSS descriptor (0x18) base = base + tss
    add eax, tss
    mov [gdt + 0x18 + 2], ax
    shr eax, 16
    mov [gdt + 0x18 + 4], al
    mov [gdt + 0x18 + 7], ah

    mov eax, [base_lin]           ; gdtr base = base + gdt
    add eax, gdt
    mov [gdtr + 2], eax

    mov eax, [base_lin]           ; idtr base = base + idt
    add eax, idt
    mov [idtr + 2], eax

    push es                       ; zero the TSS + I/O bitmap (ES = header seg here)
    push di
    push cs
    pop es                        ; ES = our segment so STOSW targets our TSS
    mov di, tss
    mov cx, 0x2070 / 2
    xor ax, ax
    rep stosw
    mov byte [tss + 0x68 + 0x2000], 0xFF  ; the Intel bitmap terminator byte
    pop di
    pop es
    mov eax, [base_lin]           ; ESP0 = monitor stack top in driver memory
    add eax, mon_stack_top
    mov [tss + 4], eax
    mov ebx, eax                  ; carry monitor ESP into PM (survives PT build)
    mov word  [tss + 8], 0x0010   ; SS0 = flat data selector
    mov word  [tss + 0x66], 0x0068 ; I/O-map base (all-zero bitmap = permissive)
    ; Trap port 0x92 so the monitor virtualizes the guest's A20 (the
    ; only bit set in the otherwise-permissive map), and force the REAL gate on
    ; for good — the monitor + the paged UMB/EMS backing sit above 1 MB.
    or byte [tss + 0x68 + (0x92/8)], 1 << (0x92 % 8)
    in al, 0x92
    or al, 2
    out 0x92, al

    mov ebp, [pd_lin]             ; carry pd_lin + drv_seg into PM
    movzx esi, word [drv_seg]

    lgdt [gdtr]
    lidt [idtr]
    mov eax, cr0
    or eax, 1                     ; PE
    mov cr0, eax
    jmp dword 0x08:pm_init        ; code sel base = base -> linear base+pm_init

%ifndef TOKAEMM_SOURCE_DIR
    %strlen TOKAEMM_SOURCE_PATH_LEN __FILE__
    %substr TOKAEMM_SOURCE_DIR __FILE__ 1, TOKAEMM_SOURCE_PATH_LEN-11
%endif
%strcat TOKAEMM_XMS_INC TOKAEMM_SOURCE_DIR, "tokaemm-xms.inc"
%include TOKAEMM_XMS_INC

; ============================================================================
; Guest EMS (INT 67h, LIM 4.0 subset; V86 code, cs: overrides).
; Hooked at INIT; apps find the manager by comparing "EMMXXXX0" at
; [IVT67-seg:000A] = our device-header name. Status in AH (0 = OK); registers
; other than documented outputs are preserved. Functions outside the
; implemented set return 84h like a real manager that lacks them.
; ============================================================================
ems_int67:
    cmp ah, 0x40
    jb ef_undef
    cmp ah, 0x4D
    ja ef_undef
    push bx
    mov bl, ah                    ; zero-extend AH through the 16-bit ABI
    xor bh, bh
    sub bx, 0x40
    add bx, bx
    mov bx, [cs:ems_jt + bx]
    mov [cs:ems_disp], bx
    pop bx
    jmp [cs:ems_disp]
ems_jt:
    dw ef_status, ef_frame, ef_counts, ef_alloc     ; 40h-43h
    dw ef_map, ef_free, ef_version, ef_save         ; 44h-47h
    dw ef_restore, ef_undef, ef_undef, ef_count     ; 48h-4Bh (49/4A reserved)
    dw ef_pages, ef_all_pages                       ; 4Ch-4Dh

ef_undef:
    mov ah, 0x84                  ; undefined function
    iret
ef_status:                        ; 40h get manager status
    xor ah, ah
    iret
ef_frame:                         ; 41h get page-frame segment -> BX
    cmp byte [cs:ems_on], 0
    je .noframe
    mov bx, EMS_FRAME_SEG
    xor ah, ah
    iret
.noframe:
    xor bx, bx
    mov ah, 0x80                  ; frameless: EMM386-NOEMS convention
    iret
; 42h get page counts: BX=free, DX=total. Free is DERIVED from the shared
; arena every call; total was fixed at INIT. No interrupt guard: INT 67h
; delivery already clears VIF before any handler runs (including this one),
; so no guest ISR can interleave with the arena_query call below the way one
; could interleave with a far-called XMS entry point -- xf_query_free's cli
; guards against that case specifically, and does not apply here. A cache HIT
; in arena_query costs a handful of compares and never enters the monitor at
; all, so the frameless check runs FIRST and skips the query outright: no
; reason to pay even that when the answer is always zero.
ef_counts:
    cmp byte [cs:ems_on], 0
    jne .on
    xor bx, bx                     ; frameless: no EMS pages are obtainable
    mov dx, [cs:ems_pages]         ; (0 in this mode; see INIT's EMS-totals step)
    xor ah, ah
    iret
.on:
    push ax                        ; AL is not a defined output of 42h;
                                    ; arena_query clobbers it as its
                                    ; largest-free-run answer, which 42h
                                    ; has no field for
    mov bl, ALLOC_EMS
    call arena_query               ; DX = total free granules for 16 KB blocks
    shr dx, 4                      ; granules -> 16 KB pages
    mov bx, dx
    mov dx, [cs:ems_pages]
    pop ax
    xor ah, ah
    iret
ef_version:                       ; 46h get version -> AL = BCD 4.0
    mov al, 0x40
    xor ah, ah
    iret

; 43h allocate: BX = pages -> DX = handle. Pages come one at a time from the
; shared arena and are linked in logical order (386MAX ALLOCEMS), so backing is
; non-contiguous by construction.
;
; D3: the free total is checked BEFORE any page is taken. A fresh arena_query
; is cheap once D1's memo is warm, so there is no reason to pay for a
; speculative take-then-unwind on the common failure path -- a caller probing
; "how much can I get" by halving no longer pays for the answer twice. Once the
; pre-check passes, the take loop below cannot fail (the pre-check already
; proved that many boundary-aligned runs exist somewhere in the arena, and each
; successful ems_page_alloc call removes exactly one); .unwind stays as a
; safety net against a future accounting bug, not as the live failure path.
;
; D2: matches the discipline the routine this replaces used -- AX and DX are
; pushed (AX is clobbered by arena_query/ems_page_alloc and is not a defined
; output; DX is a working counter/handle and only a defined output on
; success). On every exit, AX is popped back (not discarded) and DX is either
; the new handle (success) or restored to the caller's original value (every
; failure path) -- LIM says only the documented outputs change.
ef_alloc:
    test bx, bx
    jz .zero
    cmp bx, [cs:ems_pages]
    ja .total
    push ax
    push dx
    push si
    push cx
    push di
    push bp
    push bx                       ; BX must survive as "pages wanted" through
                                   ; the pre-check below. arena_query itself
                                   ; PRESERVES bx; the save is needed because
                                   ; the very next line, loading the type
                                   ; argument into bl, is what destroys it
    mov bl, ALLOC_EMS
    call arena_query               ; DX = total free granules for 16 KB blocks
    shr dx, 4                      ; granules -> 16 KB pages
    pop bx
    cmp bx, dx
    ja .nofree                    ; D3: reject before taking a single page
    mov si, ems_table             ; claim a handle slot BEFORE taking pages, so
    mov cx, EMS_HANDLES           ; the common failure needs no unwind
    xor dx, dx                    ; handle counter (1-based below)
.slot:
    inc dx
    cmp byte [cs:si], 0
    je .got
    add si, EMS_SLOT
    loop .slot
    pop bp
    pop di
    pop cx
    pop si
    pop dx                        ; restore the caller's DX: the slot scan ran over it
    pop ax
    mov ah, 0x85                  ; no more handles
    iret
.got:
    mov word [cs:si+4], 0xFFFF    ; empty chain
    mov di, 0xFFFF                ; DI = chain tail, 0xFFFF = none yet
    mov bp, bx                    ; BP = pages still wanted; BX keeps npages
.take:
    call ems_page_alloc           ; -> AX = page index. The pre-check above
    jc .unwind                    ; makes this a safety net, not the live path.
    push bx
    mov bx, ax
    add bx, bx
    mov word [cs:ems_link + bx], 0xFFFF   ; new tail terminates the chain
    pop bx
    cmp di, 0xFFFF
    je .head
    push bx
    mov bx, di
    add bx, bx
    mov [cs:ems_link + bx], ax    ; link the old tail to it
    pop bx
    jmp .linked
.head:
    mov [cs:si+4], ax
.linked:
    mov di, ax
    dec bp
    jnz .take
    mov byte [cs:si], 1           ; inuse
    mov byte [cs:si+1], 0         ; saved = 0
    mov [cs:si+2], bx             ; npages
    mov word [cs:si+16], 0        ; cold cache (0 = cold; ems_backing_of stores
                                   ; cache_logical+1, so the raw table's zeroed
                                   ; cold state and an explicit reset agree)
    pop bp
    pop di
    pop cx
    pop si
    add sp, 2                     ; discard the saved DX: DX carries the handle
    pop ax
    xor ah, ah
    iret
.unwind:
    mov di, [cs:si+4]             ; give back every page we managed to take
.uw:
    cmp di, 0xFFFF
    je .uw_done
    mov ax, di
    push bx
    mov bx, di
    add bx, bx
    mov di, [cs:ems_link + bx]
    pop bx
    call ems_page_free
    jmp .uw
.uw_done:
    mov word [cs:si+4], 0xFFFF
.nofree:
    pop bp
    pop di
    pop cx
    pop si
    pop dx
    pop ax
    mov ah, 0x88                  ; insufficient free pages
    iret
.total:
    mov ah, 0x87                  ; more than the manager's total
    iret
.zero:
    mov ah, 0x89                  ; zero pages
    iret

; 44h map: AL = physical slot 0-3, BX = logical page (0xFFFF unmaps),
; DX = handle. The bookkeeping is here; the PTE rewrite + TLB flush is the
; monitor's INT 0xC0 'PM' service (ring-0 work, like the XMS-move memcpy).
;
; Backing is a singly-linked forward chain (ems_link), not an array and not a
; doubly-linked list: random access would want the latter, but a "prev" word
; per page is another 2,912 bytes on top of ems_link's own 2,912, against the
; 16 bytes of headroom left under the 64 KB driver ceiling (D5) -- the
; structure is forced, not chosen for its own sake. ems_backing_of's per-slot
; (logical, backing) cache is what keeps the common access patterns (forward
; sequential sweep, double-buffering, ping-pong paging) cheap despite that: a
; forward walk resumes from the cache instead of restarting, so those cost
; O(L) in total rather than O(L^2); only a walk to a lower logical page than
; the cache remembers restarts from the head.
ef_map:
    cmp al, 3
    ja .badphys
    push si
    push cx
    call ems_slot_of              ; DX -> SI, or CF + AH=0x83 (LIM: the unmap
    jc .bad                       ; form still requires a valid handle)
    cmp bx, 0xFFFF
    je .unmap
    cmp bx, [cs:si+2]             ; logical >= npages?
    jae .badlog
    call ems_backing_of           ; logical BX, slot SI -> CX = backing page
.do:
    mov si, ax                    ; slot index: AL validated <= 3, mask AH away
    and si, 3
    add si, si
    mov [cs:ems_frame_map + si], cx
    call ems_remap_slot           ; AL=slot, CX=page|0xFFFF (preserves regs)
    pop cx
    pop si
    xor ah, ah
    iret
.unmap:
    mov cx, 0xFFFF
    jmp .do
.badlog:
    mov ah, 0x8A                  ; logical page out of range
.bad:
    pop cx
    pop si
    iret
.badphys:
    mov ah, 0x8B                  ; physical page out of range
    iret

; 45h release: DX = handle. Walks the handle's page chain (backing is no
; longer contiguous, so there is no [first,first+npages) range to reason
; about): unmaps any live frame slot showing that page, scrubs it from every
; saved_map by exact match (a freed-and-reassigned page must not be reinstated
; by a later 48h restore -- mirrors the retired HLE's invalidate_freed), and
; returns it to the shared arena, one page at a time.
ef_free:
    push si
    call ems_slot_of
    jc .badh
    push ax
    push bx
    push cx
    push dx
    push di
    mov di, [cs:si+4]             ; walk this handle's chain
.page:
    cmp di, 0xFFFF
    je .pages_done
    xor bx, bx                    ; unmap any live frame slot showing this page
.slots:
    push si
    mov si, bx
    add si, si
    cmp [cs:ems_frame_map + si], di
    jne .ns
    mov word [cs:ems_frame_map + si], 0xFFFF
    mov al, bl
    mov cx, 0xFFFF
    call ems_remap_slot           ; restore the INIT mapping
.ns:
    pop si
    inc bx
    cmp bx, 4
    jb .slots
    push si                       ; scrub this page from every saved_map, so a
    mov si, ems_table             ; later 48h restore cannot reinstate a page
    mov cx, EMS_HANDLES           ; that has been freed and reassigned
.scrub:
    cmp byte [cs:si+1], 0         ; saved?
    je .nh
    push cx
    push si
    add si, 8                     ; saved_map
    mov cx, 4
.sm:
    cmp [cs:si], di
    jne .smn
    mov word [cs:si], 0xFFFF
.smn:
    add si, 2
    loop .sm
    pop si
    pop cx
.nh:
    add si, EMS_SLOT
    loop .scrub
    pop si
    mov ax, di                    ; release the page and step to the next
    push bx
    mov bx, di
    add bx, bx
    mov di, [cs:ems_link + bx]
    pop bx
    call ems_page_free
    jmp .page
.pages_done:
    mov word [cs:si+4], 0xFFFF
    mov word [cs:si+16], 0        ; cold cache (0 = cold; see ems_backing_of)
    mov byte [cs:si], 0
    mov byte [cs:si+1], 0
    pop di
    pop dx
    pop cx
    pop bx
    pop ax
    pop si
    xor ah, ah
    iret
.badh:
    pop si
    iret                          ; AH = 0x83 from ems_slot_of

; 47h save / 48h restore the frame map under DX = handle.
ef_save:
    push si
    call ems_slot_of
    jc .badh
    cmp byte [cs:si+1], 0
    jne .already
    push ax
    push cx
    push di
    mov di, 4                     ; four slots
    xor cx, cx                    ; word offset 0,2,4,6
.cp:
    push si
    mov si, cx
    mov ax, [cs:ems_frame_map + si]
    pop si
    push si
    add si, cx
    mov [cs:si+8], ax
    pop si
    add cx, 2
    dec di
    jnz .cp
    mov byte [cs:si+1], 1
    pop di
    pop cx
    pop ax
    pop si
    xor ah, ah
    iret
.already:
    pop si
    mov ah, 0x8D                  ; context already saved
    iret
.badh:
    pop si
    iret

ef_restore:
    push si
    call ems_slot_of
    jc .badh
    cmp byte [cs:si+1], 0
    je .none
    push ax
    push bx
    push cx
    push di
    xor bx, bx                    ; BX = physical slot 0..3
.rs:
    mov di, bx
    add di, di
    push si
    add si, di
    mov cx, [cs:si+8]             ; saved word (page or 0xFFFF)
    pop si
    push si
    mov si, di
    mov [cs:ems_frame_map + si], cx
    pop si
    mov al, bl
    call ems_remap_slot           ; maps or restores per CX
    inc bx
    cmp bx, 4
    jb .rs
    mov byte [cs:si+1], 0
    pop di
    pop cx
    pop bx
    pop ax
    pop si
    xor ah, ah
    iret
.none:
    pop si
    mov ah, 0x8E                  ; no saved context
    iret
.badh:
    pop si
    iret

; 4Bh open-handle count -> BX. 4Ch handle pages: DX = handle -> BX.
ef_count:
    push si
    push cx
    xor bx, bx
    mov si, ems_table
    mov cx, EMS_HANDLES
.c:
    cmp byte [cs:si], 0
    je .n
    inc bx
.n:
    add si, EMS_SLOT
    loop .c
    pop cx
    pop si
    xor ah, ah
    iret
ef_pages:
    push si
    call ems_slot_of
    jc .badh
    mov bx, [cs:si+2]
    pop si
    xor ah, ah
    iret
.badh:
    pop si
    iret

; 4Dh get pages for every open handle. ES:DI receives {handle,pages} word
; pairs and BX receives the number written. This is the LIM enumeration MEM
; uses after 4Bh; empty slots are omitted.
ef_all_pages:
    push ax
    push cx
    push dx
    push si
    push di
    xor bx, bx
    xor dx, dx                    ; one-based handle number
    mov si, ems_table
    mov cx, EMS_HANDLES
.scan:
    inc dx
    cmp byte [cs:si], 0
    je .next
    mov [es:di], dx
    mov ax, [cs:si+2]
    mov [es:di+2], ax
    add di, 4
    inc bx
.next:
    add si, EMS_SLOT
    loop .scan
    pop di
    pop si
    pop dx
    pop cx
    pop ax
    xor ah, ah
    iret

; --- EMS helpers --------------------------------------------------------------

; DX = EMS handle -> SI = slot offset, CF clear; or CF set + AH = 0x83.
; Callers save SI. Preserves everything else. Handle 0 (the LIM OS handle) is
; reserved-not-modeled, so it answers 83h like an unknown handle.
ems_slot_of:
    cmp dx, 1
    jb .bad
    cmp dx, EMS_HANDLES
    ja .bad
    push ax
    mov ax, dx
    dec ax
    imul si, ax, EMS_SLOT         ; one instruction, 386-only, no register to
                                   ; save: (handle-1) * EMS_SLOT fits SI cleanly
    add si, ems_table
    pop ax
    cmp byte [cs:si], 0           ; inuse?
    je .bad
    clc
    ret
.bad:
    mov ah, 0x83                  ; invalid handle
    stc
    ret

; Take one 16 KB EMS page from the shared arena. out: AX = EMS page index,
; CF clear; or CF set when none is free. EMS page p occupies arena granules
; [p*16, p*16+16), so its absolute kilobyte is arena_base_kb + p*16.
; Preserves BX/CX/DX/SI/DI.
;
; D6: no EMS-private free chain (386MAX's PPAGELINK). A free list here would
; need to stay synchronised with grabs XMS or VCPI make out of the SAME
; bitmap, and neither of those interfaces has any reason to know or care that
; a granule range it just took happened to be "EMS page N" -- the whole point
; of one shared arena is that any of the three can take any part of it. Only
; probing the live bits directly cannot go stale that way.
;
; That does not mean no cursor, though, and an earlier version of this
; comment got that wrong. `arena_alloc` (used here originally) restarts its
; scan from granule 0 on every call, because XMS requests are variably sized
; and there is no fixed unit to remember a position for; EMS pages are always
; the same size, so a next-fit cursor applies exactly the way it already does
; for VCPI's `vcpi_cursor`. Its own scan below, `ems_cursor`-driven, mirrors
; the monitor's vcpi_page_alloc. Without it, taking N pages one at a time from
; an empty arena cost O(N^2) bit tests (each call rescanning past every
; already-taken low page): a single AH=43h asking for most of the pool -- a
; RAM disk or cache claiming all of EMS at once does exactly this -- could
; hold VIF=0 for multiple 54.9 ms IRQ0 ticks at N in the high hundreds. That
; is the real version of the hazard D1 addressed for the query path; this is
; where it actually lived, in the allocator, one guest instruction (AH=43h),
; not spread across many polls.
ems_page_alloc:
    push bx
    push cx
    push dx
    push si
    mov cx, [cs:arena_granules]
    shr cx, 4                     ; whole 16 KB pages the arena covers
    jz .none
    mov ax, [cs:ems_cursor]
    mov dx, cx                    ; candidate pages left to examine
.scan:
    cmp ax, cx
    jb .test
    xor ax, ax                    ; wrap to the arena base
.test:
    mov si, ax
    shl si, 4                     ; first granule of this candidate page
    mov bx, EMS_PAGE_GRANULES
.probe:
    bt word [cs:arena_bmp], si
    jc .next
    inc si
    dec bx
    jnz .probe
    jmp .take
.next:
    inc ax
    dec dx
    jnz .scan
.none:
    pop si
    pop dx
    pop cx
    pop bx
    stc
    ret
.take:
    mov si, ax                    ; SI = page index; recompute its first
    shl si, 4                     ; granule (the probe above left SI at the
    push ax                       ; page's end, not its start)
    mov ax, si
    mov cx, EMS_PAGE_GRANULES
    call arena_mark                ; marks the 16 granules, bumps arena_gen
    pop ax
    mov si, ax                    ; next-fit cursor = page index + 1. AX
    inc si                        ; cannot be a 16-bit addressing base (only
                                   ; BX/BP/SI/DI can), so no LEA shortcut here
                                   ; the way the 32-bit VCPI side has one.
    mov [cs:ems_cursor], si       ; next-fit: resume just past this page
    pop si
    pop dx
    pop cx
    pop bx
    clc
    ret

; Return one 16 KB EMS page to the shared arena. in: AX = EMS page index.
; Preserves every register.
ems_page_free:
    push ax
    push cx
    shl ax, 4                     ; EMS page index -> granule index
    mov cx, EMS_PAGE_GRANULES
    call arena_release
    pop cx
    pop ax
    ret

; Logical page BX of the handle at SI -> CX = backing EMS page index. Walks the
; handle's chain, resuming from the slot's (logical, backing) cache whenever the
; cache is at or before the wanted logical page. Preserves AX/BX/DX/SI/DI/BP.
;
; The cache at [si+16]/[si+18] stores cache_logical+1 (0 = cold) rather than a
; bare logical index with an 0xFFFF sentinel (D4): a raw-zeroed ems_table slot
; then already reads as cold, with no INIT-time sentinel fill needed, and an
; explicit reset (ef_alloc, ef_free) just stores 0 instead of a magic value.
ems_backing_of:
    push ax
    mov cx, [cs:si+4]             ; chain head
    xor ax, ax                    ; logical index CX currently stands at
    cmp word [cs:si+16], 0
    je .walk                      ; cold cache
    mov ax, [cs:si+16]
    dec ax                        ; decode: stored value is cache_logical+1
    cmp ax, bx
    ja .fromhead                  ; cache is past us: restart from the head
    mov cx, [cs:si+18]
    jmp .walk
.fromhead:
    xor ax, ax
    mov cx, [cs:si+4]
.walk:
    cmp ax, bx
    je .done
    push bx
    mov bx, cx
    add bx, bx
    mov cx, [cs:ems_link + bx]
    pop bx
    inc ax
    jmp .walk
.done:
    inc ax                        ; encode: cache_logical+1, 0 stays "cold"
    mov [cs:si+16], ax
    mov [cs:si+18], cx
    pop ax
    ret

; Monitor remap of one frame slot. AL = slot 0-3, CX = backing page index or
; 0xFFFF to restore the INIT (UMB-backing) mapping. Preserves all registers.
; The two 32-bit args (slot linear base, backing physical base) are staged in
; [cs:ems_rm_*] as word pairs; the monitor reads them via FS.
ems_remap_slot:
    push eax
    push dx
    mov dx, ax
    and dx, 3                     ; slot linear = EMS_FRAME_LIN + slot*16K:
    shl dx, 14                    ; low word = slot << 14,
    mov [cs:ems_rm_lin], dx
    mov word [cs:ems_rm_lin+2], EMS_FRAME_LIN >> 16   ; high word = 0x000E
    cmp cx, 0xFFFF
    je .unmap
    movzx eax, cx                 ; backing = arena base + page * 16 KB
    shl eax, 14
    push edx
    movzx edx, word [cs:arena_base_kb]
    shl edx, 10
    add eax, edx
    pop edx
    mov [cs:ems_rm_phys], eax
    jmp .go
.unmap:
    mov word [cs:ems_rm_phys], 0  ; 0 = restore the INIT mapping
    mov word [cs:ems_rm_phys+2], 0
.go:
    mov dx, 0x4D50                ; 'PM' monitor-call cookie
    int 0xC0
    pop dx
    pop eax
    ret

; Classify AL for the INIT command-line parse: AH = 0 ordinary char,
; 1 separator (space/tab), 2 line end (CR/LF/NUL). Preserves AL.
cls_al:
    cmp al, ' '
    je .sep
    cmp al, 9
    je .sep
    cmp al, 0x0D
    je .end
    cmp al, 0x0A
    je .end
    test al, al
    jz .end
    xor ah, ah
    ret
.sep:
    mov ah, 1
    ret
.end:
    mov ah, 2
    ret

align 8
gdt:
    dq 0
    dq 0x00CF9B000000FFFF         ; [08] code, base patched
    dq 0x00CF93000000FFFF         ; [10] data, base 0 (flat)
    dq 0x0000890000002068         ; [18] TSS, base patched, limit 0x2068: the
                                  ; I/O bitmap at +0x68 covers the FULL 64K port
                                  ; space (a port past the limit is DENIED, and
                                  ; V86 guests hit sound/VGA ports >= 0x100)
    dq 0x00CF93000000FFFF         ; [20] data, base patched (= base, driver data)
gdtr:
    dw 0x27                       ; 5 descriptors
    dd 0

; IDT (static gates; offsets are driver-relative, selector = PM code 0x08;
; base patched at runtime). The default boot runs the whole system in
; V86, so every device IRQ the machine can raise needs a gate — master IRQ0-7 on
; vectors 8-15 (the DOS PIC base) and slave IRQ8-15 on 0x70-0x77. Vector 13 is
; BOTH #GP and IRQ5 (SB16): vec13_entry disambiguates. The exception overlaps on
; 8/10-12/14 (#DF/#TS/#NP/#SS/#PF) have no source here: identity-mapped
; always-present pages and no PM selector loads from V86.
%macro IDTGATE 1
    dw %1, 0x0008                 ; offset-low, PM code selector (driver < 64K)
    db 0, 0x8E                    ; present, ring-0 32-bit interrupt gate
    dw 0                          ; offset-high
%endmacro
align 8
; After the PRM-correct load_flags IOPL gate, a V86 guest
; that legitimately raises its live IOPL to 3 (Watcom-compiled Toka-DOS kernel/
; EMM glue does this during MEM runs) stops trapping CLI/STI/PUSHF/POPF/INT/
; IRET as sensitive ops (check_v86_iopl correctly waves them through, matching
; real silicon) -- so a bare `INT n` now dispatches through THIS static IDT
; directly, at ring 0, instead of through vec13_entry's opcode-peek emulation.
; Every previously-null slot below was safe only because IOPL was pinned at 0;
; it no longer is. A null gate's selector field is 0x0000: deliver_exception's
; final `load_segment(bus, Cs, 0)` raises a fatal CpuError::GeneralProtection
; that unwinds out of the emulator entirely, not a re-entrant #GP the monitor
; can catch. So every slot must hold a real gate now. deflt_N/deflt_common give
; every currently-null vector the same reflect_vector treatment exc_de/exc_ud/
; exc_nm already use: bounce it to the guest's own real-mode IVT handler,
; matching how real hardware would have serviced a software INT anyway.
idt:
    IDTGATE exc_de                ; 0    #DE divide error -> reflect to IVT[0]
    IDTGATE deflt_1               ; 1    #DB/INT1 (debug; no resident debugger)
    IDTGATE deflt_2               ; 2    NMI (never raised by this emulator)
    IDTGATE deflt_3               ; 3    INT3 (breakpoint opcode / software int)
    IDTGATE deflt_4               ; 4    #OF (INTO overflow trap)
    IDTGATE deflt_5               ; 5    #BR (BOUND range exceeded)
    IDTGATE exc_ud                ; 6    #UD invalid opcode -> reflect to IVT[6]
    IDTGATE exc_nm                ; 7    #NM no-FPU trap -> reflect to IVT[7]
    IDTGATE irq_m0                ; 8    IRQ0 timer
    IDTGATE irq_m1                ; 9    IRQ1 keyboard
    IDTGATE irq_m2                ; 10   IRQ2 cascade (never raw; stub for safety)
    IDTGATE irq_m3                ; 11   IRQ3 COM2
    IDTGATE irq_m4                ; 12   IRQ4 COM1
    IDTGATE vec13_entry           ; 13   #GP monitor OR IRQ5 (SB16)
    IDTGATE irq_m6                ; 14   IRQ6 FDC
    IDTGATE irq_m7                ; 15   IRQ7 LPT / PIC-spurious
    IDTGATE deflt_16              ; 16   #MF x87 FPU error (no FPU emulated here;
                                  ;      exc_nm already reflects the no-FPU trap
                                  ;      at vector 7, so this is belt-and-braces
                                  ;      for a bare `INT 16h` at IOPL=3)
    IDTGATE deflt_ac              ; 17   #AC alignment check (error-code frame;
                                  ;      AM/CR0.AM is never set here, so this
                                  ;      emulator can't raise it, but a bare
                                  ;      `INT 17h` from V86 at IOPL=3 dispatches
                                  ;      through this same slot with NO error
                                  ;      code -- deflt_ac must handle both shapes)
%assign v 18
%rep (0x67 - 18)
    IDTGATE deflt_%[v]            ; 18..0x66: no dedicated handler (mostly the
                                  ;           BIOS/DOS software-INT range) ->
                                  ;           reflect to the guest's own IVT
%assign v v+1
%endrep
    IDTGATE int67_entry           ; 0x67 EMS/VCPI: reached directly only when
                                  ;      the guest runs V86 at IOPL=3 (else the
                                  ;      INT is IOPL-sensitive and arrives via
                                  ;      vec13_entry). AH=DEh -> the monitor's
                                  ;      VCPI server; else reflect like deflt.
%assign v 0x68
%rep (0x70 - 0x68)
    IDTGATE deflt_%[v]            ; 0x68..0x6F: reflect to the guest's own IVT
%assign v v+1
%endrep
    IDTGATE irq_s8                ; 0x70 IRQ8  RTC
    IDTGATE irq_s9                ; 0x71 IRQ9
    IDTGATE irq_s10               ; 0x72 IRQ10
    IDTGATE irq_s11               ; 0x73 IRQ11
    IDTGATE irq_s12               ; 0x74 IRQ12 PS/2 mouse
    IDTGATE irq_s13               ; 0x75 IRQ13
    IDTGATE irq_s14               ; 0x76 IRQ14 ATA
    IDTGATE irq_s15               ; 0x77 IRQ15 / slave-spurious
%assign v 0x78
%rep (0xC0 - 0x78)
    IDTGATE deflt_%[v]            ; 0x78..0xBF: the rest of the software-INT
                                  ;             space (DOS 0x20-0x2E, EMS 0x67,
                                  ;             multiplex 0x2F, user vectors) ->
                                  ;             reflect to the guest's own IVT
%assign v v+1
%endrep
    IDTGATE intc0_entry           ; 0xC0: TOKAEMM-private monitor calls
%assign v 0xC1
%rep (0x100 - 0xC1)
    IDTGATE deflt_%[v]            ; 0xC1..0xFF: the rest of the software-INT
                                  ;             space -> reflect to the guest's
                                  ;             own IVT
%assign v v+1
%endrep
idt_end:
idtr:
    dw idt_end - idt - 1
    dd 0

; Ring-0 monitor from here down: exempt from the guest ISA gate (it services
; the V86 guest from ring-0 protected mode at any GSW level), so full 386.
cpu 386
bits 32
pm_init:                          ; EBP=pd_lin, ESI=drv_seg, EBX=monitor ESP0
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, ebx                  ; monitor ring-0 stack (driver-resident)
    mov edi, ebp                  ; high RAM is not guaranteed to start clear
    xor eax, eax
    mov ecx, 0x7000 / 4
    rep stosd
    ; PD[0..5] -> the six PTs that follow the PD (each PT maps 4 MiB), so the
    ; identity map covers 0..24 MiB and the XMS-move memcpy can reach every EMB.
    lea eax, [ebp + 0x1000]       ; first PT linear = PD + 0x1000
    or eax, 7
    mov edi, ebp                  ; write PD entries
    mov ecx, 6
.pde:
    mov [edi], eax
    add eax, 0x1000               ; next PT is one page further
    add edi, 4
    loop .pde
    lea edi, [ebp + 0x1000]       ; 6144 entries (0..24 MiB), present/rw/user
    mov eax, 7
    mov ecx, 6144
.pt:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .pt
    ; Page the free upper window 0xC8000-0xEFFFF to extended RAM (the
    ; EMM386 trick). On real hardware these holes have no RAM; a UMB there must be
    ; extended RAM mapped in. (This emulator's flat array also backs phys 0xC8000 via
    ; read_phys's fallback, so identity would work too -- but mapping proper extended
    ; RAM is faithful and keeps the UMB accounted against extended memory, not phantom
    ; RAM.) ROM/video PTEs stay identity; only these 40 move.
    mov edx, esi
    shl edx, 4
    cmp byte [edx + umb_available], 0
    je .umb_map_done
    lea edi, [ebp + 0x1000 + (UMB_LIN_BASE >> 12) * 4]  ; PT0 entry for 0xC8000
    mov eax, UMB_PHYS_BASE | 7                          ; backing base, present/rw/user
    mov ecx, UMB_BYTES >> 12                            ; 40 pages
.umb_map:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .umb_map
.umb_map_done:
    mov cr3, ebp
    mov eax, cr0
    or eax, 0x80000000            ; paging on
    mov cr0, eax
    mov ax, 0x18
    ltr ax

    ; Return the running kernel into V86 at the EXECRH post-INIT code. The frame
    ; is the saved real-mode context; ESP = saved SP + 4 (past the far-return
    ; address the kernel's `call far` pushed). EXECRH then runs `sti; cld; pop
    ; ds; pop si; pop bp; ret 8` in V86 and DOS finishes booting virtualized.
    mov ax, 0x20
    mov fs, ax
    movzx eax, word [fs:k_gs]
    push eax
    movzx eax, word [fs:k_fs]
    push eax
    movzx eax, word [fs:k_ds]
    push eax
    movzx eax, word [fs:k_es]
    push eax
    movzx eax, word [fs:k_ss]
    push eax
    movzx eax, word [fs:k_sp]
    add eax, 4
    push eax
    push dword 0x00020202         ; EFLAGS: VM | IF(real) | bit1, IOPL 0
    movzx eax, word [fs:k_cs]
    push eax
    movzx eax, word [fs:k_ip]
    push eax
    iretd

; ============================================================================
; Ring-0 monitor. Entered from V86 through the IDT. deliver_exception has
; nulled DS/ES/FS/GS and switched to the driver-resident ring-0 stack; the
; guest's general registers are LIVE (the CPU saves none), so every handler
; brackets its work with pushad/popad. EBP points at the frame's saved EIP:
;   [ebp+0]=EIP [ebp+4]=CS [ebp+8]=EFLAGS [ebp+12]=V86 ESP [ebp+16]=V86 SS ...
; ============================================================================

; ---- vector 13: #GP (sensitive instruction, error-code frame) OR IRQ5 (the
; SB16, no error code). V86 trap tax Part 2, the three-layer discriminator:
;
; LAYER 1 (frame shape, I/O-free, airtight one way): every #GP this emulator's
; deliver_exception can ever deliver on vector 13 pushes error code EXACTLY 0
; (grep-confirmed: every InternalFault::Exception{vector:13,..} raise site in
; izarravm-cpu passes error_code: Some(0) -- check_v86_iopl,
; check_io_permission, require_cpl0, the WRMSR/RDMSR/RDTSC/MOV-CRn/SYSRET
; privilege checks, all of them; deliver_exception pins this with a
; debug_assert), and deliver_exception never pushes an error code for an
; external interrupt (is_external=true), the ONLY way IRQ5 reaches this
; vector. So the slot at [esp+32] holds the #GP's error code (always 0) or
; the IRQ frame's interrupted EIP. NONZERO slot -> can only be IRQ5. Done.
;
; LAYER 2 (opcode peek, I/O-free, the hot #GP case): slot == 0 is
; overwhelmingly a genuine #GP -- but NOT always: an IRQ5 can interrupt the
; guest at IP == 0, which is cheaply reachable (a handler entered at
; seg:0000, a .COM ret to PSP:0000), not a freak event. So peek the byte at
; the frame's CS:IP: one of the sensitive set monitor_body emulates
; {CLI,STI,PUSHF,POPF,INT n,IRET, and the trapped-port IN/OUT forms} -> take
; the emulate path. Every real sensitive-instruction trap (the ~100k-700k/s
; hot case) resolves here with NO port I/O at all.
;
; LAYER 3 (PIC probe, cold only): slot == 0 AND a non-sensitive byte at
; CS:IP. Either a garbage/unhandled #GP (diagnostic-bound) or an IRQ5 that
; landed on IP == 0 -- indistinguishable without asking the PIC, so ask the
; PIC: OCW3 read of the master ISR, exactly the old scheme, but now only on
; this cold path (and with the ring-0 port exemption it no longer even ends
; the CPU batch). IRQ5 in service -> .irq5; else fall through to
; monitor_body's own dispatch, whose catch-all (`signal32`) is the same
; diagnostic ending the old scheme had.
;
; Residual (same as the OLD scheme's documented double-coincidence, no
; regression): an IRQ5 at IP == 0 whose CS:0 byte happens to BE sensitive
; (~10/256 of byte space) is mis-emulated against the IRQ frame; the line
; stays un-EOI'd. Accepted then, accepted now.
;
; EMULATOR-CONTRACT NOTE: layer 1 is airtight because WE control both frame
; builders (deliver_exception's error-code-vs-external gating). It is not a
; real-hardware-portable trick -- real silicon never routes a #GP and an IRQ
; through the same vector in the first place (the vector-13 collision is this
; emulator's PIC-base-arithmetic artifact). Revisit if deliver_exception's
; push order or the is_external gating ever changes; the debug_assert there
; is the tripwire.
vec13_entry:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    cmp dword [esp+32], 0         ; LAYER 1: #GP error code (0) vs IRQ frame EIP
    jne .irq5                     ; nonzero -> can only be IRQ5
    movzx eax, word [esp+40]      ; LAYER 2: peek the frame CS:IP byte
    shl eax, 4
    movzx ecx, word [esp+36]
    add eax, ecx
    mov dl, [eax+1]               ; second byte too (the 0x66-prefix forms)
    mov al, [eax]                 ; the would-be faulting opcode
    cmp al, 0xFA                  ; CLI
    je monitor_body
    cmp al, 0xFB                  ; STI
    je monitor_body
    cmp al, 0x9C                  ; PUSHF
    je monitor_body
    cmp al, 0x9D                  ; POPF
    je monitor_body
    cmp al, 0xCD                  ; INT n
    je monitor_body
    cmp al, 0xCF                  ; IRET
    je monitor_body
    cmp al, 0xE6                  ; OUT imm8, AL (trapped port 0x92)
    je monitor_body
    cmp al, 0xEE                  ; OUT DX, AL
    je monitor_body
    cmp al, 0xE4                  ; IN AL, imm8
    je monitor_body
    cmp al, 0xEC                  ; IN AL, DX
    je monitor_body
    cmp al, 0xF4                  ; HLT (privileged since the CPL check landed;
    je monitor_body               ; a V86 task is always CPL 3)
    cmp al, 0x66                  ; operand-size prefix: PUSHFD/POPFD/IRETD are
    jne .layer3                   ; IOPL-sensitive in V86 exactly like the 16-bit
    cmp dl, 0x9C                  ; forms (CWSDPMI's mode-switch path uses them)
    je monitor_body
    cmp dl, 0x9D
    je monitor_body
    cmp dl, 0xCF
    je monitor_body
.layer3:
    mov al, 0x0B                  ; LAYER 3 (cold): OCW3, next master data
    out 0x20, al                  ; read = ISR
    in al, 0x20
    test al, 0x20                 ; IRQ5 in service?
    jz monitor_body               ; no -> a plain (unhandled) #GP, diagnose
.irq5:
    mov ebx, 5
    jmp irq_body                  ; no-error-code frame path

; ---- #GP monitor body: a sensitive instruction faulted. Error-code frame;
; entered from vec13_entry with pushad done and DS/FS loaded. ----
monitor_body:
    lea ebp, [esp + 32 + 4]       ; skip pushad(32) + error code(4)
    movzx eax, word [ebp+4]       ; guest CS
    shl eax, 4
    movzx ebx, word [ebp]         ; guest IP
    add eax, ebx                  ; eax = linear addr of the faulting opcode
    movzx edx, byte [eax]
    cmp dl, 0xFA
    je .cli
    cmp dl, 0xFB
    je .sti
    cmp dl, 0x9C
    je .pushf
    cmp dl, 0x9D
    je .popf
    cmp dl, 0xCD
    je .intn
    cmp dl, 0xCF
    je .iret_op
    cmp dl, 0xE6                  ; OUT imm8, AL — the trapped port 0x92 (A20)
    je .out92_imm
    cmp dl, 0xEE                  ; OUT DX, AL
    je .out92_dx
    cmp dl, 0xE4                  ; IN AL, imm8
    je .in92_imm
    cmp dl, 0xEC                  ; IN AL, DX
    je .in92_dx
    cmp dl, 0xF4                  ; HLT
    je .hlt
    cmp dl, 0x66                  ; operand-size prefix: the 32-bit flag/stack forms
    je .prefix66
    cmp dl, 0x0F                  ; two-byte privileged op (MOV CRn/CLTS/LMSW)
    je .prefix0f
    ; Unhandled #GP: reflect INT 0Dh to the guest IVT, the real-monitor
    ; convention (386MAX INT0D reflects vector 13; JEMM V86_Exc0D reflects
    ; INT 06h by default and vector 13 under its V86EXC0D build option --
    ; vector 13 is the literal #GP semantics and what DOS16M's hooked INT
    ; 0Dh handler expects).
    ; Real programs depend on it: DOS16M (the DOS4G loader) hooks INT 0Dh,
    ; executes privileged instructions like LGDT during early preparation,
    ; and handles its own fault reflections -- on real hardware under any
    ; V86 monitor that is normal operation, not a monitor bug. Fault
    ; semantics: the frame IP still points AT the instruction.
    mov ebx, 13
    call reflect_vector
    jmp .done_gp

; ---- 66-prefixed sensitive forms. PUSHFD/POPFD/IRETD are IOPL-sensitive in
; V86 exactly like their 16-bit forms; CWSDPMI's V86 mode-switch path uses
; them. The prefix byte is at [eax], the opcode at [eax+1]. Anything else
; 66-prefixed stays a diagnostic exit, with AL = the second byte (not 0x66)
; so the next gap names itself. ----
.prefix66:
    mov dl, [eax+1]
    cmp dl, 0x9C
    je .pushfd
    cmp dl, 0x9D
    je .popfd
    cmp dl, 0xCF
    je .iretd_op
    mov ebx, 13                   ; unhandled 66-prefixed op: reflect INT 0Dh
    call reflect_vector           ; like the unprefixed catch-all (DOS16M's
    jmp .done_gp                  ; o32 LGDT prep lands here); frame IP still
                                  ; points at the 66 byte, fault semantics
.pushfd:
    ; 32-bit image: frame EFLAGS with IF := VIF and, per the PRM, VM and RF
    ; cleared in the STORED image (the frame's own VM bit stays set). The
    ; image carries VIRTUAL IOPL = 3: the reference monitors expose IOPL 3
    ; to their V86 tenants (JEMM runs clients at real IOPL 3; 386MAX
    ; virtualizes the sensitive set the same way), and the VCPI spec S4.0
    ; requires the IOPL-sensitive instructions be "available". Extenders
    ; PROBE this: DOS16M reads the flags image to classify real-mode vs
    ; V86-under-a-monitor, and an IOPL-0 image sent it down its raw
    ; LGDT mode-switch path (fatal under any monitor). The real IOPL
    ; stays 0 -- vif virtualization is unchanged; nothing at CPL 3 can
    ; architecturally change IOPL, so the constant image is faithful.
    mov eax, [ebp+8]
    and eax, 0xFFFCFDFF           ; clear IF + VM(17) + RF(16) in the image
    or eax, 0x3000                ; virtual IOPL = 3
    cmp byte [fs:vif], 0
    je .pfd_store
    or eax, 0x0200
.pfd_store:
    mov ebx, [ebp+16]             ; guest SS
    shl ebx, 4
    sub word [ebp+12], 4          ; guest SP -= 4
    movzx ecx, word [ebp+12]
    mov [ebx+ecx], eax
    add word [ebp], 2             ; skip 66 9C
    jmp .done_gp
.popfd:
    mov ebx, [ebp+16]             ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]
    mov eax, [ebx+ecx]            ; popped EFLAGS dword
    add word [ebp+12], 4
    test ax, 0x0200               ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    and ax, 0xCFFF                ; monitor frame stays IOPL 0 (same as .popf)
    or ax, 0x0200                 ; frame keeps real IF = 1
    mov word [ebp+8], ax          ; low word only: the frame's high word (VM=1)
                                  ; is preserved, matching hardware POPFD in V86
                                  ; (VM/RF/IOPL-class bits unchanged)
    add word [ebp], 2             ; skip 66 9D
    call maybe_deliver
    jmp .done_gp
.iretd_op:
    mov ebx, [ebp+16]             ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]
    mov eax, [ebx+ecx]            ; pop EIP (V86 IP is 16-bit; high half dropped)
    mov word [ebp], ax
    mov eax, [ebx+ecx+4]          ; pop CS
    mov word [ebp+4], ax
    mov eax, [ebx+ecx+8]          ; pop EFLAGS
    add word [ebp+12], 12
    test ax, 0x0200               ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    and ax, 0xCFFF                ; monitor frame stays IOPL 0 (same as .iret_op)
    or ax, 0x0200                 ; frame keeps real IF = 1
    mov word [ebp+8], ax          ; low word only; frame VM=1 preserved
    call maybe_deliver
    jmp .done_gp

; ---- virtualized port 0x92: the guest's A20 gate. Only 0x92 is set in the
; I/O bitmap, so any other port reaching here is a monitor bug -> signal. The
; guest AL lives in the pushad frame at [esp+28]; guest DX at [esp+20]. ----
.out92_imm:
    cmp byte [eax+1], 0x92
    jne .unhandled_io
    add word [ebp], 2             ; skip OUT imm8, AL
    jmp .a20_write
.out92_dx:
    cmp word [esp+20], 0x0092     ; guest DX
    jne .unhandled_io
    inc word [ebp]                ; skip OUT DX, AL
.a20_write:
    mov cl, [esp+28]              ; guest AL: bit 1 = A20 (bit 0, fast reset,
    shr cl, 1                     ; is ignored — nothing period pulses it)
    and cl, 1
    cmp [fs:va20], cl
    je .done_gp
    mov [fs:va20], cl
    call a20_apply
    jmp .done_gp
.in92_imm:
    cmp byte [eax+1], 0x92
    jne .unhandled_io
    add word [ebp], 2             ; skip IN AL, imm8
    jmp .a20_read
.in92_dx:
    cmp word [esp+20], 0x0092
    jne .unhandled_io
    inc word [ebp]                ; skip IN AL, DX
.a20_read:
    mov cl, [fs:va20]
    add cl, cl                    ; bit 1 = the virtual A20 state
    mov [esp+28], cl              ; guest AL (byte write: AH.. preserved)
    jmp .done_gp
.unhandled_io:
    mov al, dl
    jmp signal32
.cli:
    mov byte [fs:vif], 0
    inc word [ebp]
    jmp .done_gp
.sti:
    mov byte [fs:vif], 1
    inc word [ebp]
    call maybe_deliver            ; STI may release a pending IRQ
    jmp .done_gp
.pushf:
    mov ax, [ebp+8]               ; frame EFLAGS
    and ax, 0xFDFF                ; IF := VIF for the pushed image
    or ax, 0x3000                 ; virtual IOPL = 3 (see .pushfd: extenders
    cmp byte [fs:vif], 0          ; probe the image to classify the machine)
    je .pf_store
    or ax, 0x0200
.pf_store:
    mov ebx, [ebp+16]            ; guest SS
    shl ebx, 4
    sub word [ebp+12], 2         ; guest SP -= 2
    movzx ecx, word [ebp+12]
    mov [ebx+ecx], ax
    inc word [ebp]               ; PUSHF is 1 byte
    jmp .done_gp
.popf:
    mov ebx, [ebp+16]           ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]    ; guest SP
    mov ax, [ebx+ecx]           ; popped flags
    add word [ebp+12], 2
    test ax, 0x0200             ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    and ax, 0xCFFF               ; keep the monitor's frame at IOPL 0 (bits 12-13):
                                  ; the ring-0 IRETD back into V86 restores IOPL
                                  ; from this frame verbatim (CPL 0, full PRM
                                  ; restore), so a guest-popped IOPL=3 here would
                                  ; escape vif virtualization for good
    or ax, 0x0200               ; frame keeps real IF = 1
    mov word [ebp+8], ax        ; update guest flags (VM in high word preserved)
    inc word [ebp]              ; POPF is 1 byte
    call maybe_deliver          ; POPF may re-enable interrupts
    jmp .done_gp
.intn:
    movzx ebx, byte [eax+1]      ; INT vector operand
    cmp bl, 0x67                 ; INT 67h: EMS and VCPI share the vector.
    jne .intn_not67              ; AH=DEh is the monitor-side VCPI server;
    cmp byte [esp+29], 0xDE      ; anything else reflects to the guest EMS
    jne .intn_reflect            ; driver as before. Guest AH = pushad EAX+1.
    add word [ebp], 2            ; return IP = past INT 67h (fault frame
    mov esi, esp                 ; points AT the instruction on this path)
    call vcpi_dispatch           ; ESI = pushad base, EBP = &frame.eip
    jmp .done_gp
.intn_not67:
    cmp bl, 0xC0                 ; TOKAEMM-private monitor call?
    jne .intn_reflect
    cmp word [esp+20], 0x544D    ; guest DX == 'TM' (XMS-move memcpy)?
    je .intn_memcpy
    cmp word [esp+20], 0x4D50    ; guest DX == 'PM' (EMS frame remap)?
    je .intn_remap
    cmp word [esp+20], 0x5154    ; guest DX == 'TQ' (arena free query)?
    je .intn_query
    jmp .intn_reflect            ; foreign INT 0xC0: reflect like any other
.intn_memcpy:
    add word [ebp], 2            ; skip past INT 0xC0
    call flat_memcpy
    jmp .done_gp
.intn_remap:
    add word [ebp], 2
    call frame_remap
    jmp .done_gp
.intn_query:
    add word [ebp], 2
    mov bl, [fs:arena_q_type]
    call arena_query32
    mov [fs:arena_q_largest], ax
    mov [fs:arena_q_total], dx
    jmp .done_gp
.intn_reflect:
    add word [ebp], 2            ; return IP = past INT n
    call reflect_vector
    jmp .done_gp
.iret_op:
    mov ebx, [ebp+16]           ; guest SS
    shl ebx, 4
    movzx ecx, word [ebp+12]    ; guest SP
    mov ax, [ebx+ecx]           ; pop IP
    mov word [ebp], ax
    add word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov ax, [ebx+ecx]           ; pop CS
    mov word [ebp+4], ax
    add word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov ax, [ebx+ecx]           ; pop FLAGS
    add word [ebp+12], 2
    test ax, 0x0200            ; popped IF -> VIF
    setnz cl
    mov [fs:vif], cl
    and ax, 0xCFFF              ; keep the monitor's frame at IOPL 0, same reason
                                 ; as .popf above
    or ax, 0x0200             ; frame keeps real IF = 1
    mov word [ebp+8], ax
    call maybe_deliver         ; IRET may re-enable interrupts
    jmp .done_gp
.hlt:
    inc word [ebp]             ; return IP = past the F4 byte (HLT is 1 byte)
    ; Real HLT is CPL-gated (a V86 task is always CPL 3), so the CPU now #GP(0)s
    ; every guest HLT into this monitor. Give the guest real halt semantics: run
    ; the actual `sti; hlt` at ring 0 so the CPU's own HLT/wake logic idles the
    ; machine, then IRET back to the guest just past the F4 byte. The IDT is the
    ; same table in both V86 and ring 0 (idt/idtr above), so any interrupt that
    ; fires during this real HLT vectors straight into irq_m*/irq_s*/vec13_entry
    ; exactly as it would have for the guest -- VIF=0 holds the line in vip (the
    ; existing irq_body coalesce) and VIF=1 reflects it into the guest's IVT
    ; (irq_reflect_line), same as any other interrupt arriving mid-V86.
    ;
    ; Guest VIF=0 (interrupts virtually disabled): a real 386 hangs forever on
    ; `HLT` with IF=0, woken only by NMI or reset -- NMI is not virtualized here,
    ; so a literal mirror would wedge the whole VM on a guest bug (or on a
    ; legitimate but IF=0 halt-until-NMI idiom this emulator doesn't model).
    ; Decision (documented, not a silent divergence): run the real `hlt` with
    ; real IF left clear, matching the guest's request bit-for-bit; the run
    ; loop's own interrupt-pending wake (service_pending_interrupt) still can't
    ; fire with IF=0, so this blocks until something forces IF, which nothing
    ; here does for a VIF=0 halt. To avoid a permanent guest-visible wedge on
    ; ordinary FreeDOS idle loops (which always halt with IF=1 -- DOS brackets
    ; IRQ-sensitive code with CLI/STI, never idles under CLI), only the VIF=1
    ; path executes a real hlt; a VIF=0 HLT resumes the guest immediately
    ; (equivalent to an instantaneous NMI/no-op wake), since no real game or
    ; DOS idle loop halts with interrupts masked and this monitor has nothing
    ; that will ever clear that state for it otherwise.
    cmp byte [fs:vif], 0
    je .done_gp
    sti
    hlt                         ; wakes when service_pending_interrupt admits a
                                ; real IRQ. This hlt runs at ring 0 (VM=0), so
                                ; irq_body's real-frame check (below) cannot
                                ; treat the waking IRQ's 3-dword IRETD frame as
                                ; a V86 frame; it holds the line in vip and
                                ; EOIs, same as the VIF=0 coalesce path, then
                                ; IRETDs straight back here. Drain it into the
                                ; guest now that we're about to return to V86:
                                ; maybe_deliver reflects the highest-priority
                                ; held line through EBP's real V86 frame.
    call maybe_deliver
    jmp .done_gp

; ---- Two-byte privileged 0F ops (386MAX QMAX_I0D GP_ESCOD, adapted to the
; Izarra 3000). A V86 task is CPL 3, so the CPU #GP(0)s every MOV CRn/DRn,
; CLTS, and LMSW into this monitor; the reference managers EMULATE them
; transparently (386MAX GP_MOV_*/GP_GRP7, JEMM ExtendedOp) rather than
; reflect -- extenders read CR0 through this path during their real-mode-vs-
; V86 probe. Restricted to the 386 privileged set that is both reachable and
; assembles in this cpu-386 monitor region: the 486/586 members 386MAX also
; handles (INVD/WBINVD, RDMSR/WRMSR/RDTSC/RDPMC) and the TRn/EISA/SYSROM/DPMI
; paths are DROPPED -- on a throttled 386 those opcodes #UD at the guest
; level (never reaching here), no proven client executes them in V86, and DR
; state is already reachable through VCPI DE08/DE09. Anything outside the set
; reflects INT 0Dh like the single-byte catch-all.
;
; Guest register i (ModRM rm/reg field) lives in the pushad block at
; [esp + 28 - i*4] (EDI@0..EAX@28). ESP is stable here (no push/pop), and the
; live eax/ebx/ecx/esi used as scratch are all popad-restored, so writing a
; RESULT into the pushad slot is the only guest-visible effect.
.prefix0f:
    movzx ebx, byte [eax+1]       ; the second opcode byte
    cmp bl, 0x06
    je .op_clts
    cmp bl, 0x20
    je .op_mov_r_cr
    cmp bl, 0x22
    je .op_mov_cr_r
    cmp bl, 0x21
    je .op_mov_r_dr
    cmp bl, 0x23
    je .op_mov_dr_r
    cmp bl, 0x01
    je .op_grp7
.op0f_reflect:
    mov ebx, 13                   ; unemulated 0F: reflect like the catch-all
    call reflect_vector
    jmp .done_gp

.op_clts:                         ; CLTS: clear CR0.TS at ring 0 (real)
    clts
    add word [ebp], 2
    jmp .done_gp

; MOV r32, CRn (0F 20 /r). ModRM at [eax+2]: reg = CR#, rm = dest GPR.
.op_mov_r_cr:
    movzx ecx, byte [eax+2]
    mov ebx, ecx
    shr ebx, 3
    and ebx, 7                    ; CR number
    cmp bl, 0
    je .rcr0
    cmp bl, 2
    je .rcr2
    cmp bl, 3
    je .rcr3
    jmp .op0f_reflect             ; CR4+ not modeled at 386: reflect
.rcr0:
    mov esi, cr0
    jmp .rcr_store
.rcr2:
    mov esi, cr2
    jmp .rcr_store
.rcr3:
    mov esi, cr3
.rcr_store:
    and ecx, 7                    ; rm = dest GPR
    movzx edi, cl
    shl edi, 2
    neg edi
    add edi, 28                   ; pushad offset = 28 - rm*4
    mov [esp+edi], esi
    add word [ebp], 3
    jmp .done_gp

; MOV CRn, r32 (0F 22 /r). CR0 write: force PE|PG on (a V86 client can toggle
; EM/TS/MP/NW but must never un-protect or un-page the live machine -- the
; 386MAX INS_MOV_CRn_R32B guard). CR2/CR3 pass through.
.op_mov_cr_r:
    movzx ecx, byte [eax+2]
    mov ebx, ecx
    shr ebx, 3
    and ebx, 7                    ; CR number
    and ecx, 7                    ; rm = source GPR
    movzx edi, cl
    shl edi, 2
    neg edi
    add edi, 28
    mov esi, [esp+edi]            ; the value the guest wants to write
    cmp bl, 0
    je .wcr0
    cmp bl, 2
    je .wcr2
    cmp bl, 3
    je .wcr3
    jmp .op0f_reflect
.wcr0:
    or esi, 0x80000001            ; PG|PE forced on
    mov cr0, esi
    jmp .wcr_done
.wcr2:
    mov cr2, esi
    jmp .wcr_done
.wcr3:
    mov cr3, esi                  ; (reloads the guest's own paging: legal,
                                  ; the client owns its mapping under VCPI)
.wcr_done:
    add word [ebp], 3
    jmp .done_gp

; MOV r32, DRn (0F 21 /r) and MOV DRn, r32 (0F 23 /r). 386 debug registers;
; DR0-3/6/7 (DR4/5 are undefined -- this monitor reflects them; the CPU
; aliases DR4->DR6/DR5->DR7, a harmless divergence no V86 client exercises).
; Read/write real.
.op_mov_r_dr:
    movzx ecx, byte [eax+2]
    mov ebx, ecx
    shr ebx, 3
    and ebx, 7                    ; DR number
    cmp bl, 4
    je .op0f_reflect
    cmp bl, 5
    je .op0f_reflect
    cmp bl, 0
    je .rdr0
    cmp bl, 1
    je .rdr1
    cmp bl, 2
    je .rdr2
    cmp bl, 3
    je .rdr3
    cmp bl, 6
    je .rdr6
    mov esi, dr7
    jmp .rdr_store
.rdr0:
    mov esi, dr0
    jmp .rdr_store
.rdr1:
    mov esi, dr1
    jmp .rdr_store
.rdr2:
    mov esi, dr2
    jmp .rdr_store
.rdr3:
    mov esi, dr3
    jmp .rdr_store
.rdr6:
    mov esi, dr6
.rdr_store:
    and ecx, 7
    movzx edi, cl
    shl edi, 2
    neg edi
    add edi, 28
    mov [esp+edi], esi
    add word [ebp], 3
    jmp .done_gp

.op_mov_dr_r:
    movzx ecx, byte [eax+2]
    mov ebx, ecx
    shr ebx, 3
    and ebx, 7                    ; DR number
    and ecx, 7                    ; rm = source GPR
    movzx edi, cl
    shl edi, 2
    neg edi
    add edi, 28
    mov esi, [esp+edi]
    cmp bl, 4
    je .op0f_reflect
    cmp bl, 5
    je .op0f_reflect
    cmp bl, 0
    je .wdr0
    cmp bl, 1
    je .wdr1
    cmp bl, 2
    je .wdr2
    cmp bl, 3
    je .wdr3
    cmp bl, 6
    je .wdr6
    mov dr7, esi
    jmp .wdr_done
.wdr0:
    mov dr0, esi
    jmp .wdr_done
.wdr1:
    mov dr1, esi
    jmp .wdr_done
.wdr2:
    mov dr2, esi
    jmp .wdr_done
.wdr3:
    mov dr3, esi
    jmp .wdr_done
.wdr6:
    mov dr6, esi
.wdr_done:
    add word [ebp], 3
    jmp .done_gp

; Group 7 (0F 01 /r). Only LMSW (/6) is privileged-and-emulable here; SGDT/
; SIDT/SMSW are unprivileged (never trap), and LGDT/LIDT/INVLPG reflect (a
; real V86 client that loads its own GDT is doing a mode switch -- our DE0C
; is the sanctioned path, and DOS16M-style raw LGDT probes want the fault).
; LMSW: OR PE into the value first (LMSW architecturally cannot clear PE) and
; force it through the low word of CR0 without disturbing the high half.
.op_grp7:
    movzx ecx, byte [eax+2]       ; ModRM
    mov ebx, ecx
    shr ebx, 3
    and ebx, 7                    ; /ext field
    cmp bl, 6                     ; LMSW?
    jne .op0f_reflect
    mov bl, cl
    and bl, 0xC0                  ; mod field: register form only (mod==11)?
    cmp bl, 0xC0
    jne .op0f_reflect             ; memory-form LMSW: reflect (rare, unneeded)
    and ecx, 7                    ; rm = source GPR
    movzx edi, cl
    shl edi, 2
    neg edi
    add edi, 28
    mov si, [esp+edi]             ; the 16-bit MSW image the guest supplies
    or si, 1                      ; PE stays set
    lmsw si
    add word [ebp], 3
    jmp .done_gp
.done_gp:
    popad
    add esp, 4                   ; discard the #GP error code
    iretd

; ---- Hardware IRQs (no error code). Per-line stubs load the 8259 line number
; and share one body: reflect to the guest IVT when VIF is set, else hold the
; line in the vip mask and EOI immediately (coalesce; deliver on the next
; STI/POPF/IRET). Master lines 0-7 (vectors 8-15, 5 via vec13_entry), slave
; lines 8-15 (vectors 0x70-0x77). ----
%assign line 0
%rep 8
irq_m%[line]:
    pushad
    mov ebx, line
    jmp irq_common
%assign line line+1
%endrep
%assign line 8
%rep 8
irq_s%[line]:
    pushad
    mov ebx, line
    jmp irq_common
%assign line line+1
%endrep

irq_common:                       ; pushad done, EBX = IRQ line
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
irq_body:                         ; vec13_entry joins here (segs already set)
    lea ebp, [esp + 32]
    test dword [ebp+8], 0x00020000 ; real EFLAGS VM bit of the INTERRUPTED
    jz .hold                       ; frame: clear means ring 0 (the monitor's
                                    ; own .hlt sti;hlt window is the only place
                                    ; this can happen), so reflect_vector must
                                    ; never run against that 3-dword ring-0
                                    ; IRETD frame (no V86 SS:SP to scribble
                                    ; into), regardless of vif. Hold the line
                                    ; exactly like the VIF=0 coalesce path;
                                    ; .hlt drains it via maybe_deliver once
                                    ; back in V86.
    cmp byte [fs:vif], 0
    jne .go
.hold:
    mov ecx, ebx                  ; hold the line, EOI now so the PIC keeps
    mov ax, 1                     ; delivering
    shl ax, cl
    or [fs:vip], ax
    call irq_eoi
    popad
    iretd
.go:
    call irq_reflect_line
    popad
    iretd

; ---- CPU exceptions raised by V86 guest code (#DE 0, #UD 6, #NM 7 — the
; no-error-code faults a real-mode program can produce). Reflect to the guest
; IVT exactly like a hardware INT: the frame EIP already points at the
; faulting instruction (286+ fault semantics, what DOS-era INT 00h/06h/07h
; handlers expect). No EOI — exceptions have no PIC line. A guest with no
; handler of its own inherits the BIOS IVT default, same as real hardware. ----
exc_de:
    pushad
    mov ebx, 0
    jmp exc_common
exc_ud:
    pushad
    mov ebx, 6
    jmp exc_common
exc_nm:
    pushad
    mov ebx, 7
exc_common:
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    call reflect_vector
    popad
    iretd

; ---- Default gates for every vector this driver has no dedicated handler
; for (1-5, 16, 18-0x6F, 0x78-0xFF -- see the idt: comment above). Before the
; PRM-correct load_flags IOPL fix these slots were unreachable: IOPL was
; pinned at 0, so every guest INT/IRET/PUSHF/POPF trapped as a sensitive
; instruction through vec13_entry's opcode-peek emulation first. A guest that
; legitimately raises its own IOPL to 3 (Watcom-compiled Toka-DOS kernel/EMM
; glue, observed during MEM runs) makes these dispatch for real, straight
; through this IDT. Reflect exactly like exc_de/exc_ud/exc_nm: bounce to the
; guest's own real-mode IVT handler, the same thing real hardware's IDT-driven
; INT dispatch would have done. No EOI -- these are software INTs / CPU traps,
; not PIC lines. ----
%assign v 1
%rep 5
deflt_%[v]:
    pushad
    mov ebx, v
    jmp deflt_common
%assign v v+1
%endrep
deflt_16:
    pushad
    mov ebx, 16
    jmp deflt_common
%assign v 18
%rep (0x67 - 18)
deflt_%[v]:
    pushad
    mov ebx, v
    jmp deflt_common
%assign v v+1
%endrep
%assign v 0x68
%rep (0x70 - 0x68)
deflt_%[v]:
    pushad
    mov ebx, v
    jmp deflt_common
%assign v v+1
%endrep
%assign v 0x78
%rep (0xC0 - 0x78)
deflt_%[v]:
    pushad
    mov ebx, v
    jmp deflt_common
%assign v v+1
%endrep
%assign v 0xC1
%rep (0x100 - 0xC1)
deflt_%[v]:
    pushad
    mov ebx, v
    jmp deflt_common
%assign v v+1
%endrep
deflt_common:
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    call remapped_pic_line
    jc .irq
    call reflect_vector
    popad
    iretd
.irq:
    call irq_reflect_line
    popad
    iretd

; ---- Vector 17 (#AC alignment check): the ONLY newly-covered vector whose
; CPU-exception form carries an error code. This emulator never sets CR0.AM
; (no #AC raise site exists), so the error-code shape can never actually
; arrive here in practice -- but a bare `INT 17h` from V86 dispatches with NO
; error code (software INT, deliver_exception never pushes one for is_external
; or a plain software INT regardless of vector), so this must behave exactly
; like deflt_common's no-error-code frame. Kept as its own named gate (not
; folded into the 18-0x6F run) to document the asymmetry rather than let it
; hide inside a generated range. ----
deflt_ac:
    pushad
    mov ebx, 17
    jmp deflt_common

; ---- Vector 0x67 (EMS + VCPI, which share the INT). Reached through the
; static IDT only when the guest's live IOPL is 3 (otherwise INT n is
; IOPL-sensitive in V86 and the call arrives through vec13_entry's .intn
; path, which performs the same AH=DEh split). AH=DEh -> the monitor-side
; VCPI server; anything else reflects to the guest's own IVT handler (the
; V86 EMS driver) exactly like a deflt_ gate. The frame's saved CS:IP
; already points past the INT instruction on this path (software-INT gate
; dispatch), so no IP advance -- unlike .intn's fault frame. ----
int67_entry:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    cmp byte [esp+29], 0xDE       ; guest AH (pushad EAX at [esp+28])
    jne .reflect
    lea ebp, [esp + 32]           ; no error code on a software-INT frame
    mov esi, esp                  ; pushad base
    call vcpi_dispatch
    popad
    iretd
.reflect:
    mov ebx, 0x67
    jmp deflt_common              ; re-loads DS/FS: harmless

; ---- Vector 0xC0 (TOKAEMM-private monitor calls). Reached through the static
; IDT only when the guest's live IOPL is 3; below that, INT n is IOPL-sensitive
; in V86 and the call arrives through vec13_entry's .intn path, which performs
; the same cookie split. The frame's saved CS:IP is already past the INT on this
; path (software-INT gate dispatch), so no IP advance -- unlike .intn's fault
; frame. A foreign INT 0xC0 reflects to the guest's own handler. ----
intc0_entry:
    pushad
    mov ax, 0x10
    mov ds, ax                    ; flat: deliver_exception nulled the segments
    mov ax, 0x20
    mov fs, ax                    ; FS = driver data
    cmp word [esp+20], 0x544D     ; guest DX == 'TM' (XMS-move memcpy)?
    je .memcpy
    cmp word [esp+20], 0x4D50     ; ... 'PM' (EMS frame remap)?
    je .remap
    cmp word [esp+20], 0x5154     ; ... 'TQ' (arena free query)?
    je .query
    mov ebx, 0xC0                 ; foreign: reflect like any other vector.
    jmp deflt_common              ; (re-loads DS/FS: harmless; deflt_common
                                   ; expects the pushad frame still on the
                                   ; stack, exactly like int67_entry's .reflect)
.memcpy:
    call flat_memcpy
    popad
    iretd
.remap:
    call frame_remap
    popad
    iretd
.query:
    mov bl, [fs:arena_q_type]
    call arena_query32
    mov [fs:arena_q_largest], ax
    mov [fs:arena_q_total], dx
    popad
    iretd

; ---- VCPI 1.0 server dispatch (INT 67h AH=DEh, AL = subfunction).
; presence + the query/page-pool/system-register/PIC set (DE00, DE02-DE0B).
; DE01/DE0C (the PM interface + mode switch) are later rungs; they and every
; undefined subfunction answer 8Fh, the spec's recommended "undefined
; subfunction code" (VCPI 1.0 spec p.5). Presence is answered regardless of
; ems_on: a frameless manager still provides VCPI (the EMM386 NOEMS / JEMM
; NOEMS precedent).
;   in: ESI = pushad base (guest regs: EDI+0, EBX+16, EDX+20, ECX+24,
;       EAX+28), EBP = &frame.eip (V86 frame: ES at +20, past EIP/CS/EFLAGS/
;       ESP/SS), DS = flat, FS = driver data.
;   The CALLER advances the frame IP (the two entry paths differ: vec13's
;   fault frame points at the INT, the int67_entry gate frame is already
;   past it). Guest register writes go through the pushad block; live
;   eax/ecx/edx are popad-restored, so only [esi+..] writes are outputs. ----
vcpi_dispatch:
    movzx eax, byte [esi+28]      ; guest AL = VCPI subfunction
    cmp al, 0x0C
    ja .undef
    jmp dword [fs:.jt + eax*4]    ; offsets are driver-relative == CS-relative
.jt:
    dd .de00, .de01, .de02, .de03
    dd .de04, .de05, .de06, .de07
    dd .de08, .de09, .de0a, .de0b
    dd .de0c

.de00:                            ; presence: AH=0, BX = version 1.0
    mov byte [esi+29], 0
    mov word [esi+16], 0x0100
    ret

; DE01 Get Protected Mode Interface: initialize the client's 0th page table
; (guest ES:DI) and three GDT descriptors (guest DS:SI), return the PM entry
; offset in EBX. The copy covers PT0 entries 0..0x10F -- the whole V86
; window this monitor furnishes (first MB + the 64K A20/HMA window), which
; also maps the entire low server core (code, data, GDT, TSS, and stack).
; The server page tables may be reserved above 1 MB, but DE0C reads pd_lin
; from low server data before switching back to the server CR3.
; Software-defined PTE bits 9-11 are cleared in the copy (spec p.6; the
; 386MAX COPY_PTE convention). The descriptors: +0 the server code segment
; (base = base_lin, byte limit = resident_core_end-1, 32-bit CPL0 code -- entry
; offsets are driver offsets), +8 a flat 4GB data mirror of selector 0x10,
; +16 a driver-data mirror of selector 0x20; the PM entry reaches them as
; CS+8 / CS+16 per the spec's consecutive-slot contract.
.de01:
    movzx eax, word [ebp+20]      ; guest ES
    shl eax, 4
    movzx ecx, word [esi+0]       ; guest DI
    add eax, ecx                  ; EAX = page-table buffer linear
    mov ecx, [fs:pd_lin]
    add ecx, 0x1000               ; ECX = PT0 source linear
    mov ebx, 0x110
.pte_copy:
    mov edx, [ecx]
    and edx, 0xFFFFF1FF           ; clear software bits 9-11
    mov [eax], edx
    add ecx, 4
    add eax, 4
    dec ebx
    jnz .pte_copy
    add word [esi+0], 0x110*4     ; DI -> first unused page table entry
    movzx eax, word [ebp+24]      ; guest DS
    shl eax, 4
    movzx ecx, word [esi+4]       ; guest SI
    add eax, ecx                  ; EAX = &descriptor[0]
    mov edx, [fs:base_lin]
    mov word [eax], resident_core_end - 1 ; code: limit 15..0 (image < 64K)
    mov [eax+2], dx               ; base 15..0
    shr edx, 16
    mov [eax+4], dl               ; base 23..16
    mov byte [eax+5], 0x9B        ; present ring-0 exec/read code, accessed
    mov byte [eax+6], 0x40        ; D=1 (USE32), G=0, limit 19..16 = 0
    mov [eax+7], dh               ; base 31..24
    mov dword [eax+8], 0x0000FFFF ; +8: flat 4GB data (selector 0x10 mirror)
    mov dword [eax+12], 0x00CF9300
    mov edx, [fs:base_lin]        ; +16: driver data (selector 0x20 mirror)
    mov word [eax+16], 0xFFFF
    mov [eax+18], dx
    shr edx, 16
    mov [eax+20], dl
    mov byte [eax+21], 0x93       ; present ring-0 read/write data, accessed
    mov byte [eax+22], 0xCF       ; G=1, B=1, limit 19..16 = 0xF (4GB)
    mov [eax+23], dh
    mov dword [esi+16], vcpi_pm_entry ; EBX = entry offset in the code seg
    mov byte [esi+29], 0
    ret

.de02:                            ; max physical memory address: EDX = the
    mov eax, [fs:xms_pool_end]    ; highest 4K page DE04 could ever return,
    cmp eax, [fs:xms_pool_base]   ; (vcpi_pool_base/end are gone -- the arena's
    je .de02_empty                ; own bounds mean the same thing now)
    sub eax, 0x1000               ; 12 LSBs zero (spec: both sides mask)
    mov [esi+20], eax
    mov byte [esi+29], 0
    ret
.de02_empty:
    mov dword [esi+20], 0
    mov byte [esi+29], 0
    ret

.de03:                            ; free 4K page count -> EDX
    push ebx
    push esi                      ; arena_query32 clobbers esi as its scan
                                   ; cursor; esi is this dispatch's pushad-frame
                                   ; pointer and must survive for the [esi+20]
                                   ; write below (the plan's original draft
                                   ; wrote through esi AFTER the call, using
                                   ; whatever scan position esi was left at)
    mov bl, ALLOC_VCPI
    call arena_query32             ; DX = total free granules for 4 KB blocks
    movzx edx, dx
    shr edx, 2                    ; granules -> 4 KB pages
    pop esi
    mov [esi+20], edx
    pop ebx
    mov byte [esi+29], 0
    ret

.de04:                            ; allocate a 4K page -> EDX = physical
    call vcpi_page_alloc
    jc .de04_oom
    mov [esi+20], eax
    mov byte [esi+29], 0
    ret
.de04_oom:
    mov byte [esi+29], 0x88       ; pool exhausted
    ret

.de05:                            ; free the 4K page at physical EDX
    mov eax, [esi+20]
    and eax, 0xFFFFF000           ; spec: mask the 12 LSBs
    call vcpi_page_free
    jc .de05_bad
    mov byte [esi+29], 0
    ret
.de05_bad:
    mov byte [esi+29], 0x8A       ; outside the pool / not allocated
    ret

.de06:                            ; phys addr of V86 page CX -> EDX. The
    movzx eax, word [esi+24]      ; window is what this server furnishes to
    cmp eax, 0x110                ; clients: first MB + the 64K A20/HMA
    jae .de06_bad                 ; window (PT0 entries 0..0x10F)
    mov ecx, [fs:pd_lin]
    mov eax, [ecx + 0x1000 + eax*4] ; PT0[page] via flat DS
    and eax, 0xFFFFF000
    mov [esi+20], eax
    mov byte [esi+29], 0
    ret
.de06_bad:
    mov byte [esi+29], 0x8B       ; invalid page number
    ret

.de07:                            ; read CR0 -> EBX
    mov eax, cr0
    mov [esi+16], eax
    mov byte [esi+29], 0
    ret

; DE08/DE09: read/load the debug registers through an 8-dword array at guest
; ES:DI (DR0 first, DR4/DR5 unused -- read back as zero, ignored on load).
; The guest buffer is reached via flat DS at ES*16+DI: monitor paging applies,
; so an HMA-resident buffer honors the va20 illusion like any guest access.
.de08:
    call vcpi_dr_buf              ; -> EAX = buffer linear
    mov edx, dr0
    mov [eax], edx
    mov edx, dr1
    mov [eax+4], edx
    mov edx, dr2
    mov [eax+8], edx
    mov edx, dr3
    mov [eax+12], edx
    xor edx, edx
    mov [eax+16], edx             ; DR4/DR5: unused per the interface
    mov [eax+20], edx
    mov edx, dr6
    mov [eax+24], edx
    mov edx, dr7
    mov [eax+28], edx
    mov byte [esi+29], 0
    ret
.de09:
    call vcpi_dr_buf
    mov edx, [eax]
    mov dr0, edx
    mov edx, [eax+4]
    mov dr1, edx
    mov edx, [eax+8]
    mov dr2, edx
    mov edx, [eax+12]
    mov dr3, edx
    mov edx, [eax+24]
    mov dr6, edx
    mov edx, [eax+28]
    mov dr7, edx
    mov byte [esi+29], 0
    ret

.de0a:                            ; get 8259 vector bases -> BX (master),
    mov ax, [fs:vcpi_pic_master]  ; CX (slave)
    mov [esi+16], ax
    mov ax, [fs:vcpi_pic_slave]
    mov [esi+24], ax
    mov byte [esi+29], 0
    ret

.de0b:                            ; set 8259 vector bases from BX/CX.
    in al, 0x21                   ; preserve interrupt masks across ICW init
    mov bl, al
    in al, 0xA1
    mov bh, al
    mov ax, [esi+16]
    mov [fs:vcpi_pic_master], ax
    mov ax, [esi+24]
    mov [fs:vcpi_pic_slave], ax

    mov al, 0x11
    out 0x20, al
    out 0xA0, al
    mov al, [fs:vcpi_pic_master]
    out 0x21, al
    mov al, [fs:vcpi_pic_slave]
    out 0xA1, al
    mov al, 0x04                  ; master: slave on IR2
    out 0x21, al
    mov al, 0x02                  ; slave id on cascade line 2
    out 0xA1, al
    mov al, 0x01                  ; 8086 mode
    out 0x21, al
    out 0xA1, al
    mov al, bl
    out 0x21, al
    mov al, bh
    out 0xA1, al
    mov byte [esi+29], 0
    ret

.undef:
    mov byte [esi+29], 0x8F       ; undefined / not-yet-implemented subfunction
    ret

; DE0C Switch From V86 Mode to Protected Mode (spec p.12; the exact flow
; JEMM's traced handshake runs on this CPU). Guest ESI = first-MB linear of
; the 6-field structure {CR3, &GDTR value, &IDTR value, LDTR, TR, CS:EIP}.
; Interrupts are already off (interrupt gate). Spec register contract: EAX,
; ESI, DS/ES/FS/GS destroyed; everything else must arrive at the client
; entry intact -- restored from the pushad block below, since this path
; never returns to .done_gp/popad. The V86 trap frame on the monitor stack
; is abandoned: TSS.ESP0 is static, the next V86 entry starts fresh.
;
; Ordering per spec: CR3 first, then GDTR/IDTR read through the NEW paging
; context via the linear pointers (the structure and both pseudo-descriptors
; are first-MB, mapped identically in both contexts by the DE01 contract);
; GDTR before LDTR/TR; the client TSS descriptor's busy bit cleared through
; the flat data segment (base 0, 4GB limit -- the spec's required shape)
; before LTR. The segment-descriptor caches carry DS/SS across the lgdt (the
; spec relies on exactly this), so the monitor stack and flat reads keep
; working until the far jump hands the client its own world.
.de0c:
    mov eax, [esi+4]              ; guest ESI = switch-structure linear
    mov ecx, [eax]
    mov cr3, ecx                  ; client paging context
    mov edx, [eax+4]
    lgdt [edx]                    ; client GDT (read post-switch, spec order)
    mov edx, [eax+8]
    lidt [edx]                    ; client IDT
    movzx ecx, word [eax+0x0E]    ; TR selector
    and ecx, 0xFFF8
    jz .no_tr                     ; defensive: LTR(0) would #GP; clients
    mov edx, [eax+4]              ; always furnish a TSS per spec
    mov edx, [edx+2]              ; client GDT linear base
    and byte [edx+ecx+5], 0xFD    ; clear the TSS-busy type bit
    ltr word [eax+0x0E]
.no_tr:
    lldt word [eax+0x0C]          ; LLDT(0) is legal: null LDT
    mov ebx, [esi+16]             ; hand the guest's registers through
    mov ecx, [esi+24]
    mov edx, [esi+20]
    mov edi, [esi+0]
    mov ebp, [esi+8]
    jmp far dword [eax+0x10]      ; CS:EIP from the structure: the client's
                                  ; protected-mode entry, interrupts off,
                                  ; SS:ESP = this monitor stack (>=16 bytes
                                  ; free; the client sets its own stack)

; Guest ES:DI (V86) -> EAX = buffer linear address for DE08/DE09.
; ES from the V86 trap frame at [ebp+20], DI from the pushad block.
vcpi_dr_buf:
    movzx eax, word [ebp+20]
    shl eax, 4
    movzx edx, word [esi+0]
    add eax, edx
    ret

; Arena free-space walk (386MAX QRY_PGCNT). in: BL = an ALLOC_* offset, FS =
; driver data. out: AX = largest free run, DX = total free, both in granules
; rounded down to that type's boundary. Clobbers eax, ebx, ecx, edx, esi, edi
; on a cache miss; a cache hit only touches eax, ecx, edx (every caller already
; saves/restores conservatively around this call, so either is safe to observe).
;
; Each maximal clear span is measured from its boundary-ALIGNED start and its
; length rounded down, so a span too small or too misaligned to host a block of
; this type contributes nothing. The number a caller gets is a number it can
; spend, which is the whole point of reporting it.
;
; The cursor always resumes at the span's END once the span has been scored --
; not partway through it. An earlier draft resumed at aligned_start + the
; rounded-down length, which stalls forever on a span whose usable tail is
; shorter than one boundary unit (that remainder rounds down to zero granules,
; so the cursor advanced by zero and re-entered the very same span next
; iteration). The end of the span is always strictly past where the span began,
; so resuming there is what actually guarantees termination.
;
; D1 memo: the walk below costs ~5-6 instructions per granule (~23,000 of
; them), so a naive per-call walk is the single most expensive thing a guest
; can trigger through a LIM/XMS status call. Check the per-type cache first;
; only fall into the walk on a miss, and stamp the cache with the generation
; the answer is valid for before returning. See arena_gen's comment for why
; this cannot drift from the bitmap the way a hand-maintained counter could.
arena_query32:
    movzx eax, bl                 ; save the type argument; bl is reused below
    mov ecx, eax
    shr ecx, 1                    ; ALLOC_* offsets are 0/2/4 -> cache index 0/1/2
    mov edx, [fs:arena_gen]
    cmp edx, [fs:arena_qc_gen + ecx*4]
    jne .walk
    movzx eax, word [fs:arena_qc_largest + ecx*2]
    movzx edx, word [fs:arena_qc_total + ecx*2]
    ret
.walk:
    push ecx                      ; cache index; needed again once the walk ends
    movzx ebx, al                 ; al still holds the original type argument
    movzx ebx, word [fs:alloc_lim + ebx]  ; boundary - 1
    movzx ecx, word [fs:arena_granules]
    xor edx, edx                  ; total
    xor edi, edi                  ; largest
    xor esi, esi                  ; scan cursor
.next_span:
    cmp esi, ecx
    jae .done
    bt [fs:arena_bmp], esi
    jnc .span_start
    inc esi                       ; skip an allocated granule
    jmp .next_span
.span_start:
    mov eax, esi                  ; eax = span start
.span_scan:
    inc esi
    cmp esi, ecx
    jae .span_end
    bt [fs:arena_bmp], esi
    jnc .span_scan
.span_end:                        ; esi = span end (exclusive), eax = span start
    add eax, ebx                  ; align the span's usable start up
    push ebx
    not ebx
    and eax, ebx
    pop ebx
    cmp eax, esi
    jae .next_span                ; nothing usable: esi already holds the span
                                   ; end, so the scan resumes there unmoved
    push esi                      ; save the span end -- the cursor resumes
                                   ; there regardless of how many boundary-sized
                                   ; blocks the usable tail rounds down to
    sub esi, eax                  ; esi = usable granules (unrounded)
    xchg eax, esi                 ; eax = usable granules, esi = aligned start
    push ebx
    not ebx
    and eax, ebx                  ; ... rounded down to the boundary
    pop ebx
    add edx, eax
    cmp eax, edi
    jbe .keep
    mov edi, eax
.keep:
    pop esi                       ; resume at the span's end, not partway
                                   ; through it -- see the header comment
    jmp .next_span
.done:
    mov eax, edi
    pop ecx                       ; cache index, saved before the walk started
    mov [fs:arena_qc_largest + ecx*2], ax
    mov [fs:arena_qc_total + ecx*2], dx
    mov ebx, [fs:arena_gen]
    mov [fs:arena_qc_gen + ecx*4], ebx
    ret

; Allocate one 4 KB VCPI page: four consecutive granules on a 4-granule
; boundary (386MAX ALLOC_LIM @ALLOC_VCPI). -> EAX = physical address, CF clear;
; or CF set when no such page is free. The scan is bounded by the arena's page
; count rather than by a free counter, so a bookkeeping slip can no longer spin
; forever the way the counter-driven version could.
vcpi_page_alloc:
    push ebx
    push ecx
    push edx
    movzx ecx, word [fs:arena_granules]
    shr ecx, 2                    ; whole 4 KB pages the arena covers
    jz .none
    movzx eax, word [fs:vcpi_cursor]
    mov edx, ecx                  ; pages left to examine before giving up
.scan:
    cmp eax, ecx
    jb .test
    xor eax, eax                  ; wrap to the arena base
.test:
    mov ebx, eax
    shl ebx, 2                    ; first granule of this page
    bt [fs:arena_bmp], ebx
    jc .next
    inc ebx
    bt [fs:arena_bmp], ebx
    jc .next
    inc ebx
    bt [fs:arena_bmp], ebx
    jc .next
    inc ebx
    bt [fs:arena_bmp], ebx
    jnc .take
.next:
    inc eax
    dec edx
    jnz .scan
.none:
    pop edx
    pop ecx
    pop ebx
    stc
    ret
.take:
    mov ebx, eax
    shl ebx, 2
    bts [fs:arena_bmp], ebx
    inc ebx
    bts [fs:arena_bmp], ebx
    inc ebx
    bts [fs:arena_bmp], ebx
    inc ebx
    bts [fs:arena_bmp], ebx
    bts [fs:vcpi_bmp], eax        ; record VCPI as this page's owner
    inc dword [fs:arena_gen]      ; D1: invalidate every cached query
    lea edx, [eax+1]
    mov [fs:vcpi_cursor], dx
    movzx ebx, word [fs:arena_base_kb]
    shl ebx, 10                   ; arena base, linear
    shl eax, 12
    add eax, ebx                  ; -> the page's physical address
    pop edx
    pop ecx
    pop ebx
    clc
    ret

; Free the 4 KB VCPI page at physical EAX (4K-aligned by the caller). CF set if
; the address is outside the arena or the page is not one VCPI handed out --
; which now includes any granule an XMS block or an EMS page owns.
vcpi_page_free:
    push ebx
    push ecx
    movzx ecx, word [fs:arena_base_kb]
    shl ecx, 10
    sub eax, ecx
    jb .bad
    shr eax, 12                   ; 4 KB page index inside the arena
    movzx ecx, word [fs:arena_granules]
    shr ecx, 2
    cmp eax, ecx
    jae .bad
    bt [fs:vcpi_bmp], eax
    jnc .bad
    btr [fs:vcpi_bmp], eax
    mov ebx, eax
    shl ebx, 2
    btr [fs:arena_bmp], ebx
    inc ebx
    btr [fs:arena_bmp], ebx
    inc ebx
    btr [fs:arena_bmp], ebx
    inc ebx
    btr [fs:arena_bmp], ebx
    inc dword [fs:arena_gen]      ; D1: invalidate every cached query
    pop ecx
    pop ebx
    clc
    ret
.bad:
    pop ecx
    pop ebx
    stc
    ret

; ---- The protected-mode entry point: far-called USE32 by clients running
; under THEIR OWN CR3/GDT/IDT at CPL 0, with CS = the server code descriptor
; DE01 furnished. The monitor selectors 0x08-0x20 are MEANINGLESS here; the
; only anchors are CS-relative addressing and the spec's consecutive-slot
; contract: CS+8 = flat data, CS+16 = driver data (VCPI 1.0 spec p.6). The
; server memory itself is reachable because the client's 0th page table was
; copied from ours (everything driver-resident is below 1MB). All segment
; registers are preserved (spec p.7); USE32 far return. Serves the PM set
; available here: DE00, DE03, DE04, DE05, and DE0C;
; everything else answers 8Fh. The pool ops run IF-masked: clients may call
; with interrupts enabled and an ISR of theirs could reenter the interface
; mid-bitmap-update. FS is borrowed for driver data so vcpi_page_alloc/free
; are shared verbatim with the V86-path dispatch.
vcpi_pm_entry:
    cmp ax, 0xDE0C                ; the PM->V86 switch never returns: route
    je vcpi_pm_to_v86             ; it before the prologue pushes so the
                                  ; spec's stack-frame offsets hold
    push fs
    push ecx
    mov cx, cs
    add cx, 0x10
    mov fs, cx                    ; FS = driver data via the client's GDT
    cmp ah, 0xDE
    jne .undef
    cmp al, 0x00
    je .de00
    cmp al, 0x03
    je .de03
    cmp al, 0x04
    je .de04
    cmp al, 0x05
    je .de05
.undef:
    mov ah, 0x8F
    jmp .out
.de00:                            ; presence: AH=0, BX = version 1.0
    mov bx, 0x0100
    xor ah, ah
    jmp .out
.de03:                            ; free 4K page count -> EDX
    pushfd
    cli
    push eax                      ; AL must survive (only AH/EDX are outputs);
                                   ; arena_query32 clobbers eax like
                                   ; vcpi_page_alloc does below
    push ebx
    mov bl, ALLOC_VCPI
    call arena_query32
    movzx edx, dx
    shr edx, 2
    pop ebx
    pop eax
    popfd
    xor ah, ah
    jmp .out
.de04:                            ; allocate a 4K page -> EDX = physical
    pushfd
    cli
    push eax                      ; AL must survive (only AH/EDX are outputs)
    call vcpi_page_alloc          ; -> EAX = phys or CF; clobbers ecx, edx
    jc .a_oom
    mov edx, eax
    pop eax
    popfd
    xor ah, ah
    jmp .out
.a_oom:
    pop eax
    popfd
    mov ah, 0x88
    jmp .out
.de05:                            ; free the 4K page at physical EDX
    pushfd
    cli
    push eax
    mov eax, edx
    and eax, 0xFFFFF000           ; spec: mask the 12 LSBs
    call vcpi_page_free           ; clobbers only EAX; EDX stays intact
    setc cl                       ; capture CF: the popfd below wipes flags
    pop eax
    popfd
    test cl, cl
    jnz .f_bad
    xor ah, ah
    jmp .out
.f_bad:
    mov ah, 0x8A
    jmp .out
.out:
    pop ecx
    pop fs
    retf                          ; USE32 code segment: 32-bit far return

; ---- DE0C Switch From Protected Mode to V86 Mode (spec p.15). Far-called
; on the CLIENT's stack (spec: SS:ESP in the first megabyte -- the frame is
; read with 32-bit addressing, relying on the spec-clean full ESP real
; clients maintain for exactly this call; JEMM makes the same assumption
; with real DOS4GW). Frame at entry: [esp+0]/[esp+4] the USE32 far-call
; return address (discarded, this call never returns), then EIP, CS,
; EFLAGS(reserved), ESP, SS, ES, DS, FS, GS as dwords -- deliberately the
; hardware ring0->V86 IRETD frame, so after restoring the server context we
; fill the EFLAGS slot, drop the return address, and IRETD straight off the
; client's stack (it stays mapped across the CR3 switch: first MB is
; identical in both contexts). Spec register contract: only EAX destroyed;
; every segment register is reloaded by the IRETD itself. The monitor's
; TSS-busy bit is cleared through the freshly-reloaded flat DS before LTR,
; and vif is forced 0 so the resumed V86 side stays virtually masked until
; it STIs (the spec's IF-cleared intent through the vif layer; the frame's
; real IF stays 1, the monitor convention). ----
vcpi_pm_to_v86:
    cli                           ; enforce the spec's interrupts-off rule
    mov eax, [cs:pd_lin]
    mov cr3, eax                  ; server paging context
    lgdt [cs:gdtr]                ; server GDT/IDT: the INIT-patched
    lidt [cs:idtr]                ; pseudo-descriptors (absolute bases)
    mov ax, 0x10
    mov ds, ax                    ; flat data from the server GDT
    mov eax, [cs:base_lin]
    and byte [eax + gdt + 0x18 + 5], 0xFD ; clear our TSS-busy type bit
    mov ax, 0x18
    ltr ax
    xor ax, ax
    lldt ax                       ; the monitor uses no LDT
    mov eax, [cs:base_lin]
    mov byte [eax + vif], 0       ; V86 resumes virtually masked
    mov dword [esp+0x10], 0x00020202 ; EFLAGS slot: VM=1, real IF=1,
                                  ; IOPL=0, reserved bit 1
    add esp, 8                    ; drop the far-call return address
    iretd                         ; EIP,CS,EFLAGS,ESP,SS,ES,DS,FS,GS -> V86

; EOI the chip(s) for line EBX. The just-delivered line is the highest in
; service on its chip, so the non-specific EOI clears the right bit; slave
; lines also EOI the master's cascade. Clobbers AL.
irq_eoi:
    cmp ebx, 8
    jb .master
    mov al, 0x20
    out 0xA0, al
.master:
    mov al, 0x20
    out 0x20, al
    ret

; A default-gate vector can be a software INT or a hardware IRQ after a VCPI
; client remaps the PIC away from the DOS defaults. If EBX is inside the current
; master/slave base range and the corresponding PIC ISR bit is set, return CF=1
; with EBX rewritten to the IRQ line. Otherwise CF=0 and EBX remains a vector.
remapped_pic_line:
    push eax
    push ecx
    push edx
    movzx ecx, word [fs:vcpi_pic_master]
    mov eax, ebx
    sub eax, ecx
    cmp eax, 8
    jb .master
    movzx ecx, word [fs:vcpi_pic_slave]
    mov eax, ebx
    sub eax, ecx
    cmp eax, 8
    jb .slave
.no:
    clc
    pop edx
    pop ecx
    pop eax
    ret
.master:
    mov ecx, eax                  ; candidate line 0..7
    mov al, 0x0B                  ; OCW3: read ISR
    out 0x20, al
    in al, 0x20
    movzx edx, al
    mov eax, 1
    shl eax, cl
    test edx, eax
    jz .no
    mov ebx, ecx
    stc
    pop edx
    pop ecx
    pop eax
    ret
.slave:
    mov ecx, eax                  ; candidate slave sub-line 0..7
    mov al, 0x0B
    out 0xA0, al
    in al, 0xA0
    movzx edx, al
    mov eax, 1
    shl eax, cl
    test edx, eax
    jz .no
    lea ebx, [ecx + 8]
    stc
    pop edx
    pop ecx
    pop eax
    ret

; Reflect line EBX to its guest IVT vector using the current VCPI/PIC mapping.
; Tail-jumps reflect_vector.
irq_reflect_line:
    cmp ebx, 8
    jb .master
    movzx eax, word [fs:vcpi_pic_slave]
    add ebx, eax
    sub ebx, 8
    jmp reflect_vector
.master:
    movzx eax, word [fs:vcpi_pic_master]
    add ebx, eax
    jmp reflect_vector

; Reflect an interrupt into the guest's real-mode IVT handler.
;   in: EBX = vector, EBP = &frame.eip, FS = driver data.  clobbers eax,ecx,edx,edi
reflect_vector:
    mov edx, [ebp+16]            ; guest SS
    shl edx, 4                   ; edx = guest stack base (linear)
    mov ax, [ebp+8]             ; guest flags, IF := VIF, virtual IOPL = 3
    and ax, 0xFDFF              ; (the image convention; see .pushfd)
    or ax, 0x3000
    cmp byte [fs:vif], 0
    je .rf
    or ax, 0x0200
.rf:
    sub word [ebp+12], 2         ; push FLAGS
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov ax, [ebp+4]             ; push CS
    sub word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov ax, [ebp]              ; push return IP
    sub word [ebp+12], 2
    movzx ecx, word [ebp+12]
    mov [edx+ecx], ax
    mov edi, ebx
    shl edi, 2                  ; vec*4 -> IVT entry (via null DS, base 0)
    movzx eax, word [edi]
    mov word [ebp], ax          ; guest IP = IVT[vec] offset
    movzx eax, word [edi+2]
    mov word [ebp+4], ax        ; guest CS = IVT[vec] segment
    mov byte [fs:vif], 0        ; entering the ISR clears VIF
    ret

; If VIF is set and lines are pending, deliver the highest-priority one per
; call (the reflect clears VIF; the guest ISR's IRET re-runs us, draining the
; queue). Priority = 8259 fully-nested with the slave cascaded at IR2:
; 0, 1, 8..15, then 2..7 (a raw line 2 cannot occur — cascade INTA resolves to
; the slave vectors — but the walk covers it so a held bit can never stick).
;   in: EBP = &frame.eip, FS = driver data.  clobbers eax,ebx,ecx,edx,edi
maybe_deliver:
    cmp byte [fs:vif], 0
    je .none
    movzx edx, word [fs:vip]
    test dx, dx
    jz .none
    xor ebx, ebx                  ; line 0
    test dl, 1
    jnz .hit
    mov ebx, 1                    ; line 1
    test dl, 2
    jnz .hit
    mov ebx, 8                    ; slave lines 8..15 (the cascade slot)
.slave:
    mov ecx, ebx
    mov ax, 1
    shl ax, cl
    test dx, ax
    jnz .hit
    inc ebx
    cmp ebx, 16
    jb .slave
    mov ebx, 2                    ; remaining master lines 2..7
.low:
    mov ecx, ebx
    mov ax, 1
    shl ax, cl
    test dx, ax
    jnz .hit
    inc ebx
    cmp ebx, 8
    jb .low
    ret                           ; unreachable: dx was nonzero
.hit:
    mov ecx, ebx
    mov ax, 1
    shl ax, cl
    not ax
    and [fs:vip], ax              ; claim the line
    jmp irq_reflect_line          ; tail: ret returns to maybe_deliver's caller
.none:
    ret

; Ring-0 flat memcpy for the XMS block MOVE (INT 0xC0 monitor service). The guest
; driver staged src/dst linear + byte count in its resident [xms_mv_*] dwords
; through its 16-bit ABI and enabled A20
; first; read them via FS = driver data (0x20). deliver_exception NULLED
; ES/DS/FS/GS on the V86->ring0 entry, and a null selector faults a PM memory
; access, so reload ES to the flat selector (DS is already 0x10 from monitor
; entry). The frame is untouched, so .done_gp's popad restores the guest.
flat_memcpy:
    mov ax, 0x10
    mov es, ax                    ; ES = flat (base 0); DS already 0x10
    mov edi, [fs:xms_mv_dst]      ; dst linear
    mov esi, [fs:xms_mv_src]      ; src linear
    mov ecx, [fs:xms_mv_len]      ; byte count
    cld                           ; (the REAL A20 gate is forced on at INIT and
    rep movsb                     ; never drops — EMBs above 1 MB never fold)
    ret

; Ring-0 EMS frame remap (INT 0xC0 'PM'). [ems_rm_lin] = frame-slot linear
; base, [ems_rm_phys] = backing physical base, or 0 to restore the INIT
; mapping (the UMB-backing bytes the INIT .umb_map loop pointed this window
; at), staged by ems_remap_slot in driver data and read via FS (cf. flat_memcpy).
; Rewrites the slot's 4
; PTEs in PT0 and reloads CR3 — the 386 full-TLB-flush idiom. Private,
; cookie-gated, single caller (ems_remap_slot) validates -> no arg checks.
; DS is already flat 0x10 from monitor entry; FS = 0x20 (driver data) for
; pd_lin. The frame is untouched, so .done_gp's popad restores the guest.
frame_remap:
    mov ebx, [fs:ems_rm_lin]      ; slot linear base
    mov ecx, [fs:ems_rm_phys]     ; backing phys (0 = unmap)
    test ecx, ecx
    jnz .have
    mov ecx, ebx                  ; restore INIT mapping: UMB backing for this lin
    sub ecx, UMB_LIN_BASE
    add ecx, UMB_PHYS_BASE
.have:
    or ecx, 7                     ; present/rw/user
    mov eax, [fs:pd_lin]
    add eax, 0x1000               ; PT0 linear
    mov edx, ebx
    shr edx, 12
    and edx, 0x3FF
    lea eax, [eax + edx*4]        ; &PT0[slot's first page] (flat DS)
    mov edx, 4
.pte:
    mov [eax], ecx
    add eax, 4
    add ecx, 0x1000
    dec edx
    jnz .pte
    mov eax, cr3                  ; full TLB flush, 386-style
    mov cr3, eax
    ret

; Ring-0 virtual-A20 window remap: linear [0x100000, 0x110000) becomes identity
; (va20 = 1) or folds onto phys [0, 0x10000) (va20 = 0) — the 8086 1 MB wrap the
; guest expects, as pure paging illusion while the REAL gate stays on (real
; EMM386's approach; a real A20-off would also fold the extended-RAM-backed
; UMB/EMS windows and corrupt DOS=UMB state, which is the bug this fixes).
; 16 PTEs in PT0 + CR3 reload. in: FS = driver data. Clobbers eax, ecx, edx.
a20_apply:
    mov eax, [fs:pd_lin]
    add eax, 0x1000 + 0x100*4     ; &PT0[0x100] (linear 0x100000)
    xor edx, edx                  ; fold target: phys 0
    cmp byte [fs:va20], 0
    je .have
    mov edx, 0x00100000           ; identity: phys 0x100000
.have:
    or edx, 7                     ; present/rw/user
    mov ecx, 16
.pte:
    mov [eax], edx
    add eax, 4
    add edx, 0x1000
    loop .pte
    mov eax, cr3
    mov cr3, eax
    ret

banner: db 'TOKAEMM: XMS/UMB/EMS memory manager; system running in V86.', 0x0D, 0x0A, 0

; Debug failure signal via the unit-tester exit port (AL = code).
signal32:
    mov ah, al
    mov al, 12
    out 0xE4, al
    mov al, ah
    out 0xE5, al
    mov al, 3
    out 0xE6, al
.h: jmp .h

align 16
tss:                              ; 0x68 TSS fields + 0x2000 I/O bitmap (all
    times 0x2070 db 0             ; zero = permissive; 0x92 set at INIT) + the
                                  ; 0xFF terminator byte, rounded up

align 4
mon_stack:
    times 0x400 db 0
mon_stack_top:
resident_core_end:

align 4096
tables:
    ; PD (1 page) + 6 PT (6 pages) = 0x7000, plus up to 0xFF0 of page-rounding slack
    ; (pd_lin = round_up_4k(base+tables), base is only paragraph-aligned, i.e. a
    ; multiple of 16, which caps the worst-case slack at 4096-16 = 0xFF0 rather
    ; than a full 0xFFF). Trimmed to this exact figure (D5): EMS's per-page chain
    ; (ems_link) pushed resident_core_end past the 0x7000 boundary, and the
    ; previously-unused 0x10 bytes of rounding-up-to-0x8000 padding is what kept
    ; the image inside the 64 KB driver offset ceiling.
    times 0x7000 + 0xFF0 db 0
resident_image_end:
; The reservation above now has ZERO margin: worst-case slack (0xFF0) plus the
; seven pages (0x7000) is exactly 0x7FF0. That worst case is 0xFF0 only because
; `tables` is itself page-aligned, which makes (base + tables) mod 4096 equal
; base mod 4096, and base is a paragraph, so the round-up is at most 4096-16.
; Drop the `align 4096` above and the bound becomes 0xFFF, which overruns the
; reservation by 15 bytes and corrupts whatever DOS put after the driver, on
; load addresses that depend on the boot's CONFIG.SYS ordering. Assert the
; alignment rather than trusting the directive to survive an edit.
%if (tables - $$) % 4096
    %error "TOKAEMM tables: must stay 4096-aligned; the reservation's slack budget assumes it"
%endif
%if ($ - $$) >= 0x10000
    %error "TOKAEMM resident image exceeds the 16-bit driver offset limit"
%endif
