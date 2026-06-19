; setup-ui.inc - STREAM D slot (setup page). Foundation stub: setup_maybe_enter
; returns immediately so the BIOS boots straight through. Stream D replaces this
; file with the mode-13h setup menu (hotkey SETUP_HOTKEY, live Lotura 0xE1 write
; on Save). Called by core bring-up after run_post, before enter_setup_or_boot.
setup_maybe_enter:
    ret
