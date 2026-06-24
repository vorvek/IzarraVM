; TESTDEV.SYS - clean-room character device driver that proves real .SYS loading.
; A DEVICE= line in CONFIG.SYS loads it resident, SYSINIT runs its strategy then
; interrupt INIT on the real CPU, and its header links into the device chain. The
; INIT routine writes a marker to the Lotura unit-tester so the host can confirm
; the code ran on the CPU rather than inferring it.
;
; Assemble: nasm -f bin testdev.asm -o TESTDEV.SYS

        org 0

header:
        dd 0FFFFFFFFh           ; next driver pointer: end of chain
        dw 8000h               ; attributes: character device
        dw strategy             ; strategy entry offset
        dw interrupt            ; interrupt entry offset
        db 'TESTDEV '           ; 8-character device name

req_ptr dd 0                    ; ES:BX request pointer saved by strategy

; Strategy: DOS passes the request-header far pointer in ES:BX and expects it
; stored for the interrupt routine to use. Save it and return.
strategy:
        mov [cs:req_ptr], bx
        mov [cs:req_ptr+2], es
        retf

; Interrupt: the INIT command runs here. Mark the Lotura unit-tester, fill the
; request header's break address and DONE status, and return.
interrupt:
        push ax
        push bx
        push es
        ; Marker to the Lotura unit-tester: register index 0, data byte 0xD5. Only
        ; the index and data ports are touched, never the command port, so no CRC,
        ; snapshot, or exit fires.
        mov al, 0
        out 0E4h, al            ; select register index 0
        mov al, 0D5h
        out 0E5h, al            ; store the marker, post-increments the index
        ; Fill the request header: break address at the end of resident code, and
        ; status DONE with no error.
        les bx, [cs:req_ptr]
        mov word [es:bx+0Eh], resident_end  ; break offset
        mov [es:bx+10h], cs                 ; break segment
        mov word [es:bx+03h], 0100h         ; status: DONE, no error
        pop es
        pop bx
        pop ax
        retf

resident_end:
