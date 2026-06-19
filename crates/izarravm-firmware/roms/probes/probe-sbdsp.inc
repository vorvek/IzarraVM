; probes/probe-sbdsp.inc - STREAM C slot. Foundation stub: empty.
; Stream C adds POST_STEP "component.audio_sbdsp", probe_sbdsp (PASS/FAIL) and
; POST_STEP "sound.dsp_version", probe_sbdsp_version (MEASURE). The reset
; handshake settles via a delay loop (reuse the stage2 8-iteration poll idiom).
