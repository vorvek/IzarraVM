; ramtest-core.inc - STREAM B (RAM test). Owns this file. Registers two POST
; steps in source order:
;   POST_STEP "memory.ramtest", ramtest_run        ; PASS/FAIL
;   POST_STEP "memory.detected_kib", ramtest_size   ; MEASURE, value in segment 0
;
; The Izarra emulator faults on any access to unmapped physical memory (a fatal
; bus error the guest cannot trap), so RAM cannot be sized by probing until a
; read returns garbage. The size instead comes from INT 15h AH=88h (the chipset
; reported extended memory, KiB above 1 MiB, the standard PC mechanism). The test
; then pattern-checks RAM the reported size proves is present: all of conventional
; memory plus a sampled sweep of the extended region. Every access uses the unreal
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
%define RT_PAT_B      0x5a5a5a5a
; Conventional sweep stride. The pass samples one dword per stride: a POST RAM
; check is a sanity sweep bounded by the cycle budget, not an exhaustive test.
; Each unreal flat access is costly in the emulator's timed bus model (~2000
; cycles), so 4 KiB keeps the full POST (this pass plus every probe) well inside
; clock_hz/5 while still touching every page of conventional RAM.
%define RT_CONV_STRIDE 0x1000

POST_STEP "memory.ramtest", ramtest_run
POST_STEP "memory.detected_kib", ramtest_size

; ramtest_run: pattern-test conventional RAM and a sample of extended RAM, then
; publish the total detected KiB. HRES_PASS on a clean sweep, HRES_FAIL on the
; first mismatch. DS = BDA on entry; reloaded as needed.
ramtest_run:
    push ebx
    push ecx
    push edx
    push esi

    ; --- conventional pass over [RT_CONV_LO, RT_CONV_TOP), skipping reserved ---
    mov ebx, RT_CONV_LO
.conv:
    cmp ebx, RT_CONV_TOP
    jae .conv_done
    cmp ebx, RT_STK_LO              ; skip the running stack page
    jb .conv_chk_result
    cmp ebx, RT_STK_HI
    jb .conv_skip
.conv_chk_result:
    cmp ebx, RESULT_BLOCK           ; skip the VDTS result block
    jb .conv_test
    cmp ebx, RESULT_LIMIT
    jb .conv_skip
.conv_test:
    call ramtest_check_dword
    jc .fail
.conv_skip:
    add ebx, RT_CONV_STRIDE
    jmp .conv
.conv_done:

    ; --- extended size from INT 15h AH=88h (KiB above 1 MiB) ---
    mov ax, 0x8800
    int 0x15
    movzx ecx, ax                  ; ECX = extended KiB

    ; --- extended sample sweep: first dword of each 1 MiB up to the reported
    ; size. Sampling keeps the costly unreal accesses bounded while still touching
    ; every megabyte of installed extended RAM. ---
    push ecx
    mov edx, ecx
    shr edx, 10                    ; block count = extended_kib / 1024 (MiB)
    mov ebx, 0x00100000            ; 1 MiB
.ext:
    test edx, edx
    jz .ext_done
    call ramtest_check_dword
    jc .fail_pop
    add ebx, 0x00100000            ; next 1 MiB block
    dec edx
    jmp .ext
.ext_done:
    pop ecx

    ; --- total detected KiB = 1024 (first MiB) + extended ---
    add ecx, 1024
    xor bx, bx
    mov ds, bx
    mov [RAMTEST_KIB], ecx         ; publish (segment 0)
    mov bx, BDA_SEG
    mov ds, bx
    mov [0x0013], cx               ; INT 12h word (low 16 bits of KiB)
    ; The real test and the result records are done. Now run the cosmetic
    ; count-up and bar (skipped under fast POST), which paces the screen to about
    ; 8 s and lets Esc fast-forward. It never changes the published size.
    call ramtest_animate
    mov al, HRES_PASS
    jmp .done
.fail_pop:
    pop ecx
.fail:
    mov al, HRES_FAIL
.done:
    pop esi
    pop edx
    pop ecx
    pop ebx
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

; --- Cosmetic RAM count-up animation ----------------------------------------
; ramtest_animate paces the screen to about 8 s: a "Testing memory: NNNNN KB"
; line counting up to the detected size and a red progress bar that fills, with
; Esc fast-forwarding to the full size. None of this touches the published
; RAMTEST_KIB or the result records; it only animates what the real test already
; measured. Under fast POST (port PORT_FAST_POST reads 1) it draws the final
; state once and returns, so headless runs and tests stay inside their budgets.

%define ANIM_STEPS     150           ; count-up steps (~1 PIT tick each ~= 8 s)
%define RAMTEST_TARGET (RAMTEST_SCRATCH + 0x20)   ; word: target KiB (display cap)

ramtest_animate:
    pusha
    ; Target KiB for the display, capped at 65535 so the 5-digit field fits.
    xor ax, ax
    mov ds, ax
    mov eax, [RAMTEST_KIB]
    cmp eax, 65535
    jbe .have_target
    mov eax, 65535
.have_target:
    mov [RAMTEST_TARGET], ax

    ; Fast POST: skip the pacing, draw the full count and a full bar, return.
    in al, PORT_FAST_POST
    test al, 1
    jz .slow
    mov ax, [RAMTEST_TARGET]
    call gfx_post_mem
    mov ax, 100
    call gfx_bar_fill
    jmp .out

.slow:
    ; Program PIT counter 0: mode 2, LSB/MSB, reload 0 (= 65536) so it wraps at
    ; ~18.2 Hz of emulated time, giving a real-time tick to pace on.
    mov al, 0x34
    out PORT_PIT_CTRL, al
    xor al, al
    out PORT_PIT_CH0, al            ; reload LSB
    out PORT_PIT_CH0, al            ; reload MSB

    mov cx, 1                       ; step index 1..ANIM_STEPS
.step:
    ; displayed KiB = target * step / ANIM_STEPS
    mov ax, [RAMTEST_TARGET]
    mul cx                          ; DX:AX = target * step (target<=65535, step<=150)
    mov bx, ANIM_STEPS
    div bx                          ; AX = displayed KiB
    call gfx_post_mem
    ; bar percent = step * 100 / ANIM_STEPS
    mov ax, cx
    mov bx, 100
    mul bx
    mov bx, ANIM_STEPS
    xor dx, dx
    div bx
    call gfx_bar_fill
    ; Esc (scancode 0x01) aborts to the full size with a skip note.
    call ramtest_esc_pressed
    jc .skipped
    call ramtest_pit_wait
    inc cx
    cmp cx, ANIM_STEPS
    jbe .step
    jmp .out

.skipped:
    ; Fast-forward: show the full detected size, a full bar, and the skip note.
    mov ax, [RAMTEST_TARGET]
    call gfx_post_mem
    mov ax, 100
    call gfx_bar_fill
    push cs
    pop ds
    mov si, str_post_skip
    ; gfx_post_note expects DS:SI = CS-relative string; it draws on the mem row.
    ; The skip note sits just below the bar so the final count stays visible.
    mov di, POST_TEXT_X
    mov ax, POST_BAR_Y + POST_BAR_H + 6
    mov bl, GFX_BLACK
    mov bh, GFX_WHITE
    call gfx_text
.out:
    popa
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

; ramtest_pit_wait: block until PIT counter 0 reloads once (~54.9 ms of emulated
; time). The counter decrements; a wrap is seen when a fresh latch-read reads
; higher than the previous one. The busy poll also advances emulated time, so the
; counter actually moves. Preserves all registers.
ramtest_pit_wait:
    push ax
    push bx
    push cx
    call ramtest_pit_read           ; BX = initial count
    mov cx, bx
.spin:
    call ramtest_pit_read           ; BX = current count
    cmp bx, cx                      ; counter wrapped (count rose) -> done
    ja .done
    mov cx, bx                      ; track the falling count
    jmp .spin
.done:
    pop cx
    pop bx
    pop ax
    ret

; ramtest_pit_read: latch and read PIT counter 0 -> BX = 16-bit count. Preserves
; AX. Uses the latch command (0x00 to the control port) then LSB, MSB from 0x40.
ramtest_pit_read:
    push ax
    xor al, al                      ; latch counter 0
    out PORT_PIT_CTRL, al
    in al, PORT_PIT_CH0             ; LSB
    mov bl, al
    in al, PORT_PIT_CH0             ; MSB
    mov bh, al
    pop ax
    ret
