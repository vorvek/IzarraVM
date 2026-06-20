; izbios-chime.inc - STREAM E: the power-on PC-speaker chime. play_chime plays a
; four-note rising major arpeggio (about C5, E5, G5, C6) through PIT channel 2 and
; port 0x61 at the very start of POST, then silences the speaker. The note delay
; is cosmetic, so it runs through the same fast-POST gate the RAM count-up uses
; (port PORT_FAST_POST reads 1): under fast POST each note is still programmed and
; the speaker is still enabled, but the per-note delay is skipped so headless runs
; and tests stay inside their cycle budgets.
;
; PIT channel-2 divisors (1193182 Hz / frequency):
;   C5  523.25 Hz -> 2280   E5  659.25 Hz -> 1810
;   G5  783.99 Hz -> 1522   C6 1046.50 Hz -> 1140

%define CHIME_DIV_C5   2280
%define CHIME_DIV_E5   1810
%define CHIME_DIV_G5   1522
%define CHIME_DIV_C6   1140

; play_chime: program PIT channel 2 for each of the four notes, enable the speaker,
; hold the note for about 120 ms (skipped under fast POST), then move on. Silence
; the speaker at the end. Preserves the registers POST relies on.
play_chime:
    pusha

    ; Read the fast-POST flag once into BP: 1 = skip the cosmetic delays.
    xor bp, bp
    in al, PORT_FAST_POST
    test al, 1
    jz .have_flag
    mov bp, 1
.have_flag:

    ; Under the full pacing, program PIT channel 0 (mode 2, reload 0 = 65536) so
    ; chime_delay has a ~18.2 Hz real-time tick to poll. The RAM count-up later
    ; reprograms channel 0 the same way, so this is its own setup.
    test bp, bp
    jnz .notes
    mov al, 0x34
    out PORT_PIT_CTRL, al
    xor al, al
    out PORT_PIT_CH0, al
    out PORT_PIT_CH0, al

.notes:
    mov dx, CHIME_DIV_C5
    call chime_note
    mov dx, CHIME_DIV_E5
    call chime_note
    mov dx, CHIME_DIV_G5
    call chime_note
    mov dx, CHIME_DIV_C6
    call chime_note

    ; Silence: clear the speaker data enable (and GATE2) on port 0x61.
    in al, PORT_SPEAKER
    and al, 0xfc
    out PORT_SPEAKER, al

    popa
    ret

; chime_note: program PIT channel 2 to mode 3 with the divisor in DX, enable the
; speaker, hold for about 120 ms (unless fast POST is set in BP). DX is consumed.
chime_note:
    push ax
    ; PIT channel 2, lo/hi byte, mode 3 (square wave).
    mov al, 0xb6
    out PORT_PIT_CTRL, al
    mov al, dl
    out PORT_PIT_CH2, al            ; divisor low
    mov al, dh
    out PORT_PIT_CH2, al            ; divisor high
    ; Enable the speaker: GATE2 (bit 0) + data enable (bit 1) on port 0x61.
    in al, PORT_SPEAKER
    or al, 0x03
    out PORT_SPEAKER, al
    ; Hold the note. Fast POST skips the delay; the speaker stays enabled for the
    ; next note's reprogram, which is enough for the audio path to register it.
    test bp, bp
    jnz .done
    call chime_delay
.done:
    pop ax
    ret

; chime_delay: block for about 120 ms by polling two PIT channel-0 reloads
; (~54.9 ms each). Self-contained so the chime does not depend on the RAM test
; having run. Preserves all registers.
chime_delay:
    push cx
    mov cx, 2
.tick:
    call chime_pit_wait
    loop .tick
    pop cx
    ret

; chime_pit_wait: block until PIT channel 0 reloads once. The counter falls; a
; wrap shows when a fresh read reads higher than the previous one. The busy poll
; also advances emulated time so the counter actually moves. Preserves registers.
chime_pit_wait:
    push ax
    push bx
    push cx
    call chime_pit_read             ; BX = initial count
    mov cx, bx
.spin:
    call chime_pit_read             ; BX = current count
    cmp bx, cx                      ; counter wrapped (count rose) -> done
    ja .done
    mov cx, bx                      ; track the falling count
    jmp .spin
.done:
    pop cx
    pop bx
    pop ax
    ret

; chime_pit_read: latch and read PIT counter 0 -> BX = 16-bit count. Preserves AX.
chime_pit_read:
    push ax
    xor al, al                      ; latch counter 0
    out PORT_PIT_CTRL, al
    in al, PORT_PIT_CH0             ; LSB
    mov bl, al
    in al, PORT_PIT_CH0             ; MSB
    mov bh, al
    pop ax
    ret
