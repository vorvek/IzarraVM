; probes/probe-pit.inc - STREAM C: 8254 timer (PIT) probe.
; Presence is proven through the control port (PORT_PIT_CTRL) and counter 0's
; data port (PORT_PIT_CH0) alone, so the check does not depend on the PIC or on
; IRQ0 being unmasked. We program counter 0 with a known control word, then issue
; a read-back command that latches the counter's status byte and read it back. A
; live 8254 echoes the mode and access bits we just wrote; a missing or stuck
; port does not.
;
; Note: the task suggested a counter-latch count readback (latch, read 0x40
; twice). Under the live machine clock the counter is decrementing while the
; handler runs, so the latched count is timing dependent and a fixed comparison
; would be fragile. The status read-back proves the same device with a value the
; model fixes deterministically, so we use it instead. See the stream risks.

POST_STEP "component.timer_pit", probe_pit

; Control word: counter 0, access LSB-then-MSB, mode 3, binary -> 0x36. The
; status byte the read-back returns must echo mode 3 (bits 3:1) and access mode
; LSB-then-MSB (bits 5:4).
%define PIT_PROBE_CW   0x36
; Read-back command: select status latch (bit 4 = 0), count latch off (bit 5 = 1),
; counter 0 (bit 1) -> 0xE2.
%define PIT_PROBE_RDBK 0xe2

probe_pit:
    mov al, PIT_PROBE_CW
    out PORT_PIT_CTRL, al         ; program counter 0
    mov al, PIT_PROBE_RDBK
    out PORT_PIT_CTRL, al         ; latch counter 0 status
    in al, PORT_PIT_CH0           ; read the latched status byte
    mov ah, al
    and ah, 0x0e                  ; isolate the mode field (bits 3:1)
    cmp ah, 0x06                  ; mode 3 -> bits 3:1 == 011b == 0x06
    jne .fail
    and al, 0x30                  ; isolate the access field (bits 5:4)
    cmp al, 0x30                  ; LSB-then-MSB -> 11b
    jne .fail
    call probe_pit_restore_timer
    mov al, HRES_PASS
    ret
.fail:
    call probe_pit_restore_timer
    mov al, HRES_FAIL
    ret

; The POST sequencer paces the visible icon reveal through channel 0 after every
; probe. Restore the standard 18.2 Hz mode before returning so this probe cannot
; strand the sequencer with a half-programmed counter.
probe_pit_restore_timer:
    mov al, 0x34                  ; counter 0, lobyte/hibyte, mode 2
    out PORT_PIT_CTRL, al
    xor al, al                    ; reload 0 -> 65536
    out PORT_PIT_CH0, al
    out PORT_PIT_CH0, al
    ret
