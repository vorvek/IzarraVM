; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; ============================================================================
; Guest XMS driver (16-bit real mode / V86). Reached only via the INT
; 2Fh hook (install-check / get-entry) and the far-callable control entry, never
; by fall-through (INIT above ends in a far jump). Own data via cs: overrides
; because the far-callable entry runs with the caller's DS.
;
; Everything from here to the monitor (`bits 32`) is guest-facing V86 code.
; It shares the 386 ISA exposed by both 386 clock modes.
; ============================================================================
cpu 386

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
    cmp ah, 0xF0
    je xf_toka_pools
    cmp ah, 0x12
    ja .unimpl                   ; 13h+ : not implemented
    push bx
    mov bl, ah                   ; zero-extend AH through the 16-bit ABI
    xor bh, bh
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
    xor dx, dx
    mov dl, [cs:hma_available]
    retf

; F0h private pool query used by Toka-DOS MEM. F000h is the legacy totals
; query. F001h also returns free VCPI KB in DX and uses a versioned magic.
xf_toka_pools:
    cmp al, 1
    je .split
    mov ax, 0x544B               ; 'TK'
    mov bx, [cs:xms_category_kb]
    mov cx, [cs:ems_category_kb]
    retf
.split:
    mov ax, 0x544C               ; split-pool result
    mov bx, [cs:xms_category_kb]
    mov cx, [cs:ems_category_kb]
    mov dx, [cs:vcpi_free]
    shl dx, 2                    ; pages -> KB
    retf

; 01h request HMA / 02h release HMA (a flag; no /HMAMIN gate).
xf_req_hma:
    cmp byte [cs:hma_available], 0
    je .absent
    cmp byte [cs:hma_owned], 0
    jne .inuse
    mov byte [cs:hma_owned], 1
    jmp xms_ok
.inuse:
    mov bl, 0x91                 ; HMA already in use
    jmp xms_fail
.absent:
    mov bl, 0x90                 ; HMA does not exist
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
    shr al, 1                    ; bit 1 (A20) -> bit 0
    and ax, 1                    ; AX = 0/1 (clears AH too)
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
xf_query_free:
    pushf
    cli
    call arena_longest_clear      ; AX = longest contiguous run in 4 KB pages
    mov dx, [cs:xms_free]
    shl ax, 2                     ; pages -> KB
    shl dx, 2
    xor bl, bl
    popf
    retf

; 09h allocate EMB: DX=KB in -> DX=handle. The XMS pool is reserved in
; 4 KB pages, while the handle retains the exact requested size for bounds.
xf_alloc:
    push cx
    push si
    push di
    push bp
    pushf
    cli
    mov [cs:xms_need_kb], dx
    mov ax, dx
    mov cx, ax
    shr cx, 2
    and ax, 3
    jz .pages_ready
    inc cx                        ; ceil(requested KB / 4)
.pages_ready:
    mov [cs:xms_need_pages], cx
    mov si, xms_table
    mov di, XMS_HANDLES
    xor bp, bp                    ; handle counter (1-based)
.slot:
    inc bp
    cmp byte [cs:si], 0
    je .got
    add si, XMS_SLOT
    dec di
    jnz .slot
    mov bl, 0xA1                  ; out of handles
    jmp .fail
.got:
    call arena_find_clear_run     ; CX pages -> AX = absolute start page
    jc .oom
    mov di, ax
    call arena_mark_xms
    mov byte [cs:si], 1
    mov byte [cs:si+1], 0
    mov ax, [cs:xms_need_kb]
    mov [cs:si+2], ax
    mov ax, di
    shl ax, 2
    mov [cs:si+4], ax             ; base_kb = absolute page * 4
    mov ax, [cs:xms_need_pages]
    mov [cs:si+6], ax
    sub [cs:xms_free], ax
    mov dx, bp                    ; handle out
    mov ax, 1
    xor bl, bl
    popf
    pop bp
    pop di
    pop si
    pop cx
    retf
.oom:
    mov bl, 0xA0
.fail:
    xor ax, ax
    popf
    pop bp
    pop di
    pop si
    pop cx
    retf

; 0Ah free EMB: DX=handle.
xf_free:
    push cx
    push si
    pushf
    cli
    call slot_of                  ; -> SI = slot, or CF + BL=0xA2
    jc .bad
    cmp byte [cs:si+1], 0         ; locked?
    jne .locked
    mov ax, [cs:si+4]
    shr ax, 2                     ; base_kb -> absolute page
    mov cx, [cs:si+6]
    call arena_clear_xms
    add [cs:xms_free], cx
    mov word [cs:si], 0
    mov word [cs:si+2], 0
    mov word [cs:si+4], 0
    mov word [cs:si+6], 0
    mov ax, 1
    xor bl, bl
    popf
    pop si
    pop cx
    retf
.locked:
    mov bl, 0xAB
    jmp .fail
.bad:
.fail:
    xor ax, ax
    popf
    pop si
    pop cx
    retf

; 0Ch lock EMB: DX=handle -> DX:BX = 32-bit linear base, lock++.
xf_lock:
    push si
    call slot_of
    jc .bad
    cmp byte [cs:si+1], 0xFF
    je .ovf
    inc byte [cs:si+1]
    mov bx, [cs:si+4]             ; base_kb
    mov dx, bx
    shl bx, 10                    ; DX:BX = base_kb << 10 (the linear base)
    shr dx, 6
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

; 0Fh resize EMB: BX=new KB, DX=handle. Release and re-place atomically.
; Re-places without copying the old contents to the new base — data is
; preserved only when the allocator returns the same base (no fragmentation). This
; mirrors the retired FlatEmbAllocator::resize; a
; copy-on-relocate (via the INT 0xC0 memcpy) is a future-milestone fidelity item.
xf_resize:
    push cx
    push si
    push di
    pushf
    cli
    call slot_of
    jc .bad
    cmp byte [cs:si+1], 0         ; locked?
    jne .locked
    cmp bx, [cs:si+2]             ; same size?
    je .ok
    mov ax, [cs:si+4]
    shr ax, 2                     ; old absolute start page
    mov cx, [cs:si+6]
    call arena_clear_xms
    add [cs:xms_free], cx
    mov [cs:xms_need_kb], bx
    mov ax, bx
    mov cx, ax
    shr cx, 2
    and ax, 3
    jz .pages_ready
    inc cx
.pages_ready:
    mov [cs:xms_need_pages], cx
    call arena_find_clear_run
    jc .restore
    mov di, ax                    ; new absolute start page
    call arena_mark_xms
    sub [cs:xms_free], cx
    mov ax, [cs:xms_need_kb]
    mov [cs:si+2], ax
    mov ax, di
    shl ax, 2
    mov [cs:si+4], ax
    mov ax, [cs:xms_need_pages]
    mov [cs:si+6], ax
.ok:
    mov ax, 1
    xor bl, bl
    popf
    pop di
    pop si
    pop cx
    retf
.restore:
    mov ax, [cs:si+4]
    shr ax, 2
    mov cx, [cs:si+6]
    call arena_mark_xms
    sub [cs:xms_free], cx
    mov bl, 0xA0
    jmp .fail
.fail:
    xor ax, ax
    popf
    pop di
    pop si
    pop cx
    retf
.locked:
    mov bl, 0xAB
    jmp .fail
.bad:
    jmp .fail

; 0Bh move EMB: DS:SI -> descriptor {len(dd) srcH(w) srcOff(dd) dstH(w) dstOff(dd)}.
; Resolve both endpoints to linear, then trap to the monitor for the flat copy.
; The monitor reads src/dst/len from [fs:xms_mv_*] (this side has no 32-bit regs).
xf_move:
    push cx
    push si
    push di
    push bp
    mov ax, [si]                  ; length (DS:SI +0), word pair
    mov dx, [si+2]
    mov cx, ax
    or cx, dx
    jz .zero                      ; zero length = legal no-op success
    test al, 1
    jnz .badlen                   ; odd length -> A7h
    mov [cs:xms_mv_len], ax
    mov [cs:xms_mv_len+2], dx
    mov ax, [si+6]                ; src offset -> resolve's input scratch
    mov dx, [si+8]
    mov [cs:xms_rv_off], ax
    mov [cs:xms_rv_off+2], dx
    mov bx, [si+4]                ; src handle
    call resolve                  ; -> DX:AX = linear, or CF + AL=1(handle)/2(offset)
    jc .src_err
    mov [cs:xms_mv_src], ax
    mov [cs:xms_mv_src+2], dx
    mov ax, [si+12]               ; dst offset
    mov dx, [si+14]
    mov [cs:xms_rv_off], ax
    mov [cs:xms_rv_off+2], dx
    mov bx, [si+10]               ; dst handle
    call resolve
    jc .dst_err
    mov [cs:xms_mv_dst], ax
    mov [cs:xms_mv_dst+2], dx
    mov dx, 0x544D               ; monitor-call cookie 'TM'
    int 0xC0                     ; ring-0 flat memcpy of [fs:xms_mv_*]
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

; First clear run in the XMS pool. in: CX = page count. out: AX = absolute
; start page, CF clear; or CF set. Preserves BX/CX/DX/SI/DI.
arena_find_clear_run:
    push bx
    push dx
    push si
    push di
    mov bx, [cs:xms_pool_base]
    mov ax, [cs:xms_pool_base+2]
    shr bx, 12
    shl ax, 4
    or bx, ax                     ; BX = first absolute page
    mov dx, [cs:xms_pool_end]
    mov ax, [cs:xms_pool_end+2]
    shr dx, 12
    shl ax, 4
    or dx, ax                     ; DX = one-past-last absolute page
    jcxz .found_empty
    xor si, si                    ; current clear-run length
.scan:
    cmp bx, dx
    jae .none
    bt word [cs:arena_bmp], bx
    jc .allocated
    test si, si
    jnz .extend
    mov di, bx                    ; start of this clear run
.extend:
    inc si
    cmp si, cx
    jae .found
    jmp .next
.allocated:
    xor si, si
.next:
    inc bx
    jmp .scan
.found_empty:
    mov di, bx
.found:
    mov ax, di
    pop di
    pop si
    pop dx
    pop bx
    clc
    ret
.none:
    pop di
    pop si
    pop dx
    pop bx
    stc
    ret

; Longest clear run in the XMS pool. out: AX = page count. Preserves all
; other registers used by the helper.
arena_longest_clear:
    push bx
    push cx
    push dx
    push si
    mov bx, [cs:xms_pool_base]
    mov ax, [cs:xms_pool_base+2]
    shr bx, 12
    shl ax, 4
    or bx, ax
    mov dx, [cs:xms_pool_end]
    mov ax, [cs:xms_pool_end+2]
    shr dx, 12
    shl ax, 4
    or dx, ax
    xor cx, cx                    ; current clear-run length
    xor si, si                    ; longest clear-run length
.scan:
    cmp bx, dx
    jae .done
    bt word [cs:arena_bmp], bx
    jc .allocated
    inc cx
    cmp cx, si
    jbe .next
    mov si, cx
    jmp .next
.allocated:
    xor cx, cx
.next:
    inc bx
    jmp .scan
.done:
    mov ax, si
    pop si
    pop dx
    pop cx
    pop bx
    ret

; Mark an XMS-owned run. in: AX = absolute start page, CX = page count.
arena_mark_xms:
    push bx
    push cx
    mov bx, ax
    jcxz .done
.next:
    bts word [cs:arena_bmp], bx
    inc bx
    loop .next
.done:
    pop cx
    pop bx
    ret

; Clear an XMS-owned run from the allocation bitmap.
; in: AX = absolute start page, CX = page count.
arena_clear_xms:
    push bx
    push cx
    mov bx, ax
    jcxz .done
.next:
    btr word [cs:arena_bmp], bx
    inc bx
    loop .next
.done:
    pop cx
    pop bx
    ret

; Resolve a move endpoint. in: BX=handle, [cs:xms_rv_off]=32-bit offset,
; [cs:xms_mv_len]=length. out: DX:AX = linear, CF clear; or CF set +
; AL=1 (bad handle) / AL=2 (bad offset). Handle 0 => the offset dword is a
; real-mode seg:off (high=seg, low=off). 32-bit values use word pairs.
; Clobbers ax, bx, dx.
resolve:
    test bx, bx
    jnz .handle
    mov ax, [cs:xms_rv_off+2]     ; segment
    mov dx, ax
    shr dx, 12                    ; seg<<4 high bits -> linear bits 16..19
    shl ax, 4
    add ax, [cs:xms_rv_off]       ; + offset
    adc dx, 0                     ; DX:AX = seg*16 + off
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
    mov ax, [cs:si+2]             ; size bytes = size_kb << 10, as DX:AX
    mov dx, ax
    shr dx, 6
    shl ax, 10
    mov bx, [cs:xms_rv_off+2]     ; offset > size? (32-bit compare, high first)
    cmp bx, dx
    ja .bado_pop
    jb .off_ok
    mov bx, [cs:xms_rv_off]
    cmp bx, ax
    ja .bado_pop
.off_ok:
    sub ax, [cs:xms_rv_off]       ; remaining = size - offset
    sbb dx, [cs:xms_rv_off+2]
    mov bx, [cs:xms_mv_len+2]     ; length > remaining?
    cmp bx, dx
    ja .bado_pop
    jb .len_ok
    mov bx, [cs:xms_mv_len]
    cmp bx, ax
    ja .bado_pop
.len_ok:
    mov ax, [cs:si+4]             ; linear = (base_kb << 10) + offset
    mov dx, ax
    shr dx, 6
    shl ax, 10
    add ax, [cs:xms_rv_off]
    adc dx, [cs:xms_rv_off+2]
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

; --- UMB (XMS 10h/11h/12h) over the paged window [UMB_SEG_BASE, +PARAS) ---

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
    sub ax, UMB_SEG_BASE          ; the EMS page frame carves the top
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
; DOS=UMB uses it). An exact largest-gap needs a sorted walk.
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
