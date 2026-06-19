; izbios-result.inc - foundation: POST step registration + the live VDTS
; result-block builder. The macro accumulates (name, handler) pairs across every
; %include; izbios-tables.inc (last) emits the contiguous table. result_append
; owns the three header fields and recomputes the additive u16 checksum on each
; append, so streams add records by name with no reserved template slots.

; --- POST step registration -------------------------------------------------
; In its OWN .inc a stream writes:  POST_STEP "dotted.name", handler_label
; and defines  handler_label: ... ret  in the same file. Labels are file-local
; and the table is emitted once at the end, so two streams never conflict.
%assign POST_STEP_COUNT 0
%macro POST_STEP 2
    post_step_name_%[POST_STEP_COUNT]:
        db %1, 0
    %xdefine POST_STEP_HANDLER_%[POST_STEP_COUNT] %2
    %assign POST_STEP_COUNT POST_STEP_COUNT + 1
%endmacro

; --- result_init: lay the empty VDTS header at 0:RESULT_BLOCK (DS=0). --------
result_init:
    mov word [RESULT_BLOCK], 'VD'     ; magic bytes 'V','D'
    mov word [RESULT_BLOCK + 2], 'TS' ; magic bytes 'T','S'
    mov word [RESULT_BLOCK + 4], 1    ; version
    mov word [RESULT_BLOCK + 6], 0    ; record_count
    mov word [RESULT_BLOCK + 8], 0    ; payload_len
    mov word [RESULT_BLOCK + 10], 0   ; checksum
    ret

; --- result_append: append one record line to the VDTS payload. -------------
;   AL = status (RESULT_BEGIN/PASS/FAIL/MEASURE)
;   SI = CS-relative pointer to the ASCIIZ record name (ROM)
;   DX = segment-0 offset of the ASCIIZ value (only read when AL = RESULT_MEASURE)
; Enter with DS = 0 (the block and any value live in segment 0). Writes
; "<STATUS> <name>[ <value>]\n", bumps record_count, updates payload_len, and
; recomputes the checksum. Preserves all caller registers.
result_append:
    push ax
    push bx
    push cx
    push si
    push di
    push es
    xor cx, cx
    mov es, cx                        ; ES = 0 (block segment, stosb target)
    mov bx, [RESULT_BLOCK + 8]        ; current payload_len
    lea di, [bx + RESULT_BLOCK + 12]  ; write cursor = end of payload
    ; status prefix (ROM string selected by status code)
    push si                           ; save name pointer
    mov bl, al
    xor bh, bh
    shl bx, 1
    mov si, [cs:ra_prefix_table + bx]
    call ra_copy_cs
    pop si                            ; name pointer
    call ra_copy_cs
    cmp al, RESULT_MEASURE
    jne .no_value
    mov byte [es:di], ' '
    inc di
    mov si, dx                        ; value offset (segment 0)
    call ra_copy_ds
.no_value:
    mov byte [es:di], 0x0a            ; newline
    inc di
    ; payload_len = di - (RESULT_BLOCK + 12)
    lea ax, [di - (RESULT_BLOCK + 12)]
    mov [RESULT_BLOCK + 8], ax
    inc word [RESULT_BLOCK + 6]       ; record_count
    call ra_recompute_checksum
    pop es
    pop di
    pop si
    pop cx
    pop bx
    pop ax
    ret

; Copy ASCIIZ from CS:SI to ES:DI, stopping at (not copying) the 0 terminator.
; Advances SI past the terminator and DI past the last byte. Preserves AX.
ra_copy_cs:
    push ax
.loop:
    mov al, [cs:si]
    inc si
    test al, al
    jz .done
    stosb
    jmp .loop
.done:
    pop ax
    ret

; Copy ASCIIZ from DS:SI (segment 0) to ES:DI, same contract as ra_copy_cs.
ra_copy_ds:
    push ax
.loop:
    mov al, [si]
    inc si
    test al, al
    jz .done
    stosb
    jmp .loop
.done:
    pop ax
    ret

; Recompute the additive u16 checksum over the whole payload, store at +10.
; DS = 0. Preserves all caller registers.
ra_recompute_checksum:
    push ax
    push bx
    push cx
    push si
    mov cx, [RESULT_BLOCK + 8]        ; payload_len
    mov si, RESULT_BLOCK + 12
    xor bx, bx
    jcxz .store
.loop:
    mov al, [si]
    xor ah, ah
    add bx, ax
    inc si
    loop .loop
.store:
    mov [RESULT_BLOCK + 10], bx
    pop si
    pop cx
    pop bx
    pop ax
    ret

ra_prefix_begin:   db 'BEGIN ', 0
ra_prefix_pass:    db 'PASS ', 0
ra_prefix_fail:    db 'FAIL ', 0
ra_prefix_measure: db 'MEASURE ', 0
ra_prefix_table:   dw ra_prefix_begin, ra_prefix_pass, ra_prefix_fail, ra_prefix_measure

str_suite_name:    db 'suite.izarra', 0

; --- Foundation reference step: always PASS, proves the harness end to end. ---
POST_STEP "self.framework", post_self_framework
post_self_framework:
    mov al, HRES_PASS
    ret

; --- Foundation self-test: round-trip a dword above 1 MiB through the unreal
; helpers, proving >1 MiB access works in the live BIOS (Stream B/C depend on it).
POST_STEP "self.extaccess", post_self_extaccess
post_self_extaccess:
    mov ebx, 0x00100000
    mov eax, 0x5a3c0f01
    call ramtest_poke32
    mov ebx, 0x00100000
    call ramtest_peek32
    cmp eax, 0x5a3c0f01
    je .ok
    mov al, HRES_FAIL
    ret
.ok:
    mov al, HRES_PASS
    ret
