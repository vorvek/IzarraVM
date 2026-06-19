; probes/probe-opl.inc - STREAM C: OPL (YMF262 / AdLib) presence probe.
; One POST step, PASS/FAIL. Reset the timer overflow flags by writing 0x80 to
; timer-control register 0x04 (PORT_OPL_ADDR selects, PORT_OPL_DATA writes), then
; read the status byte back on PORT_OPL_ADDR. With both timers idle and their
; flags just cleared, the defined bits 7/6/5 read 0, which is the OPL3 rest-state
; signature. Idiom reused from boot-suite/stage2.asm test_opl3.

POST_STEP "component.audio_opl", probe_opl

probe_opl:
    mov dx, PORT_OPL_ADDR
    mov al, 0x04               ; timer-control register
    out dx, al
    mov dx, PORT_OPL_DATA
    mov al, 0x80               ; reset both timer overflow flags
    out dx, al
    mov dx, PORT_OPL_ADDR      ; status reads back on the address port
    in al, dx
    and al, 0xE0               ; only IRQ/timer bits 7/6/5 are defined
    jnz .fail                  ; nonzero -> not the OPL rest signature
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
