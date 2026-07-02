; TOKAEMM.SYS — SP-4b M0 memory manager (bespoke, runs the system in V86).
;
; Task 3 increment B: the driver's INIT builds a load-relative PM/paging +
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
vip: db 0                         ; virtual interrupt pending: bit0=IRQ0, bit1=IRQ1

; ---- SP-4b M1 XMS state (resident; reached via cs: overrides from V86) ----
old_2f:   dd 0                     ; previous INT 2Fh vector (chain target)
xms_pool_base: dd 0               ; first linear byte the EMB allocator hands out
xms_pool_end:  dd 0               ; one past the last (capped to the 16 MB map)
hma_owned: db 0                   ; 1 once a guest (DOS=HIGH) claims the HMA
a20_count: dw 0                   ; XMS local-A20 enable nesting (fns 05h/06h)
xms_disp:  dw 0                   ; dispatch scratch (register-safe table jump)
xms_mv_len: dd 0                  ; 0Bh move: byte count / src linear / dst linear
xms_mv_src: dd 0
xms_mv_dst: dd 0
xms_slot_save: dw 0               ; 0Fh resize: keep the slot across find_gap (clobbers SI)

; 32 EMB handles. handle h (1-based) -> slot at xms_table + (h-1)*XMS_SLOT.
; slot: +0 inuse(b) +1 lock(b) +2 size_kb(w) +4 base_linear(dd)
XMS_HANDLES equ 32
XMS_SLOT    equ 8
xms_table: times XMS_HANDLES*XMS_SLOT db 0

; SP-4b M3 UMB: the free upper window 0xC8000-0xEFFFF (above the VGA BIOS, below
; system ROM), 160 KB, page-mapped at INIT to extended RAM just above the HMA. The
; guest allocator (XMS 10h/11h/12h) hands out segment runs in [0xC800, umb_win_end)
; — the window ends at 0xF000, or 0xE000 when the EMS page frame is on (SP-4b M2).
UMB_LIN_BASE  equ 0x000C8000      ; first upper-hole linear byte
UMB_BYTES     equ 0x00028000      ; 160 KB (0xC8000..0xEFFFF)
UMB_PHYS_BASE equ 0x00110000      ; backing physical (just above the HMA)
UMB_SEG_BASE  equ 0x0C800         ; first UMB paragraph (segment); the window
                                  ; ends at the runtime umb_win_end (SP-4b M2)
; UMB sub-blocks handed out by 10h. slot: +0 inuse(b) +1 pad +2 seg(w) +4 paras(w)
UMB_SLOTS equ 8
UMB_SLOT  equ 6
umb_table: times UMB_SLOTS*UMB_SLOT db 0

; ---- SP-4b M2 EMS state (resident; reached via cs: overrides from V86) ----
; Default-off: DEVICE=C:\TOKAEMM.SYS presents a frameless manager (INT 67h
; answers present/version/0 pages, like EMM386 NOEMS); the RAM argument
; provisions the page frame [0xE000,0xF000) + a backing pool carved from
; extended RAM just past the UMB backing, and the UMB window shrinks to
; end below the frame.
EMS_PHYS_BASE equ 0x00138000      ; backing pool base (= UMB_PHYS_BASE+UMB_BYTES)
EMS_MAX_PAGES equ 256             ; 4 MB pool ceiling (16 KB pages)
EMS_FRAME_SEG equ 0xE000          ; page frame segment (4 slots x 16 KB)
EMS_FRAME_LIN equ 0x000E0000
EMS_HANDLES   equ 32
; handle slot: +0 inuse(b) +1 saved(b) +2 npages(w) +4 first(w) +6 pad(w)
;              +8 saved_map(4w). Backing runs are CONTIGUOUS per handle
; (logical page L -> backing page first+L); contiguity is invisible to apps.
; ponytail: a fragmented pool can 88h an alloc that per-page bookkeeping would
; satisfy; switch to per-page lists if a real consumer hits it.
EMS_SLOT      equ 16
ems_on:      db 0                 ; 1 = RAM argument seen and pages provisioned
ems_pages:   dw 0                 ; total 16 KB pages (<= EMS_MAX_PAGES)
ems_free:    dw 0                 ; free pages
ems_disp:    dw 0                 ; dispatch scratch (mirrors xms_disp)
umb_win_end: dw 0xF000            ; UMB window end segment (0xE000 with EMS on)
ems_table: times EMS_HANDLES*EMS_SLOT db 0
; live frame map: backing page index per physical slot, 0xFFFF = unmapped
ems_frame_map: dw 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF

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
    ; Report resident size FIRST, while ES:BX still points at the request header
    ; (the setup below clobbers BX). r_endaddr = drv_seg:resident_end covers the
    ; driver's code + tables, which stay resident under the monitor permanently.
    mov word [es:bx+14], resident_end
    mov word [es:bx+16], cs
    mov word [es:bx+3], 0x0100    ; r_status = S_DONE

    ; --- SP-4b M2: parse the DEVICE= tail for a whole-token "RAM" argument.
    ; r_bpbptr (+18) points at the raw command line, driver path first
    ; (FreeDOS init_device). Case-insensitive; NOEMS/anything else = default off.
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
    cmp al, 'R'
    jne .p_skiptok
    lodsb
    call cls_al
    cmp ah, 1
    je .p_gap                     ; token was just "R"
    cmp ah, 2
    je .p_done
    and al, 0xDF
    cmp al, 'A'
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
    lodsb                         ; the char after "RAM" must end the token
    call cls_al
    cmp ah, 0
    je .p_skiptok                 ; longer token (e.g. RAMX): not ours
    mov byte [cs:ems_on], 1
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

    ; --- SP-4b M1/M3: size the XMS pool + hook INT 2Fh (real mode, pre-V86) ---
    ; INT 15h AH=88h -> AX = KB of extended memory above 1 MB. Extended layout:
    ; HMA [1MB,+64KB), UMB backing [0x110000,+160KB), XMS pool [0x138000, top).
    mov ah, 0x88
    int 0x15                      ; AX = extended KB (real mode, before V86)
    movzx eax, ax
    cmp eax, 15*1024              ; cap to the 15 MB above 1 MB the map covers
    jbe .pool_ok
    mov eax, 15*1024
.pool_ok:
    sub eax, 64                   ; drop the HMA (first 64 KB of extended memory)
    shl eax, 10                   ; KB -> bytes
    add eax, 0x00110000           ; eax = top of extended (pool_end, unchanged from M1)
    mov [cs:xms_pool_end], eax
    ; The 160 KB UMB backing sits just above the HMA (SP-4b M3); XMS starts past it.
    mov dword [cs:xms_pool_base], UMB_PHYS_BASE + UMB_BYTES

    ; --- SP-4b M2: with RAM, carve the EMS pool [EMS_PHYS_BASE, +pages*16K),
    ; shift the XMS pool past it, and end the UMB window below the page frame.
    cmp byte [cs:ems_on], 0
    je .ems_done
    mov eax, [cs:xms_pool_end]
    cmp eax, EMS_PHYS_BASE + 0x4000  ; at least one 16 KB page available?
    jb .ems_off                      ; degenerate small-RAM box: stay frameless
    sub eax, EMS_PHYS_BASE
    shr eax, 14                      ; bytes -> 16 KB pages
    cmp eax, EMS_MAX_PAGES
    jbe .ems_clamped
    mov eax, EMS_MAX_PAGES
.ems_clamped:
    mov [cs:ems_pages], ax
    mov [cs:ems_free], ax
    shl eax, 14
    add eax, EMS_PHYS_BASE
    mov [cs:xms_pool_base], eax      ; XMS pool starts past the EMS pool
    mov word [cs:umb_win_end], EMS_FRAME_SEG
    jmp .ems_done
.ems_off:
    mov byte [cs:ems_on], 0
.ems_done:

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
    ; --- end M1 INIT additions ---

    mov [drv_seg], cs
    xor eax, eax
    mov ax, cs
    shl eax, 4
    mov [base_lin], eax           ; base = CS<<4

    add eax, tables               ; pd_lin = page-align(base + tables)
    add eax, 0xFFF
    and eax, 0xFFFFF000
    mov [pd_lin], eax

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

    push es                       ; zero the TSS (ES still = request header seg here)
    push di
    push cs
    pop es                        ; ES = our segment so STOSW targets our TSS
    mov di, tss
    mov cx, 0x90 / 2
    xor ax, ax
    rep stosw
    pop di
    pop es
    mov eax, [base_lin]           ; ESP0 = monitor stack top in driver memory
    add eax, mon_stack_top
    mov [tss + 4], eax
    mov ebx, eax                  ; carry monitor ESP into PM (survives PT build)
    mov word  [tss + 8], 0x0010   ; SS0 = flat data selector
    mov word  [tss + 0x66], 0x0068 ; I/O-map base (all-zero bitmap = permissive)

    mov ebp, [pd_lin]             ; carry pd_lin + drv_seg into PM
    movzx esi, word [drv_seg]

    lgdt [gdtr]
    lidt [idtr]
    mov eax, cr0
    or eax, 1                     ; PE
    mov cr0, eax
    jmp dword 0x08:pm_init        ; code sel base = base -> linear base+pm_init

; ============================================================================
; SP-4b M1 — guest XMS driver (16-bit real mode / V86). Reached only via the INT
; 2Fh hook (install-check / get-entry) and the far-callable control entry, never
; by fall-through (INIT above ends in a far jump). Own data via cs: overrides
; because the far-callable entry runs with the caller's DS.
; ============================================================================

; INT 2Fh multiplex hook: XMS install-check (4300) / get-entry (4310); chain else.
xms_2f_handler:
    cmp ax, 0x4300
    je .install
    cmp ax, 0x4310
    je .entry
    jmp far [cs:old_2f]
.install:
    mov al, 0x80                 ; XMS present
    iret
.entry:
    push cs
    pop es
    mov bx, xms_entry            ; ES:BX -> control entry
    iret

; Far-callable XMS control function. AH = function; XMS 3.0 conventions
; (AX=1 success / AX=0 + BL=error). Register-safe table dispatch (only BX is
; touched, then restored, so CX/DX/SI/DI/BP/DS inputs survive to the handler).
xms_entry:
    cmp ah, 0x12
    ja .unimpl                   ; 13h+ : not implemented
    push bx
    movzx bx, ah
    add bx, bx
    mov bx, [cs:xms_jt + bx]
    mov [cs:xms_disp], bx
    pop bx
    jmp [cs:xms_disp]
.unimpl:
    xor ax, ax
    mov bl, 0x80                  ; function not implemented
    retf
xms_jt:
    dw xf_version, xf_req_hma,   xf_rel_hma,  xf_a20_gon
    dw xf_a20_goff, xf_a20_lon,  xf_a20_loff, xf_a20_query
    dw xf_query_free, xf_alloc,  xf_free,     xf_move
    dw xf_lock,    xf_unlock,    xf_info,     xf_resize
    dw xf_req_umb, xf_rel_umb,   xf_realloc_umb

; shared tails
xms_ok:
    mov ax, 1
    xor bl, bl
    retf
xms_fail:                        ; enter with BL = error code
    xor ax, ax
    retf

; 00h get version: AX=3.00, BX=revision, DX=HMA-exists. Never fails.
xf_version:
    mov ax, 0x0300
    mov bx, 0x0300
    mov dx, 1
    retf

; 01h request HMA / 02h release HMA (a flag; no /HMAMIN gate in M1).
xf_req_hma:
    cmp byte [cs:hma_owned], 0
    jne .inuse
    mov byte [cs:hma_owned], 1
    jmp xms_ok
.inuse:
    mov bl, 0x91                 ; HMA already in use
    jmp xms_fail
xf_rel_hma:
    cmp byte [cs:hma_owned], 0
    je .no
    mov byte [cs:hma_owned], 0
    jmp xms_ok
.no:
    mov bl, 0x93                 ; HMA not allocated
    jmp xms_fail

; 03h/04h global A20; 05h/06h local A20 (nesting, drive gate only on 0<->1).
xf_a20_gon:
    call a20_on
    jmp xms_ok
xf_a20_goff:
    call a20_off
    jmp xms_ok
xf_a20_lon:
    inc word [cs:a20_count]
    cmp word [cs:a20_count], 1
    jne xms_ok
    call a20_on
    jmp xms_ok
xf_a20_loff:
    cmp word [cs:a20_count], 0
    je .drive                    ; already 0: a disable still drives it off
    dec word [cs:a20_count]
    jnz xms_ok
.drive:
    call a20_off
    jmp xms_ok
; 07h query A20: AX=1 enabled / 0 disabled, BL=0.
xf_a20_query:
    in al, 0x92
    test al, 2
    setnz al
    movzx ax, al
    xor bl, bl
    retf

a20_on:
    in al, 0x92
    or al, 2
    out 0x92, al
    ret
a20_off:
    in al, 0x92
    and al, 0xFD
    out 0x92, al
    ret

; 08h query free extended memory: AX=largest free KB, DX=total free KB, BL=0.
; ponytail: largest is approximated as total (a first-fit pool rarely fragments,
; and over-reporting only turns a would-be-large alloc into an A0h failure).
xf_query_free:
    push cx
    push si
    mov eax, [cs:xms_pool_end]
    sub eax, [cs:xms_pool_base]
    shr eax, 10                  ; pool size KB
    mov si, xms_table
    mov cx, XMS_HANDLES
.sum:
    cmp byte [cs:si], 0
    je .skip
    movzx ebx, word [cs:si+2]
    sub eax, ebx
.skip:
    add si, XMS_SLOT
    loop .sum
    cmp eax, 0xFFFF
    jbe .cap
    mov eax, 0xFFFF
.cap:
    mov dx, ax                   ; total free KB
    xor bl, bl                   ; AX (=largest) already = total
    pop si
    pop cx
    retf

; 09h allocate EMB: DX=KB in -> DX=handle. First-fit gap + first free slot.
xf_alloc:
    push cx
    push si
    push di
    push bp
    movzx eax, dx
    shl eax, 10
    mov [cs:xms_mv_len], eax      ; need bytes
    call find_gap                 ; -> EDI = base, or CF (oom)
    jc .oom
    mov si, xms_table
    mov cx, XMS_HANDLES
    xor bp, bp                    ; handle counter (1-based)
.slot:
    inc bp
    cmp byte [cs:si], 0
    je .got
    add si, XMS_SLOT
    loop .slot
    mov bl, 0xA1                  ; out of handles
    jmp .fail
.got:
    mov byte [cs:si], 1
    mov byte [cs:si+1], 0
    mov eax, [cs:xms_mv_len]
    shr eax, 10
    mov [cs:si+2], ax             ; size_kb
    mov [cs:si+4], edi            ; base_linear
    mov dx, bp                    ; handle out
    pop bp
    pop di
    pop si
    pop cx
    jmp xms_ok
.oom:
    mov bl, 0xA0
.fail:
    pop bp
    pop di
    pop si
    pop cx
    jmp xms_fail

; 0Ah free EMB: DX=handle.
xf_free:
    push si
    call slot_of                  ; -> SI = slot, or CF + BL=0xA2
    jc .bad
    cmp byte [cs:si+1], 0         ; locked?
    jne .locked
    mov byte [cs:si], 0
    pop si
    jmp xms_ok
.locked:
    mov bl, 0xAB
    pop si
    jmp xms_fail
.bad:
    pop si
    jmp xms_fail

; 0Ch lock EMB: DX=handle -> DX:BX = 32-bit linear base, lock++.
xf_lock:
    push si
    call slot_of
    jc .bad
    cmp byte [cs:si+1], 0xFF
    je .ovf
    inc byte [cs:si+1]
    mov edx, [cs:si+4]
    mov ebx, edx
    shr edx, 16                   ; DX:BX = linear (BX = low word of ebx)
    pop si
    mov ax, 1
    retf
.ovf:
    mov bl, 0xAC
    pop si
    jmp xms_fail
.bad:
    pop si
    jmp xms_fail

; 0Dh unlock EMB: DX=handle.
xf_unlock:
    push si
    call slot_of
    jc .bad
    cmp byte [cs:si+1], 0
    je .notlocked
    dec byte [cs:si+1]
    pop si
    jmp xms_ok
.notlocked:
    mov bl, 0xAA
    pop si
    jmp xms_fail
.bad:
    pop si
    jmp xms_fail

; 0Eh handle info: DX=handle -> BH=lock, BL=free handles, DX=size_kb, AX=1.
xf_info:
    push si
    push cx
    call slot_of
    jc .bad
    mov bh, [cs:si+1]             ; lock count
    push si
    mov si, xms_table
    mov cx, XMS_HANDLES
    xor al, al                    ; free-handle count
.cnt:
    cmp byte [cs:si], 0
    jne .used
    inc al
.used:
    add si, XMS_SLOT
    loop .cnt
    pop si
    mov bl, al                    ; free handles
    mov dx, [cs:si+2]             ; size_kb
    pop cx
    pop si
    mov ax, 1
    retf
.bad:
    pop cx
    pop si
    jmp xms_fail

; 0Fh resize EMB: BX=new KB, DX=handle. Free + re-place; restore on failure.
; ponytail: re-places without copying the old contents to the new base — data is
; preserved only when find_gap returns the same base (no fragmentation). This
; mirrors the retired FlatEmbAllocator::resize (the M1 goal was HLE parity); a
; copy-on-relocate (via the INT 0xC0 memcpy) is a future-milestone fidelity item.
xf_resize:
    push cx
    push si
    push di
    call slot_of
    jc .bad
    mov [cs:xms_slot_save], si    ; find_gap clobbers SI; keep the slot offset
    cmp byte [cs:si+1], 0         ; locked?
    jne .locked
    cmp bx, [cs:si+2]             ; same size?
    je .ok
    push word [cs:si+2]           ; save old size_kb
    push dword [cs:si+4]          ; save old base
    mov byte [cs:si], 0           ; temporarily free the slot
    movzx eax, bx
    shl eax, 10
    mov [cs:xms_mv_len], eax
    call find_gap
    mov si, [cs:xms_slot_save]    ; restore the slot offset (find_gap clobbered SI)
    jc .restore
    mov byte [cs:si], 1
    mov byte [cs:si+1], 0
    mov eax, [cs:xms_mv_len]      ; size_kb from need bytes (find_gap clobbered BX)
    shr eax, 10
    mov [cs:si+2], ax
    mov [cs:si+4], edi
    add sp, 6                     ; discard saved old (dword + word)
.ok:
    pop di
    pop si
    pop cx
    jmp xms_ok
.restore:
    pop eax                       ; old base
    mov [cs:si+4], eax
    pop ax                        ; old size_kb
    mov [cs:si+2], ax
    mov byte [cs:si], 1
    mov bl, 0xA0
    pop di
    pop si
    pop cx
    jmp xms_fail
.locked:
    mov bl, 0xAB
    pop di
    pop si
    pop cx
    jmp xms_fail
.bad:
    pop di
    pop si
    pop cx
    jmp xms_fail

; 0Bh move EMB: DS:SI -> descriptor {len(dd) srcH(w) srcOff(dd) dstH(w) dstOff(dd)}.
; Resolve both endpoints to linear, then trap to the monitor for the flat copy.
xf_move:
    push cx
    push si
    push di
    push bp
    mov eax, [si]                 ; length (DS:SI +0)
    test eax, eax
    jz .zero                      ; zero length = legal no-op success
    test eax, 1
    jnz .badlen                   ; odd length -> A7h
    mov [cs:xms_mv_len], eax
    mov bx, [si+4]                ; src handle
    mov edx, [si+6]               ; src offset
    call resolve                  ; -> EAX = linear, or CF + AL=1(handle)/2(offset)
    jc .src_err
    mov [cs:xms_mv_src], eax
    mov bx, [si+10]               ; dst handle
    mov edx, [si+12]              ; dst offset
    call resolve
    jc .dst_err
    mov [cs:xms_mv_dst], eax
    mov esi, [cs:xms_mv_src]
    mov edi, [cs:xms_mv_dst]
    mov ecx, [cs:xms_mv_len]
    mov edx, 0x544D              ; monitor-call cookie 'TM'
    int 0xC0                     ; ring-0 flat memcpy: ES:EDI <- DS:ESI, ECX bytes
    pop bp
    pop di
    pop si
    pop cx
    jmp xms_ok
.zero:
    pop bp
    pop di
    pop si
    pop cx
    jmp xms_ok
.badlen:
    mov bl, 0xA7
    jmp .fail
.src_err:
    cmp al, 1
    je .src_h
    mov bl, 0xA4                  ; bad src offset
    jmp .fail
.src_h:
    mov bl, 0xA3                  ; bad src handle
    jmp .fail
.dst_err:
    cmp al, 1
    je .dst_h
    mov bl, 0xA6                  ; bad dst offset
    jmp .fail
.dst_h:
    mov bl, 0xA5                  ; bad dst handle
.fail:
    pop bp
    pop di
    pop si
    pop cx
    jmp xms_fail

; --- helpers ---------------------------------------------------------------
; DX = handle -> SI = slot offset (cs-relative), CF clear; or CF set + BL=0xA2.
; Clobbers AX.
slot_of:
    cmp dx, 1
    jb .bad
    cmp dx, XMS_HANDLES
    ja .bad
    mov ax, dx
    dec ax
    shl ax, 3                     ; * XMS_SLOT
    add ax, xms_table
    mov si, ax
    cmp byte [cs:si], 0           ; inuse?
    je .bad
    clc
    ret
.bad:
    mov bl, 0xA2                  ; invalid handle
    stc
    ret

; First-fit gap for [cs:xms_mv_len] bytes over [pool_base, pool_end). Restart-on-
; overlap. out: EDI = base, CF clear; or CF set (out of memory).
; Clobbers eax, ebx, edx, cx, si.
find_gap:
    mov edi, [cs:xms_pool_base]
.restart:
    mov eax, edi
    add eax, [cs:xms_mv_len]
    cmp eax, [cs:xms_pool_end]
    ja .oom
    mov si, xms_table
    mov cx, XMS_HANDLES
.scan:
    cmp byte [cs:si], 0
    je .next
    mov ebx, [cs:si+4]            ; b.base
    movzx eax, word [cs:si+2]
    shl eax, 10
    add eax, ebx                  ; b.top
    cmp eax, edi                  ; b.top <= cursor? (block below)
    jbe .next
    mov edx, edi
    add edx, [cs:xms_mv_len]      ; cursor + need
    cmp ebx, edx                  ; b.base >= cursor+need? (block above)
    jae .next
    mov edi, eax                  ; overlap: cursor = b.top, restart
    jmp .restart
.next:
    add si, XMS_SLOT
    loop .scan
    clc
    ret
.oom:
    stc
    ret

; Resolve a move endpoint. in: BX=handle, EDX=offset, [cs:xms_mv_len]=length.
; out: EAX = linear, CF clear; or CF set + AL=1 (bad handle) / AL=2 (bad offset).
; Handle 0 => EDX is a real-mode seg:off (high=seg, low=off). Clobbers eax,ebx,edx.
resolve:
    test bx, bx
    jnz .handle
    mov eax, edx
    shr eax, 16                   ; segment
    and edx, 0xFFFF               ; offset
    shl eax, 4
    add eax, edx                  ; seg*16 + off
    clc
    ret
.handle:
    cmp bx, XMS_HANDLES
    ja .badh
    push si
    mov ax, bx
    dec ax
    shl ax, 3
    add ax, xms_table
    mov si, ax
    cmp byte [cs:si], 0           ; inuse?
    je .badh_pop
    movzx eax, word [cs:si+2]
    shl eax, 10                   ; size bytes
    cmp edx, eax                  ; offset > size?
    ja .bado_pop
    sub eax, edx                  ; remaining = size - offset
    mov ebx, [cs:xms_mv_len]
    cmp ebx, eax                  ; length > remaining?
    ja .bado_pop
    mov eax, [cs:si+4]            ; base_linear
    add eax, edx                  ; + offset
    pop si
    clc
    ret
.badh_pop:
    pop si
.badh:
    mov al, 1
    stc
    ret
.bado_pop:
    pop si
    mov al, 2
    stc
    ret

; --- SP-4b M3 UMB (XMS 10h/11h/12h) over the paged window [UMB_SEG_BASE, +PARAS) --

; 10h Request UMB: DX = paragraphs. First-fit a free run; on success mark a slot.
;   success: AX=1, BX=segment, DX=paras. smaller-only: AX=0, BL=0xB0, DX=largest.
;   none: AX=0, BL=0xB1, DX=0.
xf_req_umb:
    push si
    call umb_free_run             ; DX=need -> BX=seg, CF clear; or CF set
    jc .toobig
    mov si, umb_table             ; find a free slot for the block record
    push cx
    mov cx, UMB_SLOTS
.slot:
    cmp byte [cs:si], 0
    je .got
    add si, UMB_SLOT
    loop .slot
    pop cx                        ; no slot (DOS grabs once, so unexpected)
    xor dx, dx
    mov bl, 0xB1
    pop si
    jmp xms_fail
.got:
    pop cx
    mov byte [cs:si], 1
    mov [cs:si+2], bx             ; seg
    mov [cs:si+4], dx             ; paras
    pop si
    mov ax, 1                     ; BX=seg, DX=paras already set
    retf
.toobig:
    call umb_largest              ; AX = largest free run (paras)
    test ax, ax
    jz .none
    mov dx, ax                    ; DX = largest
    mov bl, 0xB0
    xor ax, ax
    pop si
    retf
.none:
    xor dx, dx
    mov bl, 0xB1
    xor ax, ax
    pop si
    retf

; 11h Release UMB: DX = segment.
xf_rel_umb:
    push si
    call umb_slot_of_seg          ; DX=seg -> SI=slot, CF clear; or CF set (BL=0xB2)
    jc .bad
    mov byte [cs:si], 0
    pop si
    jmp xms_ok
.bad:
    pop si
    jmp xms_fail

; 12h Reallocate UMB: BX = new paras, DX = segment. Shrink always; grow if the
; space above the block is free. Grow-fail: AX=0, BL=0xB0, DX=largest growable.
xf_realloc_umb:
    push si
    call umb_slot_of_seg
    jc .bad
    cmp bx, [cs:si+4]             ; new <= old?
    jbe .set
    call umb_max_grow             ; SI=slot -> AX = max paras from this seg
    cmp bx, ax                    ; new <= maxgrow?
    ja .nofit
.set:
    mov [cs:si+4], bx
    pop si
    jmp xms_ok
.nofit:
    mov dx, ax                    ; DX = largest growable
    mov bl, 0xB0
    pop si
    xor ax, ax
    retf
.bad:
    pop si
    jmp xms_fail

; First-fit free run of DX paras in [UMB_SEG_BASE, umb_win_end). Restart-on-
; overlap (mirrors find_gap). out: BX = seg, CF clear; or CF set. Preserves DX;
; clobbers ax, cx, si.
umb_free_run:
    push cx
    push si
    mov ax, [cs:umb_win_end]      ; window end (drops to 0xE000 when the EMS
    sub ax, UMB_SEG_BASE          ; page frame carves the top; SP-4b M2)
    cmp dx, ax                    ; bigger than the whole window? can't fit (dodges
    ja .none                      ; the 16-bit wrap on cursor+need for huge probes).
    mov bx, UMB_SEG_BASE
.restart:
    mov ax, bx
    add ax, dx                    ; cursor + need
    jc .none                      ; 16-bit wrap: the cursor (a block top near the
                                  ; window end) + need passed 0xFFFF -> cannot fit
    cmp ax, [cs:umb_win_end]
    ja .none
    mov si, umb_table
    mov cx, UMB_SLOTS
.scan:
    cmp byte [cs:si], 0
    je .next
    mov ax, [cs:si+2]
    add ax, [cs:si+4]             ; b.top = seg + paras
    cmp ax, bx                    ; b.top <= cursor? (below)
    jbe .next
    mov ax, bx
    add ax, dx                    ; cursor + need
    cmp [cs:si+2], ax             ; b.seg >= cursor+need? (above)
    jae .next
    mov bx, [cs:si+2]             ; overlap: cursor = b.top, restart
    add bx, [cs:si+4]
    jmp .restart
.next:
    add si, UMB_SLOT
    loop .scan
    pop si
    pop cx
    clc
    ret
.none:
    pop si
    pop cx
    stc
    ret

; Largest free run in paras. Approximated as the run from the highest block top to
; the window end (exact for first-fit-from-base without middle frees, which is how
; DOS=UMB uses it). ponytail: an exact largest-gap needs a sorted walk.
; out: AX = largest free paras. clobbers cx, dx, si.
umb_largest:
    push cx
    push si
    push dx
    mov ax, UMB_SEG_BASE          ; highest top so far
    mov si, umb_table
    mov cx, UMB_SLOTS
.l:
    cmp byte [cs:si], 0
    je .n
    mov dx, [cs:si+2]
    add dx, [cs:si+4]             ; b.top
    cmp dx, ax
    jbe .n
    mov ax, dx
.n:
    add si, UMB_SLOT
    loop .l
    neg ax
    add ax, [cs:umb_win_end]      ; window_end - highest_top
    pop dx
    pop si
    pop cx
    ret

; DX = seg -> SI = slot offset, CF clear; or CF set (BL=0xB2). Clobbers ax, cx.
umb_slot_of_seg:
    push cx
    mov si, umb_table
    mov cx, UMB_SLOTS
.f:
    cmp byte [cs:si], 0
    je .n
    mov ax, [cs:si+2]
    cmp ax, dx
    je .hit
.n:
    add si, UMB_SLOT
    loop .f
    pop cx
    mov bl, 0xB2                  ; invalid UMB segment
    stc
    ret
.hit:
    pop cx
    clc
    ret

; Max paras the block at SI can grow to (nearest higher block seg, or window end,
; minus this seg). in: SI = slot. out: AX. preserves SI/BX/CX/DX.
umb_max_grow:
    push bx
    push cx
    push di
    mov di, [cs:si+2]             ; our seg
    mov ax, [cs:umb_win_end]      ; nearest boundary = window end
    push si
    mov si, umb_table
    mov cx, UMB_SLOTS
.s:
    cmp byte [cs:si], 0
    je .n
    mov bx, [cs:si+2]             ; other seg
    cmp bx, di                    ; other <= our seg? (self or below)
    jbe .n
    cmp bx, ax                    ; other < nearest?
    jae .n
    mov ax, bx
.n:
    add si, UMB_SLOT
    loop .s
    pop si
    sub ax, di                    ; max paras = nearest - our seg
    pop di
    pop cx
    pop bx
    ret

; ============================================================================
; SP-4b M2 — guest EMS (INT 67h, LIM 4.0 subset; V86 code, cs: overrides).
; Hooked at INIT; apps find the manager by comparing "EMMXXXX0" at
; [IVT67-seg:000A] = our device-header name. Status in AH (0 = OK); registers
; other than documented outputs are preserved. Functions outside the
; implemented set return 84h like a real manager that lacks them.
; ============================================================================
ems_int67:
    cmp ah, 0x40
    jb ef_undef
    cmp ah, 0x4C
    ja ef_undef
    push bx
    movzx bx, ah
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
    dw ef_pages                                     ; 4Ch

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
ef_counts:                        ; 42h get page counts: BX=free, DX=total
    mov bx, [cs:ems_free]
    mov dx, [cs:ems_pages]
    xor ah, ah
    iret
ef_version:                       ; 46h get version -> AL = BCD 4.0
    mov al, 0x40
    xor ah, ah
    iret

; 43h allocate: BX = pages -> DX = handle. Contiguous first-fit run.
ef_alloc:
    test bx, bx
    jz .zero
    cmp bx, [cs:ems_pages]
    ja .total
    cmp bx, [cs:ems_free]
    ja .nofree
    push ax                       ; ems_find_run clobbers AX; AL is not an output
    push dx                       ; DX is an output only on success (the handle)
    push si
    push cx
    push di
    call ems_find_run             ; BX=need -> DI=first page, CF=no run
    jc .frag
    mov si, ems_table             ; first free handle slot
    mov cx, EMS_HANDLES
    xor dx, dx                    ; handle counter (1-based below)
.slot:
    inc dx
    cmp byte [cs:si], 0
    je .got
    add si, EMS_SLOT
    loop .slot
    pop di
    pop cx
    pop si
    pop dx                        ; restore the caller's DX (the counter ran over it)
    pop ax
    mov ah, 0x85                  ; no more handles
    iret
.got:
    mov byte [cs:si], 1           ; inuse
    mov byte [cs:si+1], 0         ; saved = 0
    mov [cs:si+2], bx             ; npages
    mov [cs:si+4], di             ; first backing page
    sub [cs:ems_free], bx
    pop di
    pop cx
    pop si
    add sp, 2                     ; discard the saved DX: DX = the new handle
    pop ax
    xor ah, ah
    iret
.frag:
    pop di
    pop cx
    pop si
    pop dx
    pop ax
.nofree:
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
; monitor's INT 0xC0 'PM' service (ring-0 work, like the M1 XMS-move memcpy).
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
    mov cx, [cs:si+4]
    add cx, bx                    ; backing page = first + logical
.do:
    movzx si, al
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

; 45h release: DX = handle. Unmaps its frame slots, scrubs its pages from
; every saved_map (a freed-and-reassigned page must not be reinstated by a
; later 48h restore — mirrors the retired HLE's invalidate_freed), then
; returns the run to the pool.
ef_free:
    push si
    call ems_slot_of
    jc .badh
    push ax
    push bx
    push cx
    push dx
    push di
    mov di, [cs:si+4]             ; DI = first freed page
    mov dx, di
    add dx, [cs:si+2]             ; DX = end (exclusive)
    xor bx, bx                    ; BL = physical slot 0..3
.slots:
    push si
    movzx si, bl
    add si, si
    mov cx, [cs:ems_frame_map + si]
    cmp cx, di
    jb .ns
    cmp cx, dx
    jae .ns
    mov word [cs:ems_frame_map + si], 0xFFFF
    mov al, bl
    mov cx, 0xFFFF
    call ems_remap_slot           ; restore the INIT mapping
.ns:
    pop si
    inc bx
    cmp bx, 4
    jb .slots
    push si                       ; scrub [DI,DX) from every saved_map
    mov si, ems_table
    mov cx, EMS_HANDLES
.scrub:
    cmp byte [cs:si+1], 0         ; saved?
    je .nh
    push cx
    push si
    add si, 8                     ; saved_map
    mov cx, 4
.sm:
    mov ax, [cs:si]
    cmp ax, di
    jb .smn
    cmp ax, dx
    jae .smn
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
    mov ax, [cs:si+2]             ; release the run + the slot (its own saved
    add [cs:ems_free], ax         ; context dies with saved=0)
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
    xor bx, bx                    ; BL = physical slot 0..3
.rs:
    movzx di, bl
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
    shl ax, 4                     ; * EMS_SLOT
    add ax, ems_table
    mov si, ax
    pop ax
    cmp byte [cs:si], 0           ; inuse?
    je .bad
    clc
    ret
.bad:
    mov ah, 0x83                  ; invalid handle
    stc
    ret

; First-fit contiguous run of BX pages -> DI = first page, or CF set.
; Restart-on-overlap over the handle slots (mirrors find_gap). Clobbers ax,cx,si.
ems_find_run:
    xor di, di                    ; cursor
.restart:
    mov ax, di
    add ax, bx
    cmp ax, [cs:ems_pages]
    ja .none
    mov si, ems_table
    mov cx, EMS_HANDLES
.scan:
    cmp byte [cs:si], 0
    je .next
    mov ax, [cs:si+4]
    add ax, [cs:si+2]             ; b.top = first + npages
    cmp ax, di
    jbe .next                     ; block below the cursor
    mov ax, di
    add ax, bx
    cmp [cs:si+4], ax
    jae .next                     ; block above cursor+need
    mov di, [cs:si+4]
    add di, [cs:si+2]             ; overlap: cursor = b.top, restart
    jmp .restart
.next:
    add si, EMS_SLOT
    loop .scan
    clc
    ret
.none:
    stc
    ret

; Monitor remap of one frame slot. AL = slot 0-3, CX = backing page index or
; 0xFFFF to restore the INIT (UMB-backing) mapping. Preserves all registers.
ems_remap_slot:
    push eax
    push ebx
    push ecx
    push edx
    movzx ebx, al
    shl ebx, 14
    add ebx, EMS_FRAME_LIN        ; EBX = slot linear base
    cmp cx, 0xFFFF
    je .unmap
    movzx ecx, cx
    shl ecx, 14
    add ecx, EMS_PHYS_BASE        ; ECX = backing physical base
    jmp .go
.unmap:
    xor ecx, ecx                  ; 0 = restore the INIT mapping
.go:
    mov edx, 0x4D50               ; 'PM' monitor-call cookie
    int 0xC0
    pop edx
    pop ecx
    pop ebx
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
    dq 0x0000890000000088         ; [18] TSS, base patched, limit 0x88
    dq 0x00CF93000000FFFF         ; [20] data, base patched (= base, driver data)
gdtr:
    dw 0x27                       ; 5 descriptors
    dd 0

; IDT (static gates; offsets are driver-relative, selector = PM code 0x08).
; Only the vectors that fire in M0 are present: 8 = IRQ0 timer, 9 = IRQ1
; keyboard, 13 = #GP (sensitive-instruction trap). base patched at runtime.
align 8
idt:
    times 8*8 db 0                ; 0..7
    dw irq8, 0x0008               ; 8  IRQ0 timer (offset-high = 0, driver < 64K)
    db 0, 0x8E
    dw 0
    dw irq9, 0x0008             ; 9  IRQ1 keyboard
    db 0, 0x8E
    dw 0
    times 3*8 db 0               ; 10..12
    dw monitor, 0x0008          ; 13 #GP -> sensitive-instruction monitor
    db 0, 0x8E
    dw 0
    times 18*8 db 0             ; 14..31
idt_end:
idtr:
    dw idt_end - idt - 1
    dd 0

bits 32
pm_init:                          ; EBP=pd_lin, ESI=drv_seg, EBX=monitor ESP0
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, ebx                  ; monitor ring-0 stack (driver-resident)
    ; PD[0..3] -> the four PTs that follow the PD (each PT maps 4 MiB), so the
    ; identity map covers 0..16 MiB and the XMS-move memcpy can reach every EMB.
    lea eax, [ebp + 0x1000]       ; first PT linear = PD + 0x1000
    or eax, 7
    mov edi, ebp                  ; write PD entries
    mov ecx, 4
.pde:
    mov [edi], eax
    add eax, 0x1000               ; next PT is one page further
    add edi, 4
    loop .pde
    lea edi, [ebp + 0x1000]       ; 4096 identity entries (0..16 MiB), present/rw/user
    mov eax, 7
    mov ecx, 4096
.pt:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .pt
    ; SP-4b M3: page the free upper window 0xC8000-0xEFFFF to extended RAM (the
    ; EMM386 trick). On real hardware these holes have no RAM; a UMB there must be
    ; extended RAM mapped in. (This emulator's flat array also backs phys 0xC8000 via
    ; read_phys's fallback, so identity would work too -- but mapping proper extended
    ; RAM is faithful and keeps the UMB accounted against extended memory, not phantom
    ; RAM.) ROM/video PTEs stay identity; only these 40 move.
    lea edi, [ebp + 0x1000 + (UMB_LIN_BASE >> 12) * 4]  ; PT0 entry for 0xC8000
    mov eax, UMB_PHYS_BASE | 7                          ; backing base, present/rw/user
    mov ecx, UMB_BYTES >> 12                            ; 40 pages
.umb_map:
    mov [edi], eax
    add eax, 0x1000
    add edi, 4
    loop .umb_map
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

; ---- #GP (vector 13): a sensitive instruction faulted. Has an error code. ----
monitor:
    pushad
    mov ax, 0x10                  ; flat 4 GiB DS to reach the guest's high stacks
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
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
    mov al, dl                    ; unhandled sensitive instruction: signal its opcode
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
    cmp byte [fs:vif], 0
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
    or ax, 0x0200               ; frame keeps real IF = 1
    mov word [ebp+8], ax        ; update guest flags (VM in high word preserved)
    inc word [ebp]              ; POPF is 1 byte
    call maybe_deliver          ; POPF may re-enable interrupts
    jmp .done_gp
.intn:
    movzx ebx, byte [eax+1]      ; INT vector operand
    cmp bl, 0xC0                 ; TOKAEMM-private monitor call?
    jne .intn_reflect
    cmp word [esp+20], 0x544D    ; guest DX == 'TM' (XMS-move memcpy)?
    je .intn_memcpy
    cmp word [esp+20], 0x4D50    ; guest DX == 'PM' (EMS frame remap)?
    je .intn_remap
    jmp .intn_reflect            ; foreign INT 0xC0: reflect like any other
.intn_memcpy:
    add word [ebp], 2            ; skip past INT 0xC0
    call flat_memcpy
    jmp .done_gp
.intn_remap:
    add word [ebp], 2
    call frame_remap
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
    or ax, 0x0200             ; frame keeps real IF = 1
    mov word [ebp+8], ax
    call maybe_deliver         ; IRET may re-enable interrupts
    jmp .done_gp
.done_gp:
    popad
    add esp, 4                   ; discard the #GP error code
    iretd

; ---- IRQ0 timer (vector 8) / IRQ1 keyboard (vector 9). No error code. ----
irq8:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    cmp byte [fs:vif], 0
    jne .go
    or byte [fs:vip], 1          ; VIF clear: coalesce pending, but EOI now so the
    mov al, 0x20                 ; PIC keeps delivering (deliver on the next STI/POPF)
    out 0x20, al
    popad
    iretd
.go:
    mov ebx, 8
    call reflect_vector
    popad
    iretd
irq9:
    pushad
    mov ax, 0x10
    mov ds, ax
    mov ax, 0x20
    mov fs, ax
    lea ebp, [esp + 32]
    cmp byte [fs:vif], 0
    jne .go
    or byte [fs:vip], 2          ; coalesce pending + EOI now (see irq8)
    mov al, 0x20
    out 0x20, al
    popad
    iretd
.go:
    mov ebx, 9
    call reflect_vector
    popad
    iretd

; Reflect an interrupt into the guest's real-mode IVT handler.
;   in: EBX = vector, EBP = &frame.eip, FS = driver data.  clobbers eax,ecx,edx,edi
reflect_vector:
    mov edx, [ebp+16]            ; guest SS
    shl edx, 4                   ; edx = guest stack base (linear)
    mov ax, [ebp+8]             ; guest flags, IF := VIF
    and ax, 0xFDFF
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

; If VIF is set and an IRQ is pending, deliver the highest-priority one.
;   in: EBP = &frame.eip, FS = driver data.  clobbers eax,ebx,ecx,edx,edi
maybe_deliver:
    cmp byte [fs:vif], 0
    je .none
    movzx ebx, byte [fs:vip]
    test bl, bl
    jz .none
    test bl, 1
    jz .try9
    and byte [fs:vip], 0xFE
    mov ebx, 8
    jmp reflect_vector           ; tail: ret returns to maybe_deliver's caller
.try9:
    and byte [fs:vip], 0xFD
    mov ebx, 9
    jmp reflect_vector
.none:
    ret

; Ring-0 flat memcpy for the XMS block MOVE (INT 0xC0 monitor service). The guest
; driver put dst linear in EDI, src linear in ESI, byte count in ECX (all live in
; the pushad frame), and enabled A20 first. deliver_exception NULLED ES/DS/FS/GS on
; the V86->ring0 entry, and a null selector faults a PM memory access, so reload ES
; to the flat selector (DS is already 0x10 from monitor entry). Reads the three
; args from the pushad slots (this routine was `call`ed, so +4 for the return addr:
; guest EDI=[esp+4], ESI=[esp+8], ECX=[esp+28]). The frame is only read, never
; written, so .done_gp's popad restores the guest's registers afterwards.
flat_memcpy:
    mov ax, 0x10
    mov es, ax                    ; ES = flat (base 0); DS already 0x10
    mov edi, [esp + 4]            ; guest EDI = dst linear
    mov esi, [esp + 8]            ; guest ESI = src linear
    mov ecx, [esp + 28]           ; guest ECX = byte count
    in al, 0x92                   ; save port 0x92 + force A20 on: EMBs above the HMA
    mov ah, al                    ; have bit 20 set, so the flat physical access needs
    or al, 2                      ; A20 or it wraps at 1 MB (apply_a20 masks bit 20).
    out 0x92, al
    cld
    rep movsb                     ; DS:ESI -> ES:EDI, both flat
    mov al, ah
    out 0x92, al                  ; restore A20 to the guest's prior state
    ret

; Ring-0 EMS frame remap (INT 0xC0 'PM'). Guest EBX = frame-slot linear base,
; guest ECX = backing physical base, or 0 to restore the INIT mapping (the
; UMB-backing bytes the INIT .umb_map loop pointed this window at). Rewrites
; the slot's 4 PTEs in PT0 and reloads CR3 — the 386 full-TLB-flush idiom.
; Private, cookie-gated, single caller (ems_remap_slot) validates -> no arg
; checks. Args from the pushad slots via the call frame: EBX=[esp+20],
; ECX=[esp+28] (cf. flat_memcpy). DS is already flat 0x10 from monitor entry;
; FS = 0x20 (driver data) for pd_lin. The frame is only read, so .done_gp's
; popad restores the guest registers.
frame_remap:
    mov ebx, [esp+20]             ; guest EBX = slot linear base
    mov ecx, [esp+28]             ; guest ECX = backing phys (0 = unmap)
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
tss:
    times 0x90 db 0

align 4
mon_stack:
    times 0x400 db 0
mon_stack_top:

align 4096
tables:
    ; PD (1 page) + 4 PT (4 pages) = 0x5000, plus up to 0xFF0 of page-rounding slack
    ; (pd_lin = round_up_4k(base+tables), base is only paragraph-aligned) -> 0x6000.
    times 0x6000 db 0
resident_end:
