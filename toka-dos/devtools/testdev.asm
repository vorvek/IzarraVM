; TESTDEV.SYS - clean-room character device driver that proves real .SYS loading
; and character-device I/O routing. A DEVICE= line in CONFIG.SYS loads it resident,
; SYSINIT runs its strategy then interrupt INIT on the real CPU, and its header
; links into the device chain. After that, a guest that opens TESTDEV by name can
; write bytes to it and read them back: WRITE stores into a small resident buffer,
; READ returns what was stored. Each entry writes a distinct marker to the Lotura
; unit-tester so the host can confirm which entry ran on the CPU.
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
buflen  dw 0                    ; bytes currently stored in buf

bufmax  equ 16                  ; resident transfer buffer size in bytes

; Strategy: DOS passes the request-header far pointer in ES:BX and expects it
; stored for the interrupt routine to use. Save it and return.
strategy:
        mov [cs:req_ptr], bx
        mov [cs:req_ptr+2], es
        retf

; Interrupt: dispatch on the request command byte at [es:bx+2].
;   0  INIT   mark the unit-tester, fill the break address and DONE status.
;   4  READ   copy min(count, buflen) bytes from buf to the transfer address.
;   8  WRITE  copy min(count, bufmax) bytes from the transfer address into buf.
;   else      DONE, no action.
interrupt:
        push ax
        push bx
        push cx
        push si
        push di
        push ds
        push es
        les bx, [cs:req_ptr]
        mov al, [es:bx+02h]     ; command byte
        cmp al, 0
        je do_init
        cmp al, 4
        je do_read
        cmp al, 8
        je do_write
        jmp done                ; unknown command: DONE, no action

; --- INIT (command 0) -------------------------------------------------------
; Marker to the Lotura unit-tester: register index 0, data byte 0xD5. Only the
; index and data ports are touched, never the command port, so no CRC, snapshot,
; or exit fires. Fill the request header break address and DONE status.
do_init:
        mov al, 0
        out 0E4h, al            ; select register index 0
        mov al, 0D5h
        out 0E5h, al            ; store the marker, post-increments the index
        mov word [es:bx+0Eh], resident_end  ; break offset
        mov [es:bx+10h], cs                 ; break segment
        jmp done

; --- READ (command 4) -------------------------------------------------------
; Copy min(requested count, buflen) bytes from buf to the transfer address, set
; the request count to the bytes returned, and mark unit-tester index 1 = 0x52.
do_read:
        mov al, 1
        out 0E4h, al            ; select register index 1
        mov al, 52h             ; 'R': READ ran on the CPU
        out 0E5h, al
        mov cx, [es:bx+12h]     ; requested count
        cmp cx, [cs:buflen]
        jbe read_count_ok
        mov cx, [cs:buflen]     ; cap at the bytes stored
read_count_ok:
        mov [es:bx+12h], cx     ; report the count returned
        ; source DS:SI = cs:buf, dest ES:DI = transfer address
        mov di, [es:bx+0Eh]     ; transfer offset
        mov ax, [es:bx+10h]     ; transfer segment
        push cs
        pop ds
        mov si, buf
        mov es, ax
        rep movsb
        jmp done

; --- WRITE (command 8) ------------------------------------------------------
; Copy min(requested count, bufmax) bytes from the transfer address into buf,
; record buflen and the request count, and mark unit-tester index 2 = 0x57.
do_write:
        mov al, 2
        out 0E4h, al            ; select register index 2
        mov al, 57h             ; 'W': WRITE ran on the CPU
        out 0E5h, al
        mov cx, [es:bx+12h]     ; requested count
        cmp cx, bufmax
        jbe write_count_ok
        mov cx, bufmax          ; cap at the buffer size
write_count_ok:
        mov [cs:buflen], cx     ; remember how much we stored
        mov [es:bx+12h], cx     ; report the count taken
        ; source DS:SI = transfer address, dest ES:DI = cs:buf
        mov si, [es:bx+0Eh]     ; transfer offset
        mov ax, [es:bx+10h]     ; transfer segment
        mov ds, ax
        push cs
        pop es
        mov di, buf
        rep movsb
        jmp done

; --- common exit ------------------------------------------------------------
; Re-point ES to the request (READ/WRITE moved it), set DONE status, return.
done:
        les bx, [cs:req_ptr]
        mov word [es:bx+03h], 0100h   ; status: DONE, no error
        pop es
        pop ds
        pop di
        pop si
        pop cx
        pop bx
        pop ax
        retf

buf     times bufmax db 0

resident_end:
