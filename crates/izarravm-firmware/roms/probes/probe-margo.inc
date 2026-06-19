; probes/probe-margo.inc - STREAM C slot. Foundation stub: empty.
; Stream C adds POST_STEP "component.video_margo", probe_margo (PASS/FAIL) and
; POST_STEP "video.margo_caps", probe_margo_caps (MEASURE). Uses read_margo_reg
; (0x00 -> 0x4D470100 id, 0x04 -> caps, expect 0xFFF) at MARGO_MMIO_BASE.
