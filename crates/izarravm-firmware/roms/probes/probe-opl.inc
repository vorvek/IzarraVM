; probes/probe-opl.inc - STREAM C slot. Foundation stub: empty.
; Stream C adds POST_STEP "component.audio_opl", probe_opl and the handler
; (write timer reg 0x04 <- 0x80, expect status & 0xE0 == 0 via PORT_OPL_ADDR).
