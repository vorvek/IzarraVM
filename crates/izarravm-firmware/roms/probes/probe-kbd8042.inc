; probes/probe-kbd8042.inc - STREAM C: 8042 keyboard-controller probe.
; The controller self-test: write 0xAA to the command port, then read the data
; port; a working 8042 answers 0x55. The emulator queues 0x55 into the output
; buffer on the 0xAA command, which also sets the output-buffer-full status and,
; with IRQ1 unmasked, arms an IRQ1 edge. We mask interrupts across the self-test
; so the installed INT 09h cannot drain 0x60 before we read it; the edge stays
; pending and is serviced (against an empty buffer) after we re-enable.

POST_STEP "component.kbd_8042", probe_kbd8042

probe_kbd8042:
    pushf                        ; remember the caller's interrupt flag
    cli                          ; keep INT 09h off 0x60 until we have read it
    mov al, 0xaa
    out PORT_KBD_CMD, al         ; controller self-test command
    in al, PORT_KBD_DATA         ; expect the self-test result byte
    popf                         ; restore IF (re-enables IRQ1 if it was on)
    cmp al, 0x55
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
