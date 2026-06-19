; izbios-tables.inc - foundation: MUST be the last %include. Emits the POST step
; table that POST_STEP accumulated across every prior include, as immutable ROM
; data. run_post (in izbios-core.inc) walks this table. Each entry is
; dw name_ptr, dw handler_ptr; post_step_count is the entry count. The %[...]
; expansion substitutes the loop counter's value into the constructed names.
post_step_count equ POST_STEP_COUNT
post_step_table:
%assign POST_STEP_I 0
%rep POST_STEP_COUNT
    dw post_step_name_%[POST_STEP_I], POST_STEP_HANDLER_%[POST_STEP_I]
    %assign POST_STEP_I POST_STEP_I + 1
%endrep
