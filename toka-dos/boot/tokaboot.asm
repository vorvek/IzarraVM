; tokaboot.asm - the Toka-DOS boot record.
;
; The BIOS .disk path asks the machine to place this 512-byte record at
; 0000:7C00 and jumps here with DL set to the boot drive, exactly like a real
; INT 19h floppy boot. Before we run, the machine has set up the DOS base
; environment (COMSPEC, PATH, PROMPT) and the HLE INT 21h kernel, so DOS
; services are live. We set text mode, print the startup line, and hand off to
; the shell through EXEC. This is the "tiny real boot stub that calls into the
; HLE kernel" the design settled on.

        org 0x7C00
        bits 16

start:
        cli
        xor ax, ax
        mov ds, ax
        mov es, ax
        mov ss, ax
        mov sp, 0x7C00
        cld
        sti

        ; 80x25 colour text (mode 03h), black background, light grey text.
        mov ax, 0x0003
        int 0x10

        ; Print the startup line through the DOS console (AH=09h), so it lands on
        ; the VGA text screen through the same path ICOMMAND's output uses.
        mov dx, msg
        mov ah, 0x09
        int 0x21

.launch:
        ; EXEC C:\DOS\ICOMMAND.COM. DS:DX -> program path, ES:BX -> parameter block.
        ; The environment segment 0 in the block means inherit the shell
        ; environment the machine prepared.
        mov ax, 0x4B00
        mov dx, path
        mov bx, epb
        int 0x21

        ; The shell owns the session. If it ever returns, stop here.
.halt:
        hlt
        jmp .halt

msg:    db "Starting Toka-DOS v3.0...", 13, 10, "$"
path:   db "C:\DOS\ICOMMAND.COM", 0

        align 2
epb:
        dw 0            ; environment segment (0 = inherit)
        dw tail         ; command-tail offset
        dw 0            ; command-tail segment (segment 0)
        dw fcb          ; FCB1 offset
        dw 0            ; FCB1 segment
        dw fcb          ; FCB2 offset
        dw 0            ; FCB2 segment

tail:   db 0, 13        ; empty command tail: length 0, CR terminator
fcb:    times 16 db 0   ; one blank FCB, shared by both slots

        times 510-($-$$) db 0
        dw 0xAA55       ; boot signature, for form; the BIOS does not check it
