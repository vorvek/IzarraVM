; probes/probe-serial.inc - STREAM C: COM1 (16550-class UART) probe.
; Two cheap, non-destructive checks prove COM1 answers: the line status register
; reads its idle pattern (transmitter empty), and the scratch register round-trips
; an arbitrary byte. The emulator pins PORT_COM1_LSR to 0x60 (THR empty + line
; idle) and stores the scratch register verbatim, so both checks are exact. The
; scratch register has no side effects, so the probe leaves the port untouched
; for later use.

POST_STEP "component.serial_com1", probe_serial

%define SERIAL_PROBE_PATTERN 0xa5  ; an arbitrary mixed-bit value for the round-trip

probe_serial:
    mov dx, PORT_COM1_LSR         ; ports above 0xFF must address through DX
    in al, dx
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
