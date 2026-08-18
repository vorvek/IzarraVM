; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; TOKAEMM.SYS memory manager. Runs the system in V86.
;
; The driver's INIT builds a load-relative PM/paging and
; ring-0 monitor environment in its OWN resident memory, then instead of a
; signal stub it IRETDs the *running kernel* into V86 at the SYSINIT return
; point (the EXECRH post-INIT code), so real FreeDOS keeps booting virtualized
; under the monitor. The guest runs at real IOPL 3 -- the reference posture,
; stated outright by 386MAX (QMAX_DTE.INC: `@VMIOPL equ 3 ; Use this IOPL for
; VM clients to avoid GP Faults on CLI/STI/HLT/INT/IRET/PUSHF/POPF`), and what
; JEMM does with its clients too. So the V86 sensitive instructions
; (CLI/STI/PUSHF/POPF/INT/IRET) execute for real and the guest's IF IS the real
; IF -- nothing is emulated or virtualized. The monitor reflects
; the timer (IRQ0 -> INT 08h) and keyboard (IRQ1 -> INT 09h) hardware
; interrupts to the guest's real-mode IVT; one the guest has disabled is never
; acknowledged at all and simply stays latched in the 8259A's IRR (real DOS
; brackets IRQ-sensitive code with CLI/STI).
;
; Addressing model (all load-segment relative):
;   * PM CODE selector 0x08  base = CS<<4    (monitor runs at driver offsets)
;   * PM DATA selector 0x10  base = 0 flat   (builds page tables at linear addrs)
;   * PM DATA selector 0x20  base = CS<<4    (monitor reaches its own state +
;                                             the saved kernel context, via FS)
; On a V86 fault the CPU nulls DS/ES/FS/GS; the monitor reads guest memory + the
; real-mode IVT through the null DS (base 0 == flat) and its own data through FS.
;
; All four GSW modes expose at least the 386 ISA. The guest-facing XMS/EMS/UMB
; entry points keep a 16-bit ABI and use KB units internally. The 64 MB map
; keeps each KB count under 0x10000, so every count still fits an UNSIGNED
; 16-bit register; bitmap bit indices exceed the signed 16-bit BT range and use
; the dword BT forms. They pass 32-bit arguments to INT 0xC0 monitor services
; through driver-resident scratch dwords read through FS.
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

; `vif` (virtual IF) lived here and is gone: the guest runs at real IOPL 3, so
; the REAL EFLAGS.IF is the guest's IF and there is nothing left to proxy.
hlt_pending: db 0                 ; 1 = hlt_vector holds a vector taken by the
                                  ; ring-0 sti;hlt wake and not yet reflected.
                                  ; OUT OF BAND on purpose: every byte value is
                                  ; a legal vector -- a guest may DE0B the
                                  ; master base to 0xF8, putting IRQ7 on vector
                                  ; 0xFF -- so an in-band sentinel would
                                  ; silently drop that line and leave its ISR
                                  ; bit set forever, the exact wedge this
                                  ; design exists to remove.
hlt_vector: db 0                  ; the vector the waking gate sits on, stored
                                  ; as EBX arrived. Every gate carries its own
                                  ; vector (see the routing rule above
                                  ; irq_body), so no conversion is involved.
r0_hlt: db 0                      ; 1 only inside monitor_body .hlt's sti;hlt
                                  ; window -- the sole ring-0 stretch with IF
                                  ; open. vec13_entry and irq_body read it to
                                  ; tell a waking IRQ's no-error ring-0 frame
                                  ; from a ring-0 exception's error-code frame
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
; `vip` (lines held while VIF=0) lived here and is gone with `.hold`. Under
; IOPL 3 the chip is never acknowledged while the guest has interrupts off, so
; there is nothing to queue: an undelivered request simply stays latched in the
; 8259A's own IRR, which is what a 386 with a real EMM does. (Its `align 2`
; went with it -- the word it was aligning no longer exists.)

; ---- XMS state (resident; reached via cs: overrides from V86) ----
old_2f:   dd 0                     ; previous INT 2Fh vector (chain target)
old_15:   dd 0                     ; previous INT 15h vector (chain target)
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
xms_slot_save: dw 0               ; 0Fh resize: keep the slot across
                                  ; find_gap (which clobbers SI)
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
FLAGS_VM      equ 0x00020000      ; EFLAGS bit 17: the frame-origin bit every
                                  ; monitor-entry discriminator forks on
UMB_LIN_BASE  equ 0x000C8000      ; first upper-hole linear byte
UMB_BYTES     equ 0x00028000      ; 160 KB (0xC8000..0xEFFFF)
UMB_PHYS_BASE equ 0x00110000      ; backing physical (just above the HMA)
UMB_SEG_BASE  equ 0x0C800         ; first UMB paragraph (segment); the window
                                  ; ends at the runtime umb_win_end

; The top of the UMB backing is ALSO where the paging tables and the allocatable
; arena begin. Naming it separately is not decoration: an attempt to reclaim
; space by shrinking the UMB window walked `pd_lin` straight onto the memory it
; had just freed, because the same expression means both "end of the window" and
; "base of everything above it". Anything that moves the window must decide,
; explicitly, whether this anchor moves with it.
ARENA_PHYS_BASE equ UMB_PHYS_BASE + UMB_BYTES

; PD (1 page) + 16 PT (16 pages) for the 64 MiB identity map. Read at three
; sites that must agree: INIT's high-path fit check, `pm_init`'s zero-fill, and
; the `IMAGE_END_OFF` reservation the `.low_tables` break address is built
; from. They were three separate literals; a change that grew one and not the
; others would either leave the tail unzeroed or hand DOS back memory the
; monitor is still paging through.
TABLES_BYTES  equ 0x11000
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
EMS_MAX_PAGES equ ARENA_GRANULES / EMS_PAGE_GRANULES   ; 4032
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
tree_mode:   db 0                 ; 1 when the command line contains /T (tree-
                                  ; styled signon banner prefix)
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
; The per-EMS-page chain link table lives in the system window now, at
; SYS_EMS_LINK. It was 8,064 bytes of resident core -- 20% of it -- and the
; last table here that grew with installed RAM. 16-bit V86 code cannot name a
; linear address that high, so ef_alloc/ef_free/ems_backing_of reach it through
; the INT 0xC0 'TA' service, one call per guest EMS operation.

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
; as SIGNED for a memory operand; ARENA_GRANULES exceeds 32767 on the 64 MB
; machine, so every bitmap probe uses the DWORD BT forms with a zero-extended
; 32-bit index -- the word forms would walk backward from the bitmap for any
; granule past 32 MB.
ARENA_GRANULES  equ 64512         ; 63 MB ceiling, in 1 KB granules
ARENA_BMP_BYTES equ ARENA_GRANULES / 8      ; 8064
ARENA_PAGES     equ ARENA_GRANULES / 4      ; 4 KB pages over the same span

; ---- The SYSTEM WINDOW: driver tables that only ring-0 monitor code touches.
;
; These used to sit in the resident core, where they are charged to CONVENTIONAL
; memory and, worse, scale with installed RAM: the three of them cost about 288
; bytes per megabyte of arena, which put a hard wall at ~148 MB (past that the
; core runs off the 0xFFF0 offset ceiling and the driver will not assemble).
;
; The window is a 4 MB linear region behind ONE page directory entry, backed by
; physical pages reserved next to the paging tables. It is deliberately placed
; far above any identity-mapped RAM so it can never collide with the arena, and
; it is mapped in NO client's page directory -- a VCPI protected-mode client
; that far-calls DE03/DE04/DE05 reaches this state only after VCPI_HOST_ENTER
; has switched CR3 to ours. (JEMM uses the same address for the same reason.)
;
; Reached with a flat DS and an absolute displacement, which every monitor entry
; already has (vec13/int67/intc0 all load DS = 0x10); the VCPI PM path gets one
; from CS+8, the flat descriptor DE01 furnishes in the client's own GDT.
SYS_LIN_BASE  equ 0xF8000000
SYS_VCPI_BMP  equ 0                         ; VCPI ownership bit per 4 KB page
SYS_ARENA_BMP equ SYS_VCPI_BMP + ARENA_PAGES / 8  ; ALLOCATED bit per 1 KB granule
SYS_EMS_LINK  equ SYS_ARENA_BMP + ARENA_BMP_BYTES ; next page in a handle's chain
SYS_USED      equ SYS_EMS_LINK + EMS_MAX_PAGES * 2
SYS_BYTES     equ (SYS_USED + 4095) & ~4095
; Physical layout of the reservation that follows the paging tables: one page
; for the window's own page table, then the data pages it maps.
SYS_PT_OFF    equ TABLES_BYTES
SYS_DATA_OFF  equ SYS_PT_OFF + 0x1000
SYS_RESV      equ 0x1000 + SYS_BYTES        ; total added to the reservation

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

; INT 0xC0 'TA' arena service. arena_bmp is in the system window now, past
; anything 16-bit V86 code can name, so the allocator itself runs in the
; monitor and the V86 side calls in. Allocation events are rare (an XMS 09h/0Fh,
; an EMS 43h); the path that IS hot, the AH=42h free query, still answers from
; the cs:-readable memo in V86 and never enters the monitor at all.
;
; Everything crosses in these words -- nothing in registers, nothing in flags.
; A CF return was avoided because the guest's EFLAGS sat at different stack
; offsets depending on the entry path; intc0_entry's gate frame is the only
; path now, but the memo convention stays -- it costs nothing and the other
; services already share it. ONE cookie with a sub-function byte, not four,
; because every cookie costs a compare at the dispatch site.
; The EMS sub-functions (4-7) are here rather than behind a cookie of their own
; because every cookie costs a compare at BOTH dispatch sites; a sub-function
; byte costs one table entry. They work in whole GUEST OPERATIONS -- build a
; handle's chain, tear one down, resolve one logical page -- not in single
; links, so EMS 43h/44h/45h each cost one monitor entry rather than one per
; page. The chain WALK stays a walk; only its entry moved.
ASVC_ALLOC       equ 0
ASVC_RELEASE     equ 1
ASVC_MARK        equ 2
ASVC_EMS_ALLOC   equ 3
ASVC_EMS_TAKE    equ 4            ; build a handle's page chain (EMS 43h)
ASVC_EMS_GIVE    equ 5            ; release one and clear it (EMS 45h)
ASVC_EMS_RESOLVE equ 6            ; logical -> backing, with the slot cache (44h)
ASVC_EMS_NEXT    equ 7            ; one chain link (ef_free's scrub walk)
ASVC_MAX         equ 7
arena_svc_op:    db 0
arena_svc_type:  db 0             ; ALLOC_* byte offset (alloc only)
arena_svc_index: dw 0             ; granule/page/slot index in, result out
arena_svc_count: dw 0             ; granule count, or the wanted logical page
arena_svc_fail:  db 0             ; 0 = success, 1 = could not be satisfied
; ems_page_alloc32 clobbers every register a chain walk would want to keep, so
; the EMS sub-functions carry their loop state here instead of in registers.
; ebp was historically reserved too: the IOPL-0 entry path parked &frame.eip
; in it across the sensitive-op emulation, so the EMS arms could not use it.
; That is VESTIGIAL now. These arms are reached only from intc0_entry's .arena
; via arena_svc, and intc0_entry never writes ebp -- it does a bare pushad, and
; its popad restores ebp whatever happens in between. The memory-carried loop
; state stays because ems_page_alloc32 genuinely clobbers everything else; only
; the ebp half of the rationale is dead.
ems_svc_slot:    dw 0
ems_svc_tail:    dw 0
ems_svc_cur:     dw 0
ems_svc_left:    dw 0
; Query memoization (D1). arena_query32 costs ~5-6 instructions per granule
; and ARENA_GRANULES is ~64,500, so an uncached walk is ~320,000-390,000
; monitor instructions.
;
; An earlier version of this comment justified the memo with a tick-loss
; story: a walk this long, held under an interrupt guard, could outlast one
; 54.9 ms IRQ0 tick and lose it to one-bit coalescing. That does not
; hold up under a second look. Each AH=42h/DE03/etc. call closes its own
; interrupts-off window with its own iret, so polling never accumulates one long
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

; The arena allocation bitmap -- one ALLOCATED bit per 1 KB granule -- is NOT
; here any more either; it lives at SYS_LIN_BASE + SYS_ARENA_BMP. It was the
; single largest item in the resident core and the largest of the three that
; scaled with installed RAM. The V86 side reaches it through the INT 0xC0 'TA'
; service; see arena_alloc32 and the wrappers in tokaemm-xms.inc.
; The VCPI OWNERSHIP bitmap -- one bit per 4 KB page, saying "allocated BY
; VCPI", without which DE05 would happily free an XMS block's or an EMS page's
; memory out from under it -- is NOT here any more. It lives in the system
; window at SYS_LIN_BASE + SYS_VCPI_BMP, because only ring-0 monitor code ever
; touches it and it scaled with installed RAM in conventional memory. See the
; SYS_LIN_BASE block above.

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
    ; Also recognizes a whole-token "/T", order-independent like NOEMS, which
    ; only selects the tree-styled signon banner prefix and touches nothing
    ; else. DEVICE= parameters here use '/' lead-in only, by policy -- the '-'
    ; alternative is reserved for the .COM tools' own command lines.
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
    cmp al, '/'                   ; RAW byte, BEFORE the upcase below: '/' is
                                  ; 0x2F and 'and 0xDF' would fold it to 0x0F
    jne .p_not_slash
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap                     ; bare "/" then separator
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'T'
    jne .p_skiptok
    mov byte [cs:tree_mode], 1
    jmp .p_skiptok                ; tolerate trailing junk (/TX): skip rest of token
.p_not_slash:
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

    ; Signon banner. INT 29h works during device INIT, when INT 21h
    ; AH=09h is unreliable.
    cmp byte [tree_mode], 0
    je .bplain
    mov si, banner_tree
.btl:
    lodsb                         ; DS = CS here
    test al, al
    jz .bplain                    ; prefix done, fall into the plain banner
    int 0x29
    jmp .btl
.bplain:
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
    cmp eax, 0x04000000           ; this monitor maps at most 64 MB
    jbe .mem_top_ok
    mov eax, 0x04000000
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
    cmp edi, ARENA_PHYS_BASE
    jb .no_umb
    mov byte [cs:umb_available], 1
    mov eax, ARENA_PHYS_BASE
    jmp .arena_base
.no_umb:
    mov word [cs:umb_win_end], UMB_SEG_BASE
    mov eax, edi                  ; empty arena when the UMB backing cannot fit
.arena_base:
    ; Keep the monitor's seventeen paging pages out of conventional memory when
    ; extended RAM has room.  The .SYS retains a low fallback tail for the
    ; 1 MiB profile, but normal machines reserve these aligned pages before
    ; the allocatable XMS/VCPI arena instead.
    mov edx, eax
    add edx, TABLES_BYTES + SYS_RESV
    jc .low_tables
    cmp edx, edi
    ja .low_tables
    mov [cs:pd_lin], eax
    mov eax, edx
    mov cx, resident_core_end     ; break offset, CS-relative
    xor si, si                    ; no extra paragraphs beyond CS
    jmp .tables_selected
.low_tables:
    ; The in-image tables are reserved past the end of the FILE and past the
    ; 64 KB offset ceiling, so the break cannot be expressed as cs:offset. Report
    ; it as (cs + IMAGE_END_OFF/16):0 instead. The kernel computes the paragraph
    ; count as FP_SEG(end) + (FP_OFF(end)+15)/16 - FP_SEG(driver), so an advanced
    ; segment reserves exactly the same paragraphs a large offset would have, and
    ; a zero offset cannot trip that 16-bit rounding.
    xor cx, cx
    mov si, IMAGE_END_OFF >> 4
.tables_selected:
    mov [cs:xms_pool_base], eax

    ; BIOS calls above may clobber ES:BX.  Reload the saved INIT request and
    ; report only the low core when the page tables were reserved high.
    les bx, [cs:rh_ptr]
    mov [es:bx+14], cx
    mov ax, cs
    add ax, si
    mov [es:bx+16], ax
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
    ; Clamp to the span the granule bitmap covers, measured from the POOL BASE:
    ; indices are pool-relative, so the ceiling has to be too. The old absolute
    ; ceiling (ARENA_PAGES * 0x1000 from physical zero) silently donated the
    ; bitmap's first ~1.3 MB of index space to memory below the pool and lost
    ; the same amount off the RAM top.
    mov ecx, eax
    add ecx, ARENA_GRANULES * 0x400
    cmp ebx, ecx
    jbe .arena_ceiling_ok
    mov ebx, ecx
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
    ; INT 15h too: once this driver owns all extended memory, AH=88h must
    ; report 0 KB free (XMS spec section 2.4; HIMEM and JEMMEX both do this).
    ; Leaving the raw BIOS answer visible double-counts the arena, and at
    ; exactly 64 MB installed it feeds Borland 32RTM's 16-bit total-memory
    ; math (64512 KB extended + 1024 KB base wraps to 0), which sent it into
    ; an allocate-everything-then-free-everything spiral ending in a dead
    ; idle -- found via TSUMERA in the extender gate, 2026-08-08. AH=0xE801
    ; stays unhooked deliberately: it reports INSTALLED memory, no current
    ; corpus member double-counts through it, and hooking it would change
    ; MEM's hardware row.
    mov eax, [ds:0x15*4]
    mov [cs:old_15], eax
    mov word [ds:0x15*4], i15_handler
    mov [ds:0x15*4+2], cs
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
    cmp ah, 0x50                  ; 50h sits past the 40h-4Dh table
    je ef_map_multi
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
; delivery already clears the real IF before any handler runs (including this
; one) -- the interrupt gate does it in hardware, and reflect_vector_v86
; clears bit 9 in the frame for a reflected entry -- so no guest ISR can
; interleave with the arena_query call below the way one
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
    ; The whole take-and-chain loop is one monitor call now: ems_link is in the
    ; system window. The service does its own unwind on failure, so the .unwind
    ; block this used to need is gone, and so is the cold-cache reset (it sets
    ; [si+16] itself, where the cache it feeds also lives).
    mov [cs:arena_svc_index], si
    mov [cs:arena_svc_count], bx  ; pages wanted
    mov byte [cs:arena_svc_op], ASVC_EMS_TAKE
    push dx
    mov dx, 0x4154                ; 'TA' monitor-call cookie
    int 0xC0
    pop dx
    cmp byte [cs:arena_svc_fail], 0
    jne .nofree
    mov byte [cs:si], 1           ; inuse
    mov byte [cs:si+1], 0         ; saved = 0
    mov [cs:si+2], bx             ; npages
    pop bp
    pop di
    pop cx
    pop si
    add sp, 2                     ; discard the saved DX: DX carries the handle
    pop ax
    xor ah, ah
    iret
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

; 50h map/unmap multiple handle pages (LIM 4.0). AL = 00 (physical page
; NUMBERS) or 01 (physical page SEGMENTS); DX = handle; CX = entry count;
; DS:SI = array of (logical, physical) word pairs, caller's DS. Logical
; 0xFFFF unmaps the slot. The WHOLE array is validated before any slot is
; touched, so a bad entry maps nothing (the LIM error contract). UW.EXE maps
; its frame with one 5000h call at startup and aborts with its error C003
; when the function answers 84h, which is how 50h earned its slot here.
ef_map_multi:
    cmp al, 1
    ja .badsub
    push si
    push cx
    push dx
    push bx
    push di
    push bp
    mov bp, cx                    ; entry count for the apply pass
    mov di, si                    ; array cursor (caller's DS)
    call ems_slot_of              ; DX -> SI = slot offset, or CF + AH=83h
    jc .out
    mov dx, di                    ; remember the array start (DX is free now)
    jcxz .done                    ; zero entries: a successful no-op
.validate:
    mov bx, [di+2]
    call .phys_to_slot
    jc .badphys
    mov bx, [di]
    cmp bx, 0xFFFF
    je .v_next
    cmp bx, [cs:si+2]             ; logical >= npages?
    jae .badlog
.v_next:
    add di, 4
    loop .validate
    mov di, dx                    ; rewind for the apply pass
.apply:
    mov bx, [di]
    cmp bx, 0xFFFF
    je .a_unmap
    call ems_backing_of           ; logical BX, slot SI -> CX = backing page
    jmp .a_slot
.a_unmap:
    mov cx, 0xFFFF
.a_slot:
    mov bx, [di+2]
    call .phys_to_slot            ; -> BL = frame slot (validated above)
    push si
    movzx si, bl
    add si, si
    mov [cs:ems_frame_map + si], cx
    pop si
    push ax
    mov al, bl
    call ems_remap_slot           ; AL=slot, CX=page|0xFFFF (preserves regs)
    pop ax
    add di, 4
    dec bp
    jnz .apply
.done:
    xor ah, ah
.out:
    pop bp
    pop di
    pop bx
    pop dx
    pop cx
    pop si
    iret
.badsub:
    mov ah, 0x8F                  ; undefined subfunction
    iret
.badphys:
    mov ah, 0x8B                  ; physical page out of range
    jmp .out
.badlog:
    mov ah, 0x8A                  ; logical page out of range
    jmp .out
; BX = raw physical field, AL = subfunction -> BL = slot 0-3, or CF set.
; The segment form only accepts the four exact frame-window segments.
.phys_to_slot:
    test al, al
    jnz .p_seg
    cmp bx, 3
    ja .p_bad
    clc
    ret
.p_seg:
    sub bx, EMS_FRAME_SEG
    jb .p_bad
    test bx, 0x03FF               ; 16 KB windows are 0x400 paragraphs apart
    jnz .p_bad
    shr bx, 10
    cmp bx, 3
    ja .p_bad
    clc
    ret
.p_bad:
    stc
    ret

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
    mov [cs:arena_svc_index], di  ; step to the next page in the chain. This is
    mov byte [cs:arena_svc_op], ASVC_EMS_NEXT   ; the one per-link monitor call
    push dx                        ; in the design, and it is on 45h release
    mov dx, 0x4154                 ; only -- a path that already does a
    int 0xC0                       ; frame_remap per live frame slot
    pop dx
    mov di, [cs:arena_svc_index]
    jmp .page
.pages_done:
    ; Release every page and clear the chain in one call: ems_link is in the
    ; system window. The unmap and saved_map scrub above stay here -- they touch
    ; ems_frame_map and ems_table, both still resident -- so this walks the
    ; chain first and only then hands it to the service to tear down.
    mov [cs:arena_svc_index], si
    mov byte [cs:arena_svc_op], ASVC_EMS_GIVE
    push dx
    mov dx, 0x4154
    int 0xC0
    pop dx
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
; The scan itself is ems_page_alloc32 in the monitor, because arena_bmp is in
; the system window; the D6 rationale (why there is no EMS-private free chain,
; and why the ems_cursor next-fit is load-bearing rather than an optimization)
; moved there with it.
ems_page_alloc:
    push dx
    mov byte [cs:arena_svc_op], ASVC_EMS_ALLOC
    mov dx, 0x4154                ; 'TA' monitor-call cookie
    int 0xC0
    mov ax, [cs:arena_svc_index]
    cmp byte [cs:arena_svc_fail], 0
    pop dx                        ; POP leaves the flags alone
    jne .none
    clc
    ret
.none:
    stc
    ret

; The V86-side single-page free is gone: both callers (ef_alloc's unwind and
; ef_free's walk) now hand whole chains to the monitor, which frees pages with
; ems_page_free32 while it already has the chain in hand.

; Logical page BX of the handle at SI -> CX = backing EMS page index.
; Preserves AX/BX/DX/SI/DI/BP.
;
; The walk itself is arena_svc's .resolve in the monitor, along with the
; (logical, backing) slot cache that keeps a forward sequential sweep O(L)
; rather than O(L^2); ems_link is in the system window and 16-bit code cannot
; name it. This is one monitor entry per EMS 44h, not one per chain link -- and
; 44h already made one for the frame remap, so the map path goes from one entry
; to two, not from one to L.
ems_backing_of:
    push dx
    mov [cs:arena_svc_index], si
    mov [cs:arena_svc_count], bx
    mov byte [cs:arena_svc_op], ASVC_EMS_RESOLVE
    mov dx, 0x4154                ; 'TA' monitor-call cookie
    int 0xC0
    mov cx, [cs:arena_svc_index]
    pop dx
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
; directly, at ring 0. That is now the ONLY route: the monitor runs every V86
; guest at real IOPL 3, so `INT n` is never IOPL-sensitive and monitor_body's
; sensitive-instruction emulation is gone (an INT that faults there today is
; the 0xD6 tripwire, not a dispatch path).
; Every previously-null slot below was safe only because IOPL was pinned at 0;
; it no longer is. A null gate's selector field is 0x0000: deliver_exception's
; final `load_segment(bus, Cs, 0)` raises a fatal CpuError::GeneralProtection
; that unwinds out of the emulator entirely, not a re-entrant #GP the monitor
; can catch. So every slot must hold a real gate now. deflt_N/deflt_common give
; every currently-null vector the same treatment exc_de/exc_ud/exc_nm already
; use: bounce it to the guest's own real-mode IVT handler, matching how real
; hardware would have serviced a software INT anyway. deflt_common routes
; through irq_body rather than reflecting directly, so a hardware IRQ remapped
; onto one of these vectors is classified by frame origin first -- see the
; routing rule above irq_body.
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
    IDTGATE int67_entry           ; 0x67 EMS/VCPI: THE dispatch route for a
                                  ;      guest INT 67h -- at real IOPL 3 the
                                  ;      INT is not IOPL-sensitive and comes
                                  ;      straight here. AH=DEh -> the monitor's
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
    mov ecx, (TABLES_BYTES + SYS_RESV) / 4
    rep stosd                     ; covers the paging tables AND the system
                                  ; window's page table and data pages, which
                                  ; is the only thing that zeroes the latter
    ; PD[0..15] -> the sixteen PTs that follow the PD (each PT maps 4 MiB), so
    ; the identity map covers 0..64 MiB and the XMS-move memcpy can reach every
    ; EMB.
    lea eax, [ebp + 0x1000]       ; first PT linear = PD + 0x1000
    or eax, 7
    mov edi, ebp                  ; write PD entries
    mov ecx, 16
.pde:
    mov [edi], eax
    add eax, 0x1000               ; next PT is one page further
    add edi, 4
    loop .pde
    lea edi, [ebp + 0x1000]       ; 16384 entries (0..64 MiB), present/rw/user
    mov eax, 7
    mov ecx, 16384
.pt:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .pt
    ; The system window: one PDE at SYS_LIN_BASE >> 22 pointing at its own page
    ; table, whose first SYS_BYTES/4096 entries map the reserved data pages. Not
    ; present in any client's page directory by construction -- that is the
    ; point of putting the bitmaps here rather than in the resident core.
    lea eax, [ebp + SYS_PT_OFF]
    or eax, 7
    mov [ebp + (SYS_LIN_BASE >> 22) * 4], eax
    lea edi, [ebp + SYS_PT_OFF]   ; fill the window's own page table
    lea eax, [ebp + SYS_DATA_OFF]
    or eax, 7
    mov ecx, SYS_BYTES >> 12
.syspt:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .syspt
    ; ems_link shipped as `times EMS_MAX_PAGES dw 0xFFFF` when it was in the
    ; file; the window is zero-filled instead, and 0 is a VALID page index while
    ; 0xFFFF is the chain terminator. ef_alloc does write every link before
    ; anything reads it, so this is belt and braces -- but a zeroed table claims
    ; every page is chained to page 0, which is some other handle's memory, and
    ; that is not a state to leave reachable by a future bug. Written by linear
    ; address: paging is not on yet, and DS is flat either way.
    lea edi, [ebp + SYS_DATA_OFF + SYS_EMS_LINK]
    mov eax, 0xFFFFFFFF
    mov ecx, EMS_MAX_PAGES / 2    ; two link words per stored dword
    rep stosd
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
    ; The frame carries REAL IOPL 3, which is the whole mechanism: at IOPL 3 in
    ; V86 the CPU stops treating CLI/STI/PUSHF/POPF/INT n/IRET as sensitive, so
    ; they execute for real and the guest's IF *is* the real IF. That is what
    ; the reference monitors do (386MAX `@VMIOPL equ 3`, QMAX_DTE.INC; JEMM runs
    ; its clients at real IOPL 3), and it is what the VCPI spec S4.0 requires
    ; when it says the IOPL-sensitive instructions must be "available".
    ;
    ; Extenders PROBE for it. DOS16M (the DOS4G loader) reads the flags image to
    ; classify real-mode vs V86-under-a-monitor, and an IOPL-0 image sent it
    ; down its raw LGDT mode-switch path, which is fatal under any monitor.
    ; The old monitor forged IOPL 3 into every PUSHF/PUSHFD image while real
    ; IOPL stayed 0; now the image is simply correct and the forgery is gone.
    ; Nothing at CPL 3 can architecturally change IOPL (load_flags preserves it
    ; on any CPL != 0 load), so a guest POPF/IRET cannot lower it back to 0 and
    ; escape.
    push dword 0x00023202         ; EFLAGS: VM | IOPL 3 | IF(real) | bit1
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

; ---- vector 13: #GP (error-code frame, from V86 OR from the monitor's own
; ring 0) OR any no-error-code delivery on vector 13 (IRQ5 at the default
; master base, or a guest INT 0Dh -- the fork does not distinguish them, and
; per the routing rule above irq_body it does not need to). V86 trap tax
; Part 2. The discriminator forks on FRAME SHAPE, read from the frame itself --
; never on the error-code VALUE, and with no opcode peek and no PIC probe:
;
;   error-code frame:  [esp+32]=EC  [esp+36]=EIP [esp+40]=CS     [esp+44]=EFLAGS
;   no-error frame:    [esp+32]=EIP [esp+36]=CS  [esp+40]=EFLAGS
;
; TEST 1: bit 17 (VM) of [esp+40]. In an error-code frame that slot holds
; CS, a zero-extended 16-bit value in BOTH origins (a V86 segment, or the
; monitor's own 0x08), so bit 17 can never be set; in a no-error frame it
; holds the interrupted EFLAGS. Set -> a V86 no-error frame, at ANY
; interrupted IP (the IP == 0 ambiguity the old error-code-value scheme
; needed an opcode peek and a PIC probe for does not exist in this basis,
; and neither does that scheme's documented mis-emulation residual).
;
; TEST 2 (clear): the ring-0 sti;hlt window below is the only ring-0 code
; that runs with IF open, and it brackets itself in `r0_hlt`. Flag set AND
; the no-error frame's CS slot ([esp+36]) == our 0x08 -> the no-error frame
; that woke the halt; irq_body's own VM check then parks it in the halt slot.
; A ring-0 #GP raised INSIDE the flagged window cannot take this arm: its
; frame carries the faulting EIP at [esp+36], and offset 8 is the DOS
; device-driver header (dh_next), never executed code.
;
; TEST 3 (remaining = error-code frame): bit 17 of [esp+44], the frame's
; real EFLAGS. Set -> a genuine V86 #GP -> monitor_body (whose dispatch
; re-reads the opcode anyway). Clear -> the monitor faulted on ITSELF.
; Report and stop: the stage-1 corpus triage (2026-08-17, G1) caught the
; old scheme routing exactly this frame to the IRQ path, where the IRETD
; popped three dwords off a four-dword frame and re-faulted 615 times,
; ESP marching through the driver's own tables until exception delivery
; itself died (baroll, SpacPlum, MontyNrm). Any handling here that touches
; the frame re-faults the same way; the only correct move is the named
; diagnostic exit. The same fork guards the OTHER PIC-base-8 collisions:
; irq_body classifies error-code frames on vectors 8/10/11/12/14 (0xD5 for
; a ring-0 origin -- and the deflt_* gates report there too, since
; deflt_common routes through irq_body), and reflect_vector refuses ring-0
; frames outright (0xD4) as the backstop for the exc_de/exc_ud/exc_nm reflect
; paths, which are the only ones left that reflect unconditionally. monitor_body
; adds 0xD6: an IOPL-sensitive instruction faulted at all, which means the
; V86 frame's IOPL is not 3 -- a monitor bug, not a guest one. irq_body adds
; 0xD7 for a doubly-occupied halt-window slot.
;
; EMULATOR-CONTRACT NOTE: TEST 1 is airtight because deliver_exception
; pushes CS zero-extended and never pushes an error code for an external
; interrupt (is_external=true), the ONLY way IRQ5 reaches this vector. The
; vector-13 #GP/IRQ collision itself is this emulator's PIC-base-arithmetic
; artifact, not real-silicon behavior. Revisit if deliver_exception's push
; order or the is_external gating ever changes. The old scheme additionally
; required every V86-origin #GP to push EC == 0; this basis does not, but
; deliver_exception's debug_assert for it remains as a contract tripwire.
vec13_entry:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    test dword [esp+40], FLAGS_VM ; TEST 1: no-error frame's EFLAGS.VM
    jnz .vec13_noerror            ; (an error-code frame holds 16-bit CS here)
    cmp byte [fs:r0_hlt], 0       ; TEST 2: the ring-0 halt window is the only
    je .ec_frame                  ; IF-open ring-0 code
    cmp dword [esp+36], 8         ; no-error ring-0 frame: CS slot = our 0x08
    je .vec13_noerror             ; -> the no-error frame that woke the hlt
.ec_frame:
    test dword [esp+44], FLAGS_VM ; TEST 3: error-code frame's EFLAGS.VM
    jnz monitor_body              ; V86 #GP -> emulate/reflect as ever
    mov al, 0xD3                  ; ring-0 #GP: the monitor faulted on itself
    jmp signal32                  ; (G1 storm iteration 0) -- report, stop
; TEST 1 decides FRAME SHAPE (no-error vs error-code), never origin -- hence
; the name. Calling this arm ".irq5" or ".vec13_external" would re-assert the
; software-vs-hardware claim the routing note above irq_body shows is neither
; knowable nor needed: at IOPL 3 a guest `INT 0Dh` and a hardware IRQ5 both
; arrive here as byte-identical no-error frames. EBX is the vector this gate
; sits on, 13, for both -- which is the vector the guest should see in both
; cases, so the question never has to be answered.
.vec13_noerror:
    mov ebx, 13
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
    ; The IOPL-sensitive set. The guest runs at real IOPL 3 (pm_init,
    ; vcpi_pm_to_v86), where CLI/STI/PUSHF/POPF/INT n/IRET are NOT
    ; IOPL-sensitive: the CPU executes them for real and they never fault here.
    ; So reaching this arm means the V86 frame's IOPL is not 3 -- a monitor bug
    ; in whoever built that frame, never a guest one. Emulating it would hide
    ; the defect behind a monitor that silently half-works, so name it and stop.
    cmp dl, 0xFA                  ; CLI
    je .sensitive_at_iopl0
    cmp dl, 0xFB                  ; STI
    je .sensitive_at_iopl0
    cmp dl, 0x9C                  ; PUSHF
    je .sensitive_at_iopl0
    cmp dl, 0x9D                  ; POPF
    je .sensitive_at_iopl0
    cmp dl, 0xCD                  ; INT n (dispatches through the static IDT
    je .sensitive_at_iopl0        ; now: deflt_*/int67_entry/intc0_entry)
    cmp dl, 0xCF                  ; IRET
    je .sensitive_at_iopl0
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

; ---- 66-prefixed forms. PUSHFD/POPFD/IRETD are IOPL-sensitive in V86 exactly
; like their 16-bit forms, so at IOPL 3 they too execute for real and cannot
; fault here; reaching one is the same monitor bug the unprefixed set reports
; (0xD6). The prefix byte is at [eax], the opcode at [eax+1]. Anything else
; 66-prefixed stays a diagnostic exit, with AL = the second byte (not 0x66)
; so the next gap names itself. ----
.prefix66:
    mov dl, [eax+1]
    cmp dl, 0x9C                  ; PUSHFD
    je .sensitive_at_iopl0
    cmp dl, 0x9D                  ; POPFD
    je .sensitive_at_iopl0
    cmp dl, 0xCF                  ; IRETD
    je .sensitive_at_iopl0
    mov ebx, 13                   ; unhandled 66-prefixed op: reflect INT 0Dh
    call reflect_vector           ; like the unprefixed catch-all (DOS16M's
    jmp .done_gp                  ; o32 LGDT prep lands here); frame IP still
                                  ; points at the 66 byte, fault semantics
; The 66-prefixed .pushfd/.popfd/.iretd_op bodies lived here and are gone for
; the same reason as the unprefixed six. The DOS16M rationale that justified
; .pushfd's forged IOPL-3 image now lives at pm_init's IRETD, where it explains
; why the frame carries REAL IOPL 3 -- the forgery it used to describe is
; deleted because the image is simply true.

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
; ---- The IOPL-3 tripwire. See the dispatch comment above: an IOPL-sensitive
; opcode can only fault into this monitor if the V86 frame it came from does
; not carry IOPL 3. Nothing the guest can do produces that -- load_flags
; preserves IOPL on every CPL != 0 load, so a guest POPF/IRET cannot lower it.
; It is therefore a defect in a monitor-built frame, and the machine stops. ----
.sensitive_at_iopl0:
    mov al, 0xD6
    jmp signal32
; The six single-byte IOPL-sensitive arms (.cli/.sti/.pushf/.popf/.intn/
; .iret_op) lived here. They are gone with the IOPL-3 switch: the CPU executes
; all six for real now. INT 67h and INT 0xC0 -- the two the monitor SERVES --
; arrive through their own static-IDT gates instead (int67_entry, intc0_entry),
; which perform the same AH=DEh and DX-cookie splits against a software-INT
; frame that is already past the instruction.
.hlt:
    inc word [ebp]             ; return IP = past the F4 byte (HLT is 1 byte)
    ; Real HLT is CPL-gated (a V86 task is always CPL 3), so the CPU now #GP(0)s
    ; every guest HLT into this monitor. Give the guest real halt semantics: run
    ; the actual `sti; hlt` at ring 0 so the CPU's own HLT/wake logic idles the
    ; machine, then IRET back to the guest just past the F4 byte. The IDT is the
    ; same table in both V86 and ring 0 (idt/idtr above), so any interrupt that
    ; fires during this real HLT vectors straight into irq_m*/irq_s*/vec13_entry
    ; exactly as it would have for the guest -- irq_body's .hlt_wake arm parks
    ; it in the bounded halt slot (it arrives on a ring-0 frame reflect_vector
    ; must refuse) and the drain below reflects it into the guest's IVT.
    ;
    ; Guest IF=0 (interrupts disabled): a real 386 hangs forever on `HLT` with
    ; IF=0, woken only by NMI or reset -- NMI is not virtualized here, so a
    ; literal mirror would wedge the whole VM on a guest bug (or on a
    ; legitimate but IF=0 halt-until-NMI idiom this emulator doesn't model).
    ; Decision (documented, not a silent divergence): the run loop's own
    ; interrupt-pending wake (service_pending_interrupt) cannot fire with IF=0,
    ; so a faithful halt would block until something forces IF, which nothing
    ; here does. To avoid a permanent guest-visible wedge on ordinary FreeDOS
    ; idle loops (which always halt with IF=1 -- DOS brackets IRQ-sensitive
    ; code with CLI/STI, never idles under CLI), only the IF=1 path executes a
    ; real hlt; an IF=0 HLT resumes the guest immediately (equivalent to an
    ; instantaneous NMI/no-op wake), since no real game or DOS idle loop halts
    ; with interrupts masked and this monitor has nothing that will ever clear
    ; that state for it otherwise. The test is now against the guest's REAL
    ; flag in the frame rather than the deleted `vif` proxy; the divergence
    ; itself is unchanged.
    test dword [ebp+8], 0x200   ; frame EFLAGS.IF = the guest's own IF
    jz .done_gp
    mov byte [fs:r0_hlt], 1     ; vec13_entry's TEST 2: IRQ5 landing in this
                                ; window is the wake, not a #GP
    sti
    hlt                         ; wakes when service_pending_interrupt admits a
                                ; real IRQ. This hlt runs at ring 0 (VM=0), so
                                ; irq_body's frame-origin check cannot treat the
                                ; waking IRQ's 3-dword IRETD frame as a V86
                                ; frame; .hlt_wake stores it, leaves it in
                                ; service, clears IF in the frame it returns
                                ; through, and IRETDs straight back here.
    cli                         ; belt-and-braces: .hlt_wake already cleared bit
                                ; 9 in the waking frame, so IF is shut on
                                ; arrival. Kept because the fall-through from a
                                ; spurious wake (HLT can resume without a
                                ; delivered vector) is not covered by that.
    mov byte [fs:r0_hlt], 0
    cmp byte [fs:hlt_pending], 0 ; drain the slot into the guest's real V86
    je .done_gp                  ; frame, now that we are about to return
    movzx ebx, byte [fs:hlt_vector]
    mov byte [fs:hlt_pending], 0
    call reflect_vector_v86     ; EBX is the vector the waking gate sat on, so
                                ; it reflects exactly like irq_body's V86 arm.
                                ; EBP is the V86 frame; the origin is proven
                                ; (we only reach here from a V86 HLT).
    jmp .done_gp

; ---- Two-byte privileged 0F ops (386MAX QMAX_I0D GP_ESCOD, adapted to the
; Izarra3000). A V86 task is CPL 3, so the CPU #GP(0)s every MOV CRn/DRn,
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

; ---- Hardware IRQs (no error code). Per-line stubs load THE VECTOR THEY SIT
; ON and share one body, which reflects it to the guest IVT unconditionally:
; the guest runs at real IOPL 3, so a line the guest has not enabled is never
; acknowledged in the first place and never reaches a gate. Master lines 0-7
; (vectors 8-15, 5 via vec13_entry), slave lines 8-15 (vectors 0x70-0x77). ----
%assign line 0
%rep 8
irq_m%[line]:
    pushad
    mov ebx, 8 + line             ; the vector this gate occupies
    jmp irq_common
%assign line line+1
%endrep
%assign line 8
%rep 8
irq_s%[line]:
    pushad
    mov ebx, 0x70 + (line - 8)    ; the vector this gate occupies
    jmp irq_common
%assign line line+1
%endrep

irq_common:                       ; pushad done, EBX = vector
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
                                  ; falls through into irq_body

; ---- THE ROUTING RULE: EVERY GATE REFLECTS THE VECTOR IT SITS ON. ----
;
; The correctness argument is one sentence: the arriving vector IS the vector
; the guest should see, because the monitor's IDT and the guest's IVT are
; indexed by the same 256-entry vector space. That holds whether the entry was
; produced by the PIC's INTA or by a `CD nn` byte pair, so **no gate ever has
; to answer the software-INT-vs-hardware-IRQ question** -- which is fortunate,
; because on a 386 it is not answerable: both deliveries produce byte-identical
; no-error V86 frames, the ISR bit is necessary-but-not-sufficient for a
; hardware IRQ, and a frame-IF test only separates the IF=0 case. The one
; remaining signal is an opcode peek at CS:IP-2, and that scheme is recorded
; just above vec13_entry as ABANDONED, with a documented mis-emulation
; residual. Do not re-introduce a discriminator here.
;
; What this replaced: the gates used to discard the arriving vector and
; RECOMPUTE one as `vcpi_pic_master + line`. At the DOS default bases that is
; the identity and the bug was invisible; away from them it was wrong, and
; worse, `vcpi_pic_master`/`vcpi_pic_slave` are a CACHE of the chip's state
; that the chip can outrun -- the PIC ports are not in the TSS I/O bitmap (only
; 0x92 is), so a guest `OUT 0x20`/`OUT 0x21` reprograms the 8259 without the
; monitor seeing it. DE0B the master to 0x88, then direct-OUT it back to 8, and
; a real IRQ0 arriving at vector 8 was reflected to 0x88. Reflecting the
; arriving vector deletes that whole class rather than patching the arms that
; read the cache.
;
; MATCHED-METAL RESIDUE, deliberate: a guest that executes `INT 08h` while IRQ0
; genuinely is in service reflects to vector 8, and its handler EOIs a line
; that is in service, prematurely ending IRQ0's service. Real hardware does
; exactly this, for exactly the same reason -- the 8259 has no idea a `CD 08`
; ran. It is matched behaviour, not residue to be fixed later.
;
; irq_body is now purely a FRAME-ORIGIN classifier: it decides frame shape
; (V86 / ring-0 halt wake / error-code), never vector.
irq_body:                         ; vec13_entry joins here (segs already set)
    lea ebp, [esp + 32]
    test dword [ebp+8], FLAGS_VM  ; no-error frame's EFLAGS.VM; an ERROR-CODE
    jz .not_v86_irq               ; frame holds a 16-bit CS here (bit 17
                                  ; never set), so set can only be the V86
                                  ; IRQ frame
    ; A V86 IRQ frame can only EXIST when the guest had IF=1: at IOPL 3 the
    ; guest's own CLI clears the real flag, and service_pending_interrupt gates
    ; the INTA on it, so an interrupt taken under a guest CLI never happens --
    ; the request stays latched in the 8259A's IRR and is taken after the STI.
    ; So there is nothing to hold: reflecting is the only V86 outcome, and the
    ; `.go` label that used to name it is gone with the branch that chose it.
    ;
    ; What the deleted `.hold` was protecting, kept because it is expensive to
    ; re-derive: the EOI belongs to whoever services the interrupt, which under
    ; this monitor is always the guest's own IVT handler, and the handler must
    ; find its OWN LINE STILL IN SERVICE. An earlier revision EOI'd on the
    ; monitor's behalf and handed the guest a state the 8259A cannot produce;
    ; DJGPP's shared hardware-IRQ wrapper probes exactly that (OCW3 0x0B +
    ; IN 0x20) to tell a real IRQ from a spurious entry, read the 0 that path
    ; left behind, took its not-my-line branch, indexed its 16-entry per-IRQ
    ; old-vector table with 16, and RETF'd through the garbage pair it found --
    ; E10, the MonikaTT #GP(0) at 0xAF:78A3. Holding WITH the ISR bit set fixed
    ; that and is faithful for a VME or EMM386-class (IOPL-0) manager, which is
    ; what this monitor used to be -- under VME the INTA has already happened
    ; when the monitor gets control, and the AMD-K5 TRM S3.1.4 (VME) describes
    ; exactly this: with VIF clear, "the operating system holds the interrupt
    ; pending", saving the vector and setting VIP. Reference, in
    ; dev_docs/reference/Pentium-K6/ :
    ;     AMD-K5_Processor_Technical_Reference_Manual_(November_1996).txt
    ;
    ; So the old architecture was a software VME, not an invented state. It
    ; needed a complete set of drain points, and the VCPI DE0C mode switch
    ; destroyed the drainer. Running at IOPL 3 deletes the question instead of
    ; completing the drain set.
    call reflect_vector_v86
    popad
    iretd
; Bit 17 clear: either the IRQ that woke the ring-0 sti;hlt window (a
; no-error frame, CS slot = our 0x08, only under the r0_hlt bracket --
; reflect_vector must never run against that 3-dword frame, so park it in
; the bounded halt slot and let .hlt drain it), or an
; ERROR-CODE frame: vectors 8/10/11/12/14 on the master gates are also
; #DF/#TS/#NP/#SS/#PF, the same PIC-base-8 collision vector 13 has with
; IRQ5, and the same frame-shape fork decides it. The old code fell into
; .hold for every bit-17-clear frame; for an error-code frame that EOI'd a
; line that never fired and IRETD'd three pops off a longer frame -- the
; G1 storm mechanism on the OTHER collision vectors.
.not_v86_irq:
    cmp byte [fs:r0_hlt], 0
    je .ec_frame
    cmp dword [ebp+4], 8          ; no-error ring-0 frame: CS slot = monitor CS
    je .hlt_wake                  ; -> the hlt wake
.ec_frame:
    test dword [ebp+12], FLAGS_VM ; error-code frame's EFLAGS.VM
    jz .ring0_exc
    ; A V86-origin CPU exception collided onto this IRQ gate: reflect it to
    ; the guest IVT entry for the EXCEPTION vector, fault semantics, and no EOI
    ; -- no PIC line is in service. EBX is ALREADY that vector: every gate
    ; carries the vector it sits on, and a colliding exception is by definition
    ; the one whose vector equals this gate's. (This used to read `add ebx, 8`,
    ; converting a master line to its default-base vector -- the same number,
    ; arrived at by arithmetic that only held at the default base.)
    lea ebp, [ebp+4]              ; &frame.eip of the error-code frame
    call reflect_vector
    popad
    add esp, 4                    ; discard the error code
    iretd
.ring0_exc:
    mov al, 0xD5                  ; a ring-0 CPU exception landed on an IRQ
    jmp signal32                  ; gate: the monitor faulted on itself
; ---- The bounded halt-window slot. `.hlt` runs a real `sti; hlt` at ring 0,
; so the waking IRQ lands on a 3-dword ring-0 frame that reflect_vector must
; refuse. Park it here and let `.hlt` drain it into the guest's real V86 frame
; on the way out. This is NOT the old `vip`: its lifetime is a monitor-internal
; window the guest cannot extend, and it is drained unconditionally before the
; next V86 entry.
;
; ONE SLOT IS PROVABLY SUFFICIENT, and 0xD7 is the contract assert on the proof
; rather than the reason it is safe:
;   1. service_pending_interrupt takes at most ONE vector per call and returns
;      immediately after delivery;
;   2. delivery is through an interrupt gate, so this arm runs with IF already
;      0 -- no second interrupt can arrive while the slot is being written;
;   3. the only boundary at which a second interrupt could be accepted is the
;      one AFTER the wake's IRETD returns into the halt window, and the frame-IF
;      clear below shuts exactly that boundary.
; No interleaving can occupy the slot twice. If 0xD7 ever fires one of those
; three premises is false and the machine must stop, not improvise.
.hlt_wake:
    cmp byte [fs:hlt_pending], 0
    jne .hlt_slot_busy
    mov [fs:hlt_vector], bl       ; EBX as it arrived (see hlt_vector)
    mov byte [fs:hlt_pending], 1
    and dword [ebp+8], ~0x200     ; clear bit 9 in the frame we IRETD through,
                                  ; so IF is already shut when control lands
                                  ; back in the halt window (premise 3)
    popad
    iretd
.hlt_slot_busy:
    mov al, 0xD7
    jmp signal32

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
; for (1-5, 16, 18-0x6F, 0x78-0xFF -- see the idt: comment above). These are
; the UNIVERSAL dispatch route for a guest software INT: the monitor runs its
; V86 guests at real IOPL 3, so INT n is never IOPL-sensitive and always lands
; on the IDT slot it names. (Historically these slots were unreachable -- IOPL
; was pinned at 0 and every guest INT/IRET/PUSHF/POPF trapped through
; vec13_entry into monitor_body's emulation first; then the PRM-correct
; load_flags IOPL fix made them reachable for the rare guest that raised its
; own IOPL to 3, and the IOPL-3 monitor made that the only case there is.)
; Reflect exactly like exc_de/exc_ud/exc_nm: bounce to the guest's own
; real-mode IVT handler, the same thing real hardware's IDT-driven INT dispatch
; would have done. No EOI -- these are software INTs / CPU traps, not PIC
; lines. ----
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
    ; Every gate now hands irq_body a vector and a frame, and irq_body is
    ; purely a frame-origin classifier -- so this is an unconditional jump, not
    ; a reflect. It must NOT be a bare reflect_vector: a hardware IRQ remapped
    ; onto a deflt_* vector can still wake the ring-0 sti;hlt, and
    ; reflect_vector would refuse that 3-dword ring-0 frame with signal32 0xD4.
    ;
    ; Two consequences that look like side effects and are not:
    ;   * deflt_ac improves. Vector 17 is the one default-covered vector whose
    ;     CPU-exception form carries an error code. The old path called
    ;     reflect_vector against a frame it assumed was 3 dwords; irq_body's
    ;     .ec_frame arm classifies by frame SHAPE first, so a genuine #AC is no
    ;     longer at risk of being mis-popped. The emulator never sets CR0.AM, so
    ;     this is latent-only -- but it is a strict improvement.
    ;   * The 0xD4 backstop narrows, deliberately. reflect_vector's ring-0
    ;     refusal used to guard these reflects too; they now report 0xD5 through
    ;     irq_body's .ring0_exc arm instead, leaving 0xD4 guarding only
    ;     exc_de/exc_ud/exc_nm. Same stop-the-machine semantics, different code
    ;     -- a reader chasing a 0xD5 must know the default gates land there.
    jmp irq_body

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

; ---- Vector 0x67 (EMS + VCPI, which share the INT). THE dispatch route for
; a guest INT 67h: the guest runs at real IOPL 3, so the INT is not
; IOPL-sensitive and goes straight through this static IDT gate. AH=DEh -> the
; monitor-side VCPI server; anything else reflects to the guest's own IVT
; handler (the V86 EMS driver) exactly like a deflt_ gate. The frame's saved
; CS:IP already points PAST the INT instruction (software-INT gate dispatch),
; so no IP advance -- the IOPL-0 fault frame that pointed AT it, and the
; monitor_body arm that compensated, are both gone. ----
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

; ---- Vector 0xC0 (TOKAEMM-private monitor calls). THE dispatch route for a
; guest INT 0xC0: at real IOPL 3 the INT is not IOPL-sensitive and arrives
; straight through this static IDT gate. The frame's saved CS:IP is already
; PAST the INT (software-INT gate dispatch), so no IP advance -- the IOPL-0
; fault frame that pointed AT it, and the monitor_body arm that compensated,
; are both gone. A foreign INT 0xC0 reflects to the guest's own handler. ----
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
    cmp word [esp+20], 0x4154     ; ... 'TA' (arena allocator)?
    je .arena
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
.arena:
    call arena_svc
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
;   No IP advance anywhere: int67_entry is the only caller, and its
;   software-INT gate frame already points PAST the INT. (The IOPL-0 fault
;   frame that pointed AT it, and the monitor_body arm that compensated, are
;   both gone.) Guest register writes go through the pushad block; live
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
;
; THAT LAST CLAUSE IS A CONSTRAINT, NOT AN OBSERVATION. Every structure the CPU
; reads implicitly while a guest runs -- the TSS (SS0/ESP0 on a ring change, and
; the I/O permission bitmap on every V86 port access), the GDT, the IDT and the
; ring-0 stack -- must live inside linear 0..0x10FFFF, because the CPU reads
; them under whatever CR3 is live and the monitor does not choose all of those.
; A VCPI client installs its own via DE0C, and the GUEST can install one of its
; own too: MOV CR3 from V86 faults #GP, monitor_body routes it to .op_mov_cr_r,
; and .wcr3 honours it before .done_gp's IRETD returns to V86 with that paging
; live. Only these 0x110 furnished entries are guaranteed present in such a
; context. A server structure above 1 MB would take a #PF whose own delivery
; needs that same structure: a triple fault, under a zero-limit IDT, with
; nothing to read afterwards.
;
; Two attempts to move the TSS out of the driver image were cut on exactly this
; (2026-08-06). Do not move any of these structures up without re-deriving it.
; The server page tables may be reserved above 1 MB, but DE0C reads pd_lin
; from low server data before switching back to the server CR3.
;
; The same bound pins arena_bmp and vcpi_bmp, which is less obvious because they
; are plain data rather than fault-delivery machinery. DE01 copies exactly 0x110
; page-table entries (below), so a client running under its own CR3 has linear
; 0..0x110000 mapped and nothing above it. DE03/DE04/DE05 arrive HERE, under
; that CR3, and run the same arena_query32/vcpi_page_alloc/vcpi_page_free bodies
; the V86 path does. A bitmap at ARENA_PHYS_BASE (0x138000) would take a #PF in
; the client's world, and a data selector based there would not exist in the
; client's GDT either -- only CS, CS+8 and CS+16 are furnished. Trapping to the
; monitor does not rescue it: these callers are not V86 code and cannot trap at
; all. Two proposals to move those two bitmaps into the high reservation, to buy
; back what the 64 MB arena cost the resident core, were cut on this
; (2026-08-08). Only ems_link is movable that way, because EMS is INT 67h
; AH=40h-4Dh and never reaches this entry point. vcpisw.asm exercises the
; DE04/DE05 protected-mode path for real, so this is not an untested corner.
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
    ; No boundary release is needed here any more. The interim fix disclaimed
    ; lines held in `vip` with specific EOIs before handing the machine to the
    ; client, because a line held across this switch was held forever (the
    ; drainer only ran from a V86 sensitive-op trap, and the client never
    ; returns to that world) and a stuck IS0 inhibits the whole chip. At IOPL 3
    ; the monitor never acknowledges a line it cannot deliver, so there is
    ; nothing held at this boundary and nothing to give back -- which is also
    ; why metal never needed this site.
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
    ; KNOWN INERT HOLE, left deliberately (2026-08-06). Falling through here
    ; without an LTR leaves TR still selecting OUR 0x18 while the CLIENT's CR3
    ; is live. Nothing in the tree reaches it: the VCPI spec says a client
    ; always furnishes a TSS, vcpisw.asm furnishes 0x18, and a client runs at
    ; CPL 0 so nothing reads the TSS body. It is recorded rather than turned
    ; into a signal32 because a loud failure on a path no fixture exercises
    ; could break a real client that legitimately passes TR=0, which would be
    ; trading a latent non-issue for an active one.
    ;
    ; IF A GAME OR EXTENDER EVER MISBEHAVES AFTER A DE0C SWITCH, START HERE:
    ; put a breakpoint or a signal32 on .no_tr and see whether it is taken.
    ; The symptom would be a fault or a hang shortly after the client's first
    ; ring transition or task switch, not at the switch itself.
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
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], esi
    jnc .span_start
    inc esi                       ; skip an allocated granule
    jmp .next_span
.span_start:
    mov eax, esi                  ; eax = span start
.span_scan:
    inc esi
    cmp esi, ecx
    jae .span_end
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], esi
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
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    jc .next
    inc ebx
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    jc .next
    inc ebx
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    jc .next
    inc ebx
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
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
    bts [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    bts [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    bts [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    bts [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    bts [SYS_LIN_BASE + SYS_VCPI_BMP], eax  ; record VCPI as this page's owner
                                  ; (flat DS + absolute displacement: the
                                  ; system window, see SYS_LIN_BASE)
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
    bt [SYS_LIN_BASE + SYS_VCPI_BMP], eax
    jnc .bad
    btr [SYS_LIN_BASE + SYS_VCPI_BMP], eax
    mov ebx, eax
    shl ebx, 2
    btr [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    btr [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    btr [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
    inc ebx
    btr [SYS_LIN_BASE + SYS_ARENA_BMP], ebx
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

; ---- The one arena allocator, ring-0 side (INT 0xC0 'TA'). ----
; Straight ports of the 16-bit originals that used to live in tokaemm-xms.inc;
; the structure, the 386MAX lineage and the boundary arithmetic are unchanged,
; only the addressing moved from `cs:arena_bmp` to the system window. They must
; run here because arena_bmp is above anything a 16-bit offset can name.
;
; Reached ONLY from V86 through INT 0xC0, never from vcpi_pm_entry, so DS is
; always the flat 0x10 both dispatch sites load -- no CR3 concern.

; Allocate ECX granules for allocation type EBX (an ALLOC_* byte offset).
; out: AX = first granule, CF clear; or CF set when no run of that size starts
; on that boundary. A zero-granule request succeeds at granule 0 without marking
; anything, which is what a zero-KB XMS block has always received.
arena_alloc32:
    jecxz .empty
    movzx edi, word [fs:alloc_lim + ebx]   ; boundary - 1
    xor esi, esi                           ; candidate granule
.align:
    add esi, edi                           ; round the candidate up
    mov eax, edi
    not eax
    and esi, eax
    movzx edx, word [fs:arena_granules]    ; reloaded per restart: every GPR is
    mov eax, esi                           ; spoken for and allocation is rare
    add eax, ecx                           ; does the run fit in the arena?
    cmp eax, edx
    ja .none
    mov edx, esi                           ; probe [esi, esi+ecx)
    mov ebx, ecx
.probe:
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], edx
    jc .busy
    inc edx
    dec ebx
    jnz .probe
    mov eax, esi
    call arena_mark32                      ; preserves eax/ecx
    clc
    ret
.busy:
    lea esi, [edx+1]                       ; restart past the blocking granule
    jmp .align
.empty:
    xor eax, eax
    clc
    ret
.none:
    stc
    ret

; Mark / release ECX granules from granule EAX. Preserve every register. Bump
; arena_gen (D1): every cached query is invalid the instant the bitmap changes.
arena_mark32:
    push eax
    push ecx
    jecxz .done
.next:
    bts [SYS_LIN_BASE + SYS_ARENA_BMP], eax
    inc eax
    dec ecx
    jnz .next
    inc dword [fs:arena_gen]
.done:
    pop ecx
    pop eax
    ret

arena_release32:
    push eax
    push ecx
    jecxz .done
.next:
    btr [SYS_LIN_BASE + SYS_ARENA_BMP], eax
    inc eax
    dec ecx
    jnz .next
    inc dword [fs:arena_gen]
.done:
    pop ecx
    pop eax
    ret

; Return the 16 KB EMS page at index EAX to the arena. Preserves EAX.
ems_page_free32:
    push eax
    push ecx
    shl eax, 4                    ; EMS page index -> granule index
    mov ecx, EMS_PAGE_GRANULES
    call arena_release32
    pop ecx
    pop eax
    ret

; Take one 16 KB EMS page from the shared arena. out: AX = page index, CF clear;
; or CF set when none is free. EMS page p occupies granules [p*16, p*16+16).
;
; The ems_cursor next-fit is not decoration (D6): without it, taking N pages one
; at a time rescans every already-taken low page each call, so a single AH=43h
; asking for most of the pool -- a RAM disk claiming all of EMS does exactly
; this -- cost O(N^2) bit tests and could hold interrupts off across multiple
; 54.9 ms IRQ0 ticks. And no EMS-private free chain: it would have to stay in step with
; grabs XMS or VCPI make out of the SAME bitmap.
ems_page_alloc32:
    movzx ecx, word [fs:arena_granules]
    shr ecx, 4                    ; whole 16 KB pages the arena covers
    jz .none
    movzx eax, word [fs:ems_cursor]
    mov edx, ecx                  ; candidate pages left to examine
.scan:
    cmp eax, ecx
    jb .test
    xor eax, eax                  ; wrap to the arena base
.test:
    mov esi, eax
    shl esi, 4                    ; first granule of this candidate page
    mov ebx, EMS_PAGE_GRANULES
.probe:
    bt [SYS_LIN_BASE + SYS_ARENA_BMP], esi
    jc .next
    inc esi
    dec ebx
    jnz .probe
    jmp .take
.next:
    inc eax
    dec edx
    jnz .scan
.none:
    stc
    ret
.take:
    lea edx, [eax+1]
    mov [fs:ems_cursor], dx       ; next-fit: resume just past this page
    push eax
    mov esi, eax
    shl esi, 4                    ; recompute the page's FIRST granule (the
    mov eax, esi                  ; probe above left esi at its end)
    mov ecx, EMS_PAGE_GRANULES
    call arena_mark32
    pop eax
    clc
    ret

; The service itself. Sub-function in [arena_svc_op]; see the block beside
; arena_q_type for why everything crosses in memory rather than registers.
arena_svc:
    push ebp                      ; VESTIGIAL, kept deliberately: the only
    movzx eax, byte [fs:arena_svc_op]   ; caller is intc0_entry's .arena, which
                                  ; never writes ebp and whose popad restores
                                  ; it regardless, so this preserves nothing
                                  ; anyone needs. It dates from the IOPL-0
                                  ; entry path, which did park &frame.eip in
                                  ; ebp. Left as calling-convention
                                  ; conservatism -- the jump table below is
                                  ; shared, and a future caller that DOES hold
                                  ; something in ebp gets it back for free.
    cmp eax, ASVC_MAX
    ja .bad
    call dword [fs:.jt + eax*4]   ; call, not jmp: the pop below must run.
    pop ebp                       ; Offsets are driver-relative == CS-relative,
    ret                           ; the same table form vcpi_dispatch uses
.bad:
    mov byte [fs:arena_svc_fail], 1
    pop ebp
    ret
.jt:
    dd .alloc, .release, .mark, .ems_alloc
    dd .take, .give, .resolve, .next
.alloc:
    movzx ebx, byte [fs:arena_svc_type]
    movzx ecx, word [fs:arena_svc_count]
    call arena_alloc32
    jc .fail
    mov [fs:arena_svc_index], ax
    mov byte [fs:arena_svc_fail], 0
    ret
.ems_alloc:
    call ems_page_alloc32
    jc .fail
    mov [fs:arena_svc_index], ax
    mov byte [fs:arena_svc_fail], 0
    ret
.fail:
    mov byte [fs:arena_svc_fail], 1
    ret
.release:
    movzx eax, word [fs:arena_svc_index]
    movzx ecx, word [fs:arena_svc_count]
    call arena_release32
    mov byte [fs:arena_svc_fail], 0
    ret
.mark:
    movzx eax, word [fs:arena_svc_index]
    movzx ecx, word [fs:arena_svc_count]
    call arena_mark32
    mov byte [fs:arena_svc_fail], 0
    ret

; ASVC_EMS_TAKE. in: arena_svc_index = ems_table slot offset,
; arena_svc_count = pages wanted. Builds the handle's chain; on failure gives
; every page back and reports fail. The caller has already range- and
; free-checked (D3), so the failure path is a safety net, not the live path.
.take:
    mov ax, [fs:arena_svc_index]
    mov [fs:ems_svc_slot], ax
    mov ax, [fs:arena_svc_count]
    mov [fs:ems_svc_left], ax
    movzx esi, word [fs:ems_svc_slot]
    mov word [fs:esi+4], 0xFFFF   ; empty chain
    mov word [fs:ems_svc_tail], 0xFFFF
.tk_next:
    call ems_page_alloc32         ; -> AX = page index, or CF. Clobbers every
    jc .tk_unwind                 ; register a loop would want; hence the
    movzx edi, ax                 ; memory-held loop state
    ; new tail terminates the chain
    mov word [SYS_LIN_BASE + SYS_EMS_LINK + edi*2], 0xFFFF
    movzx edx, word [fs:ems_svc_tail]
    cmp dx, 0xFFFF
    je .tk_head
    mov [SYS_LIN_BASE + SYS_EMS_LINK + edx*2], ax ; link the old tail to it
    jmp .tk_linked
.tk_head:
    movzx esi, word [fs:ems_svc_slot]
    mov [fs:esi+4], ax
.tk_linked:
    mov [fs:ems_svc_tail], ax
    dec word [fs:ems_svc_left]
    jnz .tk_next
    movzx esi, word [fs:ems_svc_slot]
    mov word [fs:esi+16], 0       ; cold cache (0 = cold; see .resolve)
    mov byte [fs:arena_svc_fail], 0
    ret
.tk_unwind:
    movzx esi, word [fs:ems_svc_slot]
    mov ax, [fs:esi+4]            ; give back every page we did take
    mov [fs:ems_svc_cur], ax
.tk_uw:
    movzx edi, word [fs:ems_svc_cur]
    cmp di, 0xFFFF
    je .tk_uw_done
    mov ax, [SYS_LIN_BASE + SYS_EMS_LINK + edi*2]
    mov [fs:ems_svc_cur], ax
    mov eax, edi
    call ems_page_free32
    jmp .tk_uw
.tk_uw_done:
    movzx esi, word [fs:ems_svc_slot]
    mov word [fs:esi+4], 0xFFFF
    mov byte [fs:arena_svc_fail], 1
    ret

; ASVC_EMS_GIVE. in: arena_svc_index = slot offset. Walks the chain and returns
; every page to the arena. The chain is the only record of what the handle
; holds -- backing runs are not contiguous, so there is no [first,first+npages)
; range -- which is why the release has to happen here. Frame-slot unmapping
; and saved_map scrubbing stay in ef_free: they touch ems_frame_map and
; ems_table only, both still in the resident core.
.give:
    mov ax, [fs:arena_svc_index]
    mov [fs:ems_svc_slot], ax
    movzx esi, ax
    mov ax, [fs:esi+4]
    mov [fs:ems_svc_cur], ax
.gv_page:
    movzx edi, word [fs:ems_svc_cur]
    cmp di, 0xFFFF
    je .gv_done
    mov ax, [SYS_LIN_BASE + SYS_EMS_LINK + edi*2]
    mov [fs:ems_svc_cur], ax
    mov eax, edi
    call ems_page_free32
    jmp .gv_page
.gv_done:
    movzx esi, word [fs:ems_svc_slot]
    mov word [fs:esi+4], 0xFFFF
    mov byte [fs:arena_svc_fail], 0
    ret

; ASVC_EMS_RESOLVE. in: arena_svc_index = slot offset, arena_svc_count =
; logical page. out: arena_svc_index = backing EMS page index. Resumes from the
; slot's (logical, backing) cache whenever the cache is at or before the wanted
; page, so a forward sequential sweep of an L-page handle costs O(L) in total
; rather than O(L^2).
;
; The cache at [slot+16]/[slot+18] stores cache_logical+1 (0 = cold) rather
; than a bare index with an 0xFFFF sentinel (D4): a raw-zeroed ems_table slot
; then already reads as cold, with no INIT-time sentinel fill needed.
.resolve:
    movzx esi, word [fs:arena_svc_index]
    movzx ebx, word [fs:arena_svc_count]  ; wanted logical page
    movzx ecx, word [fs:esi+4]            ; chain head
    xor eax, eax                          ; logical index ECX stands at
    movzx edx, word [fs:esi+16]
    test edx, edx
    jz .rs_walk                           ; cold cache
    dec edx                               ; decode: stored value is logical+1
    cmp edx, ebx
    ja .rs_walk                           ; cache is past us: eax/ecx still head
    mov eax, edx                          ; resume from the cache
    movzx ecx, word [fs:esi+18]
.rs_walk:
    cmp eax, ebx
    je .rs_done
    movzx ecx, word [SYS_LIN_BASE + SYS_EMS_LINK + ecx*2]
    inc eax
    jmp .rs_walk
.rs_done:
    inc eax                               ; encode: logical+1, 0 stays "cold"
    mov [fs:esi+16], ax
    mov [fs:esi+18], cx
    mov [fs:arena_svc_index], cx
    mov byte [fs:arena_svc_fail], 0
    ret

; ASVC_EMS_NEXT. in/out: arena_svc_index = a page index -> the next page in its
; chain. The one per-link round trip in the design, on EMS 45h release only,
; bounded by the pages the handle holds and on a path that already does a
; frame_remap per live slot.
.next:
    movzx eax, word [fs:arena_svc_index]
    movzx eax, word [SYS_LIN_BASE + SYS_EMS_LINK + eax*2]
    mov [fs:arena_svc_index], ax
    mov byte [fs:arena_svc_fail], 0
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
; Enter the SERVER's paging context from a client's. Interrupts must already be
; off. Costs ebp (saved on the client stack), ecx and eax (both already saved by
; every call site). On return SS:ESP is the monitor stack and the client's
; SS/ESP/CR3 are carried on it for VCPI_HOST_LEAVE.
;
; Why this exists: without it, everything DE03/DE04/DE05 touch has to be inside
; the window DE01 furnished, because the client's page tables are what is live.
; That pinned the arena bitmaps to the resident core, in conventional memory,
; where they scale with installed RAM. Switching CR3 for the duration of the
; call moves that constraint to "whatever the SERVER maps", which is everything.
;
; The stack must be swapped BEFORE CR3: the client's stack is at some linear
; address of its choosing, and under our identity map that same linear address
; is a different physical page. Our monitor stack is driver-resident, so it is
; in the first megabyte, which the DE01 contract maps identically in both
; contexts -- there is never an instruction where SS:ESP is invalid. (JEMM has
; to accept exactly such a window because its host stack is at 0xF8000000,
; unmapped in the client; ours does not.)
;
; SS is loaded from CS+8, the flat 4 GB data descriptor DE01 furnished in the
; CLIENT's GDT, so the load itself is legal before the switch. Restoring it
; afterwards is the reason LEAVE puts CR3 back FIRST: `mov ss` walks the
; client's GDT, and the client's GDT need not be mapped in our context.
%macro VCPI_HOST_ENTER 0
    push ebp
    push ds                       ; the client's DS; we need a FLAT one to
                                  ; reach the system window by absolute linear
                                  ; displacement, the way every V86-side
                                  ; monitor entry already does with DS = 0x10
    mov ebp, esp                  ; client ESP (at the saved ds)
    mov cx, ss                    ; client SS
    mov ax, cs
    add ax, 8                     ; flat data, from the client's own GDT
    mov ds, ax
    mov ss, ax
    mov esp, [fs:base_lin]
    add esp, mon_stack_top        ; the V86 task cannot be running here (we are
                                  ; a CPL0 far call, not a V86 fault), so the
                                  ; monitor stack is free
    push ecx                      ; carry the client's context across
    push ebp
    mov eax, cr3
    push eax
    mov eax, [fs:pd_lin]
    mov cr3, eax                  ; ---- server context live from here
%endmacro

%macro VCPI_HOST_LEAVE 0
    pop eax
    mov cr3, eax                  ; ---- client context live again, FIRST
    pop ebp                       ; client ESP
    pop ecx                       ; client SS
    mov ss, cx
    mov esp, ebp
    pop ds                        ; client DS, restored under the client's own
    pop ebp                       ; CR3 so the GDT walk resolves
%endmacro

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
    VCPI_HOST_ENTER
    mov bl, ALLOC_VCPI
    call arena_query32
    movzx edx, dx
    shr edx, 2
    VCPI_HOST_LEAVE
    pop ebx
    pop eax
    popfd
    xor ah, ah
    jmp .out
.de04:                            ; allocate a 4K page -> EDX = physical
    pushfd
    cli
    push eax                      ; AL must survive (only AH/EDX are outputs)
    VCPI_HOST_ENTER
    call vcpi_page_alloc          ; -> EAX = phys or CF; clobbers ecx, edx
    jc .a_oom                     ; branch INSIDE the host context: there is no
    mov edx, eax                  ; register left to carry CF out through
    VCPI_HOST_LEAVE               ; VCPI_HOST_LEAVE, which itself needs eax/ecx/
    pop eax                       ; ebp, while ebx/esi/edi belong to the client
    popfd
    xor ah, ah
    jmp .out
.a_oom:
    VCPI_HOST_LEAVE
    pop eax
    popfd
    mov ah, 0x88
    jmp .out
.de05:                            ; free the 4K page at physical EDX
    pushfd
    cli
    push eax
    VCPI_HOST_ENTER
    mov eax, edx
    and eax, 0xFFFFF000           ; spec: mask the 12 LSBs
    call vcpi_page_free           ; clobbers only EAX; EDX stays intact
    jc .f_bad                     ; branch inside the host context: see .de04
    VCPI_HOST_LEAVE
    pop eax
    popfd
    xor ah, ah
    jmp .out
.f_bad:
    VCPI_HOST_LEAVE
    pop eax
    popfd
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
; and the EFLAGS slot is stamped with IF=0 so the resumed V86 side really is
; masked until it STIs. That is the spec's interrupts-off intent expressed in
; the REAL flag: the guest runs at IOPL 3, so its own STI clears the mask for
; it and no monitor-side proxy bit is involved. ----
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
    mov dword [esp+0x10], 0x00023002 ; EFLAGS slot: VM=1, IOPL=3, real IF=0
                                  ; (masked per the spec), reserved bit 1
    add esp, 8                    ; drop the far-call return address
    iretd                         ; EIP,CS,EFLAGS,ESP,SS,ES,DS,FS,GS -> V86

; THE MONITOR ISSUES NO EOI OF ITS OWN, ANYWHERE. Every hardware line it
; takes is reflected to a guest IVT handler, and that handler owns the EOI
; exactly as it does on bare hardware. Two earlier revisions broke this and
; both are recorded so neither comes back:
;
;   * EOI-then-hold: the guest's handler ran with its own line NOT in
;     service, a state the 8259A cannot produce. DJGPP's shared IRQ wrapper
;     probes it (OCW3 0x0B + IN 0x20) -- E10, MonikaTT #GP(0) at 0xAF:78A3.
;   * `vip_release_to_chip`: specific EOIs at the VCPI DE0C boundary to
;     disclaim lines the monitor could no longer deliver (Tomb Raider froze
;     on the FMV's first frame, Grand Prix 2 at LAP 0, both from a stuck IS0
;     inhibiting the chip under the fully-nested rule). Correct as far as it
;     went, but it cleared an in-service bit with no handler having run, at
;     a boundary metal never reaches.
;
; Both existed only to clean up after an early INTA. Running the guest at
; real IOPL 3 means the acknowledge never happens while the guest has
; interrupts off, so there is nothing to clean up and no EOI to fabricate.

; `remapped_pic_line` (vector -> line, by subtracting the cached PIC base and
; probing the ISR) and `irq_reflect_line` (line -> vector, by adding it back)
; lived here. They were inverses, so the round trip through them was the
; identity and never added information -- it only made the fixed gates wrong
; at any non-default base, and put a cache that can go stale on the delivery
; path. Every gate now carries its own vector; see the routing note above
; irq_body. vcpi_pic_master/vcpi_pic_slave survive, but only to answer DE0A
; and to be written by DE0B (and read back as its ICW2 source) -- they no
; longer participate in delivery.

; Reflect an interrupt into the guest's real-mode IVT handler.
;   in: EBX = vector, EBP = &frame.eip, FS = driver data.  clobbers eax,ecx,edx,edi
;
; The frame MUST be a V86 one: [ebp+12]/[ebp+16] (guest SS:SP) only exist on
; a V86 trap frame, and rewriting a ring-0 frame's EIP/CS to real-mode IVT
; values makes the next IRETD re-fault -- one reflected ring-0 frame is what
; turned the G1 storm self-sustaining. The callers listed at
; reflect_vector_v86 below classify their own frames and enter past the check
; (vec13_entry classifies but does not reflect -- it jumps to irq_body); the
; exc_de/exc_ud/exc_nm gates reflect unconditionally, so a future
; ring-0-origin fault through any of those vectors lands on the VM check and
; reports (0xD4) on its first iteration: the bounded-storm backstop.
reflect_vector:
    test dword [ebp+8], FLAGS_VM   ; frame EFLAGS.VM
    jz reflect_ring0_frame
reflect_vector_v86:               ; entry for callers that already proved the
                                  ; frame's origin (monitor_body via TEST 3,
                                  ; irq_body via its VM test, .hlt via the V86
                                  ; HLT that opened the window)
    mov edx, [ebp+16]            ; guest SS
    shl edx, 4                   ; edx = guest stack base (linear)
    mov ax, [ebp+8]             ; guest flags, pushed VERBATIM: real IF is the
                                ; guest's IF and IOPL really is 3, so there is
                                ; nothing left to forge. The old image
                                ; substituted VIF for IF and OR'd in a virtual
                                ; IOPL 3 over a real IOPL 0; both are now true
                                ; of the frame itself.
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
    and word [ebp+8], 0xFDFF    ; clear IF in the FRAME: real-mode INT
                                ; semantics. The gate already cleared the live
                                ; flag; this is the image the guest's ISR
                                ; returns through, and its IRET restores IF for
                                ; real because the guest runs at IOPL 3. The
                                ; flags pushed onto the guest stack above still
                                ; carry the pre-interrupt IF, which is what the
                                ; ISR's IRET must put back.
    ret
reflect_ring0_frame:
    mov al, 0xD4                ; reflection asked against a ring-0 frame:
    jmp signal32                ; a storm's first bounced iteration -- report

; `maybe_deliver` -- the vip drain, run from every sensitive-op trap that
; could raise VIF -- lived here. With no early INTA there is no queue to
; drain: an interrupt the guest has not enabled simply stays latched in the
; 8259A's IRR and the chip delivers it in priority order once the guest STIs.

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

banner_tree: db 0xC3, 0xC4, '>', ' ', 0
banner: db 'TOKAEMM XMS/UMB/EMS memory manager; system running in V86.', 0x0D, 0x0A, 0

; Failure signal via the unit-tester exit port (AL = code). Stops the
; machine with the code as the exit status, so a monitor defect names
; itself on a game run instead of wedging or storming. Codes in use:
;   <opcode byte>  a trapped I/O port that is not 0x92
;                  (monitor_body .unhandled_io)
;   0xD3           ring-0 #GP: the monitor faulted on itself
;                  (vec13_entry TEST 3)
;   0xD4           reflect asked against a ring-0 frame (reflect_vector).
;                  Backstops exc_de/exc_ud/exc_nm only -- the default gates
;                  moved to 0xD5 when deflt_common started routing through
;                  irq_body
;   0xD5           a ring-0 CPU exception on an IRQ gate, or on a default gate
;                  (irq_body .ring0_exc)
;   0xD6           an IOPL-sensitive instruction faulted at all, so the V86
;                  frame's IOPL is not 3 -- a monitor bug, not a guest one
;                  (monitor_body .sensitive_at_iopl0)
;   0xD7           the one-slot halt window was already occupied
;                  (irq_body .hlt_slot_busy) -- a contract assert on the
;                  single-slot proof, and unreachable if it holds
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
; Overflow direction note (G1 review, 2026-08-17): the stack is the LAST
; thing in the core, so a runaway descent eats the TSS below it first,
; then code, and reaches the GDT/IDT only after ~8 KB. Reordering was
; considered and declined: every arrangement inside the core puts SOME
; load-bearing structure in the fall path, the DE01 comment pins all of
; these structures inside the furnished window, and the storm paths that
; walked ESP now report on their first iteration (vec13_entry TEST 3,
; reflect_vector's VM check) instead of iterating.
mon_stack:
    times 0x400 db 0
mon_stack_top:
resident_core_end:

; The paging tables are RESERVED, not emitted. Only the `.low_tables` fallback
; uses this region, and even there the driver reaches it exclusively by LINEAR
; address through `pd_lin` (a flat-selector dword read); nothing addresses it
; with a 16-bit offset. So the bytes need to be memory DOS keeps for us, which
; INIT arranges by reporting a break address past them, and they do not need to
; sit inside the driver's 64 KB offset space, nor be shipped in the file.
;
; Emitting them cost 32,752 bytes of file and, worse, 32,752 bytes of the 64 KB
; offset budget on EVERY configuration, to serve a path that only a 1 MiB
; machine takes. That is what left the image 16 bytes under its ceiling.
;
; Rounded up to a page: PD (1 page) + 16 PT (16 pages) = 0x11000, plus up to 0xFF0
; of page-rounding slack. `pd_lin` is round_up_4k(base + tables), and the load
; base is only paragraph-aligned, so keeping TABLES_OFF itself 4096-aligned
; makes (base + tables) mod 4096 equal base mod 4096, which caps the round-up at
; 4096-16 = 0xFF0 rather than a full 0xFFF. The alignment and the slack figure
; are load-bearing for each other; change neither alone.
;
; Written as offsets from `$$`, not as label expressions. In `-f bin` a label is
; relocatable rather than scalar, so `&`, `>>` and `%if` on one are rejected
; outright ("operator may only be applied to scalar values").
TABLES_OFF        equ ((resident_core_end - $$) + 4095) & ~4095
IMAGE_END_OFF     equ TABLES_OFF + TABLES_BYTES + SYS_RESV + 0xFF0
tables            equ $$ + TABLES_OFF

; Nothing zero-fills this region any more. DOS used to do it incidentally, by
; loading a file that was long enough to cover it; now the file ends at
; `resident_core_end`. `pm_init`'s `rep stosd` over exactly 0x11000 bytes at
; `pd_lin` is therefore the ONLY thing that clears the tables, which makes it a
; load-bearing invariant rather than belt and braces. Anything added here that
; expects to start zeroed must zero itself.

; The 16-bit offset limit binds on the CORE, which is what the driver addresses
; with 16-bit offsets and what the GDT code selector's limit is built from
; (`mov word [eax], resident_core_end - 1`). Without this assert NASM truncates
; that limit silently.
;
; The bound is 0xFFF0, NOT 0xFFFF, and the difference is the whole point. The
; HIGH path reports this offset raw (`mov cx, resident_core_end`), and the
; kernel rounds a reported break with `(FP_OFF(r_endaddr) + 15)/16` in 16-bit
; unsigned arithmetic (`hdr/portab.h:353`, built with COMPILER=owwin). At
; 0xFFF1 that sum wraps to 0x0000, the division yields 0, and DOS reserves ONE
; paragraph for the whole driver.
;
; The old assert measured the emitted image instead, which happened to force
; the core under 0x8000 and kept the high path 32 KB away from the window.
; Removing the reservation from the file removed that accidental bound, so the
; window has to be excluded explicitly or this change would have MOVED the
; defect onto the path every machine takes rather than closing it. Today's core
; is paragraph-aligned only by accident of the tail layout, and the next queued
; item rearranges exactly that tail.
%if (resident_core_end - $$) > 0xFFF0
    %error "TOKAEMM resident core is past 0xFFF0; the kernel break rounding wraps"
%endif

; The fallback break is reported as a paragraph count, `IMAGE_END_OFF >> 4`, and
; a shift truncates in silence. It is exact only because 0x11000 + 0xFF0 is a
; multiple of 16. Widening the slack to 0xFFF, which is the very edit the
; comment on TABLES_OFF warns about, would under-reserve by 15 bytes with no
; diagnostic anywhere.
%if IMAGE_END_OFF % 16
    %error "TOKAEMM IMAGE_END_OFF must be whole paragraphs; INIT reports a shift"
%endif

; pm_init lays the chain terminators down with a dword store, two link words at
; a time, so an odd page count would leave the last link zeroed -- reading as
; "chained to page 0" rather than "end of chain".
%if EMS_MAX_PAGES % 2
    %error "TOKAEMM ems_link terminator fill stores dwords; EMS_MAX_PAGES must be even"
%endif

; The system window is reserved by two independent expressions (INIT's high-path
; add and IMAGE_END_OFF's fallback), zero-filled by a third and mapped by a
; fourth, all built from SYS_BYTES. Only this catches a table added to the
; layout without SYS_USED being grown to cover it, which would place it on top
; of whatever follows the region.
%if SYS_USED > SYS_BYTES
    %error "TOKAEMM system window layout exceeds SYS_BYTES; grow SYS_USED"
%endif
