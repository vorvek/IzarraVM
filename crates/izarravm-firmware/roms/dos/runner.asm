; RUNNER.COM - launched by AUTOEXEC as "RUNNER PROG.EXT". Frees memory, EXECs the
; named program, reads its DOS exit code, reports it to the Izarra unit-tester exit
; port, and halts. Built: nasm -f bin runner.asm -o runner.com
        cpu 8086
        org 0x100
start:
        ; Patch the parameter-block segment fields with our actual segment (CS).
        mov     ax, cs
        mov     [parblock + 4], ax      ; cmd-tail segment
        mov     [parblock + 8], ax      ; FCB1 segment
        mov     [parblock + 12], ax     ; FCB2 segment

        ; Free memory above us so the child has room (AH=4Ah; ES=PSP for a .COM).
        mov     ah, 0x4A
        mov     bx, 0x1000              ; keep 64 KiB (0x1000 paragraphs)
        int     0x21

        ; Copy the first token of our command tail (PSP:0x81..) into `fname`.
        mov     si, 0x81
.skip:
        lodsb
        cmp     al, ' '
        je      .skip
        dec     si                      ; SI -> first non-space char
        mov     di, fname
.copy:
        lodsb
        cmp     al, ' '
        je      .term
        cmp     al, 0x0D
        je      .term
        test    al, al
        je      .term
        stosb
        jmp     .copy
.term:
        xor     al, al
        stosb                           ; NUL-terminate

        ; EXEC the child: DS:DX = ASCIIZ name, ES:BX = parameter block.
        push    cs
        pop     es                      ; ES = CS
        mov     bx, parblock
        mov     dx, fname               ; DS = CS for a .COM
        mov     ax, 0x4B00
        int     0x21
        mov     al, 0xFF                ; EXEC failed (CF) -> sentinel code 255
        jc      .report
        ; Get the child's return code (AH=4Dh -> AL = code).
        mov     ah, 0x4D
        int     0x21
.report:
        mov     bl, al                  ; save code
        mov     al, 12
        out     0xE4, al                ; index = REG_EXIT
        mov     al, bl
        out     0xE5, al                ; data  = exit code
        mov     al, 3
        out     0xE6, al                ; command = CMD_EXIT -> machine stops
.hang:
        hlt
        jmp     .hang

; ---- data (execution never reaches here; .hang loops) ----
parblock:
        dw      0                       ; +0  env segment (0 = inherit)
        dw      child_tail              ; +2  cmd-tail offset
        dw      0                       ; +4  cmd-tail segment (patched to CS)
        dw      dummy_fcb               ; +6  FCB1 offset
        dw      0                       ; +8  FCB1 segment (patched to CS)
        dw      dummy_fcb               ; +10 FCB2 offset
        dw      0                       ; +12 FCB2 segment (patched to CS)
child_tail:
        db      0, 0x0D                 ; empty command tail for the child
dummy_fcb:
        db      0                       ; default drive
        times 11 db ' '                 ; blank 8.3 name
        times 25 db 0                   ; rest of the FCB
fname:
        times 80 db 0
