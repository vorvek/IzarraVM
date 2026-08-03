; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; LOADTEST.COM - the boot profiler's hard-drive load workload. Marks its own
; entry, reads the named file end to end in 4 KB chunks through INT 21h AH=3Fh,
; marks again, then stops the machine through the unit tester.
;
; Usage: LOADTEST <file>
;
; The entry mark matters as much as the read: reaching it means COMMAND.COM has
; finished parsing the command line and loading this image off Katea, so the
; window between the host's idle-end mark and EXEC_END is itself a measurement
; of "load a program off the hard drive".
;
; The target must be a file in the mounted HOST FOLDER. System-file overrides are
; served out of RAM, so reading one would never touch the host I/O path this
; workload exists to measure.
;
; Deliberately silent on success; see MARK.COM.
;
; Exit codes through CMD_EXIT: 0 read to EOF, 2 no filename, 3 open failed,
; 4 read failed.
;
; Build: nasm -f bin loadtest.asm -o loadtest.com
        cpu 8086
        org 0x100

REG_EXIT        equ 12
REG_MARK        equ 26
CMD_EXIT        equ 3
CMD_MARK        equ 4

MARK_EXEC_END   equ 3
MARK_LOAD_END   equ 4

CHUNK           equ 4096
; The read buffer lives above the code in the .COM's own 64 KB segment, well
; clear of both the program and the stack DOS parks at the segment top. Placing
; it by equate rather than reserving it keeps the committed .COM tiny.
buf             equ 0x2000

start:
        mov     al, MARK_EXEC_END
        call    mark

        ; Copy the first command-tail token into an ASCIIZ filename.
        mov     cl, [0x80]
        xor     ch, ch
        mov     si, 0x81
        mov     di, fname
.skip:
        jcxz    .no_name
        mov     al, [si]
        cmp     al, ' '
        je      .advance
        cmp     al, 9                   ; tab
        je      .advance
        jmp     .copy
.advance:
        inc     si
        dec     cx
        jmp     .skip
.copy:
        jcxz    .name_done
        mov     al, [si]
        inc     si
        dec     cx
        cmp     al, 13                  ; CR
        je      .name_done
        cmp     al, ' '
        je      .name_done
        cmp     al, 9
        je      .name_done
        stosb
        jmp     .copy
.name_done:
        xor     al, al
        stosb
        cmp     di, fname + 1           ; nothing but the terminator?
        jbe     .no_name

        mov     ax, 0x3d00              ; AH=3Dh open, AL=0 read-only
        mov     dx, fname
        int     0x21
        jc      .open_failed
        mov     bx, ax                  ; handle

.read_loop:
        mov     ah, 0x3f
        mov     cx, CHUNK
        mov     dx, buf
        int     0x21
        jc      .read_failed
        test    ax, ax
        jnz     .read_loop              ; 0 bytes = EOF

        mov     ah, 0x3e                ; close
        int     0x21

        mov     al, MARK_LOAD_END
        call    mark
        mov     al, 0
        jmp     exit_vm

.no_name:
        mov     dx, msg_no_name
        call    say
        mov     al, 2
        jmp     exit_vm

.open_failed:
        mov     dx, msg_open
        call    say
        mov     al, 3
        jmp     exit_vm

.read_failed:
        mov     ah, 0x3e                ; close before reporting
        int     0x21
        mov     dx, msg_read
        call    say
        mov     al, 4
        jmp     exit_vm

; say: print the '$'-terminated string at DX. Clobbers AX.
say:
        mov     ah, 0x09
        int     0x21
        ret

; mark: place phase boundary AL. Clobbers AX.
mark:
        mov     ah, al
        mov     al, REG_MARK
        out     0xE4, al                ; index = REG_MARK
        mov     al, ah
        out     0xE5, al                ; data = boundary id
        mov     al, CMD_MARK
        out     0xE6, al
        ret

; exit_vm: stop the machine with code AL. Does not return.
exit_vm:
        mov     ah, al
        mov     al, REG_EXIT
        out     0xE4, al
        mov     al, ah
        out     0xE5, al
        mov     al, CMD_EXIT
        out     0xE6, al
.hang:
        jmp     .hang

msg_no_name:    db 'LOADTEST: no filename', 13, 10, '$'
msg_open:       db 'LOADTEST: open failed', 13, 10, '$'
msg_read:       db 'LOADTEST: read failed', 13, 10, '$'
fname:          times 80 db 0
