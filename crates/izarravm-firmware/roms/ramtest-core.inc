; ramtest-core.inc - STREAM B (RAM test). Owns this file. Registers two POST
; steps in source order:
;   POST_STEP "memory.ramtest", ramtest_run        ; PASS/FAIL
;   POST_STEP "memory.detected_kib", ramtest_size   ; MEASURE, value in segment 0
;
; The Izarra emulator faults on any access to unmapped physical memory (a fatal
; bus error the guest cannot trap), so RAM cannot be sized by probing until a
; read returns garbage. The size instead comes from INT 15h AH=88h (the chipset
; reported extended memory, KiB above 1 MiB, the standard PC mechanism). The test
; then pattern-checks the RAM the reported size proves is present: all of
; conventional memory plus the extended region, and the on-screen count is the
; address actually under test, not a cosmetic timer. Every access uses the unreal
; FS flat segment (set up at bring-up), so a single 32-bit pointer reaches any
; physical address and the 16-bit segment limit is never in the way.

; Scratch layout inside RAMTEST_SCRATCH (segment 0, 160 bytes from 0x0560):
%define RAMTEST_KIB     (RAMTEST_SCRATCH + 0x00)    ; dword: detected total KiB
%define RAMTEST_VALSTR  (RAMTEST_SCRATCH + 0x10)    ; ASCIIZ decimal of the KiB

; Conventional pattern pass: dword-walk [RT_CONV_LO, RT_CONV_TOP), skipping the
; POST stack page and the VDTS result block. Starts above the foundation scratch,
; stops at the VGA aperture.
%define RT_CONV_LO    0x00000800
%define RT_CONV_TOP   0x000a0000
%define RT_STK_LO     0x00006000        ; reserved POST stack page (top STACK_TOP)
%define RT_STK_HI     0x00007000
%define RT_PAT_A      0xa5a5a5a5

; Extended region base (1 MiB) and the strides that set the walk's density, and
; with it the real time the walk takes (each unreal flat access is costly in the
; emulator's timed bus model, ~2000 cycles). Slow POST (the GUI) walks a fine
; stride so the real bus accesses pace the on-screen count over a few seconds at
; 25 MHz; fast POST (headless and tests) walks a coarse stride so the costly
; accesses stay inside the POST cycle budget.
%define RT_EXT_BASE        0x00100000
%define RT_CONV_STRIDE      0x100         ; slow: one check per 256 bytes
%define RT_CONV_STRIDE_FAST 0x1000        ; fast: one check per 4 KiB page
%define RT_EXT_STRIDE       0x100         ; slow: one check per 256 bytes
%define RT_EXT_STRIDE_FAST  0x00100000    ; fast: one check per 1 MiB
; Redraw the count and bar every this many bytes of address progress; redrawing
; on every dword would swamp the walk with mode-13h fills.
%define RT_DRAW_STEP  0x00010000        ; 64 KiB

POST_STEP "memory.ramtest", ramtest_run
POST_STEP "memory.detected_kib", ramtest_size

; ramtest_run: pattern-test conventional RAM and the extended region, driving the
; on-screen count and progress bar off the address actually under test, then
; publish the total detected KiB. HRES_PASS on a clean walk, HRES_FAIL on the
; first mismatch. DS = BDA on entry; reloaded as needed.
;
; The walk index lives in EBX, the extended top in ESI, and the next redraw
; threshold in EDI, all of which the INT 08h timer ISR preserves (it touches only
; AX and DS), so an IRQ0 firing mid-walk cannot corrupt them. The mode-13h draw
; routines use 16-bit pusha/popa and never touch the high halves of EBX/ESI/EDI,
; so those survive a redraw too.
ramtest_run:
    push ebx
    push ecx
    push edx
    push esi
    push edi
    push ebp

    ; --- detected size first: total top = 1 MiB + extended (INT 15h AH=88h). The
    ; conventional and extended walks both drive the bar off this top, so size it
    ; before either walk runs. ---
    mov ax, 0x8800
    int 0x15
    movzx ecx, ax                  ; ECX = extended KiB
    mov esi, ecx
    shl esi, 10                    ; extended bytes
    add esi, RT_EXT_BASE           ; ESI = top physical address (1 MiB + extended)
    mov ebp, ecx
    add ebp, 1024                  ; EBP = total KiB, the bar's 100% denominator

    ; --- publish the detected size now so the records hold even if the walk
    ; aborts on Esc. ---
    push ecx
    mov ebx, ebp
    xor cx, cx
    mov ds, cx
    mov [RAMTEST_KIB], ebx         ; total KiB (segment 0)
    mov cx, BDA_SEG
    mov ds, cx
    mov [0x0013], bx               ; INT 12h word (low 16 bits of KiB)
    pop ecx                        ; ECX = extended KiB (no longer needed)

    ; --- choose both strides from the POST-pacing port. ECX takes the conventional
    ; stride, EDX the extended stride; both survive the walks (the draw routines
    ; and the timer ISR preserve them). ---
    in al, PORT_FAST_POST
    test al, 1
    mov ecx, RT_CONV_STRIDE        ; slow defaults
    mov edx, RT_EXT_STRIDE
    jz .have_stride
    mov ecx, RT_CONV_STRIDE_FAST
    mov edx, RT_EXT_STRIDE_FAST
.have_stride:

    xor edi, edi                   ; first redraw threshold (drawn at addr 0)

    ; --- conventional pass over [RT_CONV_LO, RT_CONV_TOP), skipping reserved ---
    mov ebx, RT_CONV_LO
.conv:
    cmp ebx, RT_CONV_TOP
    jae .conv_done
    call ramtest_draw_maybe        ; redraw on each 64 KiB the walk crosses
    call ramtest_esc_pressed
    jc .skipped
    cmp ebx, RT_STK_LO             ; skip the running stack page
    jb .conv_chk_result
    cmp ebx, RT_STK_HI
    jb .conv_skip
.conv_chk_result:
    cmp ebx, RESULT_BLOCK          ; skip the VDTS result block
    jb .conv_test
    cmp ebx, RESULT_LIMIT
    jb .conv_skip
.conv_test:
    call ramtest_check_dword
    jc .fail
.conv_skip:
    add ebx, ecx                   ; conventional stride
    jmp .conv
.conv_done:

    ; --- extended pass over [RT_EXT_BASE, ESI) at the chosen stride ---
    mov ebx, RT_EXT_BASE
.ext:
    cmp ebx, esi
    jae .ext_done
    call ramtest_draw_maybe
    call ramtest_esc_pressed
    jc .skipped
    call ramtest_check_dword
    jc .fail
    add ebx, edx
    jmp .ext
.ext_done:
    ; Final draw: the full detected size and a 100% bar.
    mov ebx, esi
    call ramtest_draw
    mov al, HRES_PASS
    jmp .done

.skipped:
    ; Esc fast-forwards: show the full detected size, a full bar, and the note.
    mov ebx, esi
    call ramtest_draw
    push cs
    pop ds
    mov si, str_post_skip
    call gfx_post_note
    mov al, HRES_PASS
    jmp .done

.fail:
    mov al, HRES_FAIL
.done:
    pop ebp
    pop edi
    pop esi
    pop edx
    pop ecx
    pop ebx
    ret

; ramtest_draw_maybe: redraw the count and bar only when the walk index EBX has
; reached the next RT_DRAW_STEP threshold in EDI, then advance the threshold past
; EBX. Keeps the redraw rate at one per 64 KiB of address progress regardless of
; the stride. Under fast POST it draws nothing per step; the single final draw in
; ramtest_run paints the full count and bar, so headless runs stay inside the
; cycle budget. Preserves all registers except flags.
ramtest_draw_maybe:
    push ax
    in al, PORT_FAST_POST          ; fast POST: skip the per-step redraw
    test al, 1
    jnz .skip
    cmp ebx, edi
    jb .skip
    call ramtest_draw
.advance:
    add edi, RT_DRAW_STEP          ; next threshold strictly above EBX
    cmp edi, ebx
    jbe .advance
.skip:
    pop ax
    ret

; ramtest_draw: paint "Testing memory: NNNNN KB" and the bar from the current
; walk index EBX. The count is EBX >> 10 (KiB); the bar percent is
; current_KiB * 100 / total_KiB in 32-bit math (EBP holds the total). EBX, ESI,
; EDI, EBP and their high halves survive (gfx uses 16-bit pusha/popa).
ramtest_draw:
    push eax
    push ecx
    push edx
    mov eax, ebx
    shr eax, 10                    ; current KiB (fits 16 bits: <= 24576)
    push eax                       ; save current KiB for the bar
    call gfx_post_mem              ; AX = current KiB
    pop eax                        ; current KiB
    mov ecx, 100
    mul ecx                        ; EDX:EAX = current_KiB * 100 (the dividend)
    mov ecx, ebp                   ; total KiB
    test ecx, ecx
    jz .no_pct                     ; guard a zero denominator
    div ecx                        ; EAX = percent (EDX:EAX / ECX)
.no_pct:
    call gfx_bar_fill              ; AX = percent
    pop edx
    pop ecx
    pop eax
    ret

; ramtest_check_dword: write an address-keyed value to [fs:EBX], read it back,
; then restore the original. The value is the address XOR a fixed mask, so it
; differs per cell and exercises both bit polarities across the sweep, catching
; stuck data and address bits with one round-trip (each unreal bus access is
; costly, so the check stays lean). CF=1 on mismatch (cell still restored), CF=0
; on success. Preserves EBX; clobbers EAX and saves/uses ESI for the original.
ramtest_check_dword:
    push esi
    mov esi, [fs:ebx]              ; save original
    mov eax, ebx
    xor eax, RT_PAT_A              ; address-keyed value
    mov [fs:ebx], eax
    cmp [fs:ebx], eax
    jne .bad
    mov [fs:ebx], esi              ; restore
    pop esi
    clc
    ret
.bad:
    mov [fs:ebx], esi              ; restore even on failure
    pop esi
    stc
    ret

; ramtest_size: format RAMTEST_KIB as a fixed-width 8-digit ASCIIZ decimal into
; RAMTEST_VALSTR (leading zeros keep payload_len deterministic across sizes), and
; return SI = its segment-0 offset, AL = HRES_MEASURE.
ramtest_size:
    push ebx
    push ecx
    push edx
    push di
    xor bx, bx
    mov ds, bx
    mov eax, [RAMTEST_KIB]
    mov di, RAMTEST_VALSTR + 8
    mov byte [di], 0
    mov cx, 8
    mov ebx, 10
.digit:
    dec di
    xor edx, edx
    div ebx                        ; EAX /= 10, EDX = remainder
    add dl, '0'
    mov [di], dl
    loop .digit
    mov si, RAMTEST_VALSTR
    mov al, HRES_MEASURE
    pop di
    pop edx
    pop ecx
    pop ebx
    ret

; ramtest_esc_pressed: CF=1 if Esc (scancode 0x01) is waiting in the BDA ring,
; else CF=0. Peeks the ring head without consuming; the keyboard ISR (INT 09h)
; fills it. Preserves all registers except flags.
ramtest_esc_pressed:
    push ax
    push bx
    push ds
    mov ax, BDA_SEG
    mov ds, ax
    mov bx, [KB_BUF_HEAD]
    cmp bx, [KB_BUF_TAIL]
    je .none
    mov ax, [bx]                    ; AH = scancode, AL = ASCII
    cmp ah, 0x01                    ; Esc make code
    jne .none
    pop ds
    pop bx
    pop ax
    stc
    ret
.none:
    pop ds
    pop bx
    pop ax
    clc
    ret
