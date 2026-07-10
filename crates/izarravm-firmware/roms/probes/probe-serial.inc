; probes/probe-serial.inc - STREAM C: COM1 (16550-class UART) probe.
; Wait for the timed transmitter to drain and check its idle pattern. Then
; round-trip the scratch register, which has no side effects.

POST_STEP "component.serial_com1", probe_serial

%define SERIAL_PROBE_PATTERN 0xa5  ; an arbitrary mixed-bit value for the round-trip

probe_serial:
    mov dx, PORT_COM1_LSR         ; ports above 0xFF must address through DX
.wait_idle:
    in al, dx
    test al, 0x40                 ; shift register still active
    jz .wait_idle
    cmp al, 0x60                  ; transmitter holding + shift register empty
    jne .fail
    mov dx, PORT_COM1_SCR
    mov al, SERIAL_PROBE_PATTERN
    out dx, al                    ; stash a known byte in the scratch register
    in al, dx                     ; and read it straight back
    cmp al, SERIAL_PROBE_PATTERN
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
