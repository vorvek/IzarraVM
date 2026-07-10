; probes/probe-kbd8042.inc - STREAM C: 8042 keyboard-controller probe.
; The controller self-test: wait for IBF to clear, write 0xAA to the command
; port, then wait for OBF and read 0x55 from the data port. Interrupts stay
; masked across the handshake so INT 09h cannot drain the response first.

POST_STEP "component.kbd_8042", probe_kbd8042

probe_kbd8042:
    pushf                        ; remember the caller's interrupt flag
    push cx
    cli                          ; keep INT 09h off 0x60 until we have read it
    mov cx, 0xffff
.wait_input:
    in al, PORT_KBD_CMD
    test al, KBC_STATUS_IBF
    jz .send
    loop .wait_input
    jmp .timeout
.send:
    mov al, 0xaa
    out PORT_KBD_CMD, al         ; controller self-test command
    mov cx, 0xffff
.wait_output:
    in al, PORT_KBD_CMD
    test al, KBC_STATUS_OBF
    jnz .ready
    loop .wait_output
.timeout:
    pop cx
    popf
    mov al, HRES_FAIL
    ret
.ready:
    in al, PORT_KBD_DATA         ; self-test result byte
    pop cx
    popf                         ; restore IF (re-enables IRQ1 if it was on)
    cmp al, 0x55
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
