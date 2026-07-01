; TOKAEMM.SYS — SP-4b M0 Task 1: prove the .SYS INIT seam. No V86 yet.
;
; A minimal char device driver. At SYSINIT the kernel calls STRATEGY (ES:BX ->
; request header, which we save) then INTERRUPT (do the work). For the C_INIT
; command we leave a visible marker on screen, then report "no resident code" by
; setting r_endaddr = header seg:0 so DOS unloads us again (fine for M0 task 1).
;
; Contract (verified against toka-dos/freedos/kernel):
;   - device.h: dh_next(dd), dh_attr(dw 0x8000=char), dh_strategy(dw),
;     dh_interrupt(dw), dh_name(8 bytes). Request packet: r_status @ +3 (S_DONE
;     = 0x0100), r_endaddr (far ptr) @ +14.
;   - execrh.asm: strategy at [si+6], interrupt at [si+8], ES:BX = request pkt,
;     both entered via `call far` so we must `retf`. INIT runs in REAL MODE.
;   - main.c init_device: r_endaddr == dhp (device header far ptr) => "no memory
;     taken" => driver is unlinked/unloaded.
;
; Marker: INT 21h AH=09h is not reliable at device-INIT time. We use INT 29h
; (fast console char-out, AL = char), which the kernel wires up very early in
; SYSINIT (main.c: setvec(0x29, int29_handler) "required for printf!") before
; device drivers load. It routes through the normal teletype/scroll path, so the
; marker flows into the boot output and stays visible in the final screen frame.
;
; Assemble: nasm -f bin crates/izarravm-firmware/roms/dos/tokaemm.asm \
;                       -o crates/izarravm-firmware/roms/dos/tokaemm.sys
    cpu 386
    org 0

; ---- device header (loaded at CS:0) ----
    dd 0xFFFFFFFF           ; dh_next = none (last driver)
    dw 0x8000               ; dh_attr = char device
    dw strategy             ; dh_strategy offset
    dw interrupt            ; dh_interrupt offset
    db 'TOKAEMM '           ; dh_name (8 bytes, space-padded)

rh_ptr: dd 0                ; saved ES:BX (request header far ptr)

; ---- STRATEGY: kernel passes ES:BX -> request header; just save it ----
strategy:
    mov [cs:rh_ptr], bx
    mov [cs:rh_ptr + 2], es
    retf

; ---- INTERRUPT: do the work (INIT only for M0 task 1) ----
interrupt:
    pusha
    push ds
    push es

    ; Print the marker via INT 29h (fast console output, AL = char), so it flows
    ; through the normal teletype/scroll path and stays in the boot text.
    push cs
    pop ds                  ; DS = header segment (string is CS-relative)
    mov si, banner
.copy:
    lodsb                   ; AL = next char, SI++
    test al, al
    jz .copied
    int 0x29                ; write AL to the console
    jmp .copy
.copied:

    ; Report status + "no resident code" into the request packet at ES:BX.
    les bx, [cs:rh_ptr]                 ; ES:BX -> request header
    mov word [es:bx + 14], 0           ; r_endaddr offset = 0
    mov word [es:bx + 16], cs          ; r_endaddr seg = header => no resident
    mov word [es:bx + 3], 0x0100       ; r_status = S_DONE

    pop es
    pop ds
    popa
    retf

; NUL-terminated marker string (printed char-by-char via INT 29h above).
banner: db 13, 10, 'TOKAEMM M0 task1: INIT ran', 13, 10, 0
