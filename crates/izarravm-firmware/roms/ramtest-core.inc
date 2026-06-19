; ramtest-core.inc - STREAM B slot (RAM test). Foundation stub: empty.
; Stream B owns this file and adds, in source order:
;   POST_STEP "memory.ramtest", ramtest_run        ; PASS/FAIL
;   POST_STEP "memory.detected_kib", ramtest_size   ; MEASURE, value in segment 0
; plus the ramtest_run / ramtest_size handlers. ramtest_run uses the foundation
; helpers ramtest_peek32/ramtest_poke32 for the >1 MiB pass and stashes the
; detected size in RAMTEST_SCRATCH for ramtest_size to format.
