; probes/probe-kbd8042.inc - STREAM C slot. Foundation stub: empty.
; Stream C adds POST_STEP "component.kbd_8042", probe_kbd8042 and the handler
; (0x64 <- 0xAA self-test, expect 0x55 from 0x60).
