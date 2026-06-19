; probes/probe-lotura.inc - STREAM C: Lotura system-controller probe.
; Two POST steps share this file. The first reads the Lotura identity port and
; reports PASS/FAIL; the second reads the live GSW-586 mode register and reports
; it as a one-character MEASURE value. The emulator answers PORT_LOTURA_ID with
; LOTURA_ID_VALUE and PORT_LOTURA_MODE with the active mode code (0, 1, or 2).

POST_STEP "component.cpu_lotura", probe_lotura
POST_STEP "cpu.gsw_mode", probe_lotura_mode

; probe_lotura: read the identity port and compare against the known value.
probe_lotura:
    in al, PORT_LOTURA_ID
    cmp al, LOTURA_ID_VALUE
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret

; probe_lotura_mode: read the GSW-586 mode code and format it as a single ASCII
; digit at PROBE_SCRATCH so result_append can copy it. The model only ever
; returns 0, 1, or 2, so one digit plus a terminator is enough. The digit is
; stored through a base register (the foundation's [di] pattern); the direct
; moffs store form is avoided because the CPU core does not decode it.
probe_lotura_mode:
    in al, PORT_LOTURA_MODE
    add al, '0'                  ; mode code 0/1/2 -> '0'/'1'/'2'
    xor cx, cx
    mov ds, cx                   ; DS = 0 so PROBE_SCRATCH is reachable
    mov di, PROBE_SCRATCH
    mov [di], al                 ; the digit
    mov byte [di + 1], 0         ; ASCIIZ terminator
    mov si, PROBE_SCRATCH        ; SI = value offset for the MEASURE record
    mov al, HRES_MEASURE
    ret
