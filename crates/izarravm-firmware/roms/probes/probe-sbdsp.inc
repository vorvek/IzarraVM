; probes/probe-sbdsp.inc - STREAM C: Sound Blaster DSP probes.
; Two POST steps: a PASS/FAIL reset handshake and a MEASURE of the DSP version.
; The reset routine writes 0x226 (1 then 0), lets the ~100us settle accrue
; through a busy-delay loop (advance_devices ticks the DSP per CPU clock), polls
; PORT_DSP_RSTATUS bit 7, then reads PORT_DSP_READ expecting the 0xAA ack. On a
; good reset it also issues the version command 0xE1 and stashes the two returned
; bytes in PROBE_SCRATCH so the version step can format them. Idioms reused from
; boot-suite/stage2.asm test_sb_dsp_reset.

; Sub-offsets inside the shared 256-byte PROBE_SCRATCH (0x0700). The upper half
; keeps these clear of the lower-numbered Stream C probes' working bytes.
%define SBDSP_VER_HI   PROBE_SCRATCH + 0x40   ; stashed major byte (from 0xE1)
%define SBDSP_VER_LO   PROBE_SCRATCH + 0x41   ; stashed minor byte
%define SBDSP_VER_STR  PROBE_SCRATCH + 0x42   ; formatted "major.minor", ASCIIZ

POST_STEP "component.audio_sbdsp", probe_sbdsp
POST_STEP "sound.dsp_version", probe_sbdsp_version

; probe_sbdsp: reset the DSP and confirm the 0xAA acknowledge. On success, read
; the version (command 0xE1 -> two bytes) and stash them for the MEASURE step.
; Returns AL = HRES_PASS / HRES_FAIL. The version bytes default to 0 so a failed
; reset still leaves a well-formed "0.0" for the measure step to format.
probe_sbdsp:
    push ds
    xor ax, ax
    mov ds, ax                  ; DS = 0 to seed the scratch defaults
    mov byte [SBDSP_VER_HI], 0
    mov byte [SBDSP_VER_LO], 0
    pop ds
    ; Reset: write 1 then 0 to PORT_DSP_RESET. The settle is time-based, not
    ; instant, so the poll below has to let emulated time pass between reads.
    mov dx, PORT_DSP_RESET
    mov al, 0x01
    out dx, al
    mov al, 0x00
    out dx, al
    mov cx, 8                   ; up to 8 delay windows for the ~100us settle
.wait:
    call probe_sbdsp_delay
    mov dx, PORT_DSP_RSTATUS
    in al, dx
    test al, 0x80              ; bit 7 = read-data available
    jnz .ready
    loop .wait
    mov al, HRES_FAIL          ; never became ready
    ret
.ready:
    mov dx, PORT_DSP_READ
    in al, dx
    cmp al, 0xAA               ; the reset acknowledge byte
    jne .fail
    ; Reset good. Ask for the version (command 0xE1 on the write-data port). The
    ; DSP queues the two bytes synchronously, so they are readable at once.
    mov dx, PORT_DSP_WSTATUS
    mov al, 0xE1
    out dx, al
    mov dx, PORT_DSP_READ
    in al, dx                  ; major
    mov bl, al
    in al, dx                  ; minor
    mov bh, al
    push ds
    xor cx, cx
    mov ds, cx
    mov [SBDSP_VER_HI], bl
    mov [SBDSP_VER_LO], bh
    pop ds
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret

; probe_sbdsp_version: format the stashed major.minor bytes as an ASCIIZ string
; in PROBE_SCRATCH and return it as the MEASURE value. Each byte is a small
; integer (the SB16 reports 4.5), rendered as a decimal run so a two-digit minor
; would still print. Returns AL = HRES_MEASURE, SI = offset of the value string.
probe_sbdsp_version:
    push ds
    xor ax, ax
    mov ds, ax
    mov di, SBDSP_VER_STR
    ; Read the stashed bytes through BX. The partial CPU core does not decode the
    ; byte accumulator-moffs form (mov al,[imm16], opcode 0xA0), so address the
    ; scratch with a register and the r/m8 encoding (0x8A) instead.
    mov bx, SBDSP_VER_HI
    mov al, [bx]
    call probe_sbdsp_putdec     ; major digits at DS:DI, advances DI
    mov byte [di], '.'
    inc di
    mov bx, SBDSP_VER_LO
    mov al, [bx]
    call probe_sbdsp_putdec     ; minor digits
    mov byte [di], 0            ; ASCIIZ terminator
    pop ds
    mov si, SBDSP_VER_STR
    mov al, HRES_MEASURE
    ret

; probe_sbdsp_putdec: write AL as decimal digits to DS:DI, advancing DI past the
; last digit. Values 0..255; no leading zeros (a bare 0 still prints "0"). Uses
; the stack to reverse the digit order. Preserves nothing of note beyond DI.
probe_sbdsp_putdec:
    push bx
    push cx
    push dx
    xor cx, cx                 ; digit count pushed
    mov bl, 10
.extract:
    xor ah, ah
    div bl                     ; AL = AL/10, AH = AL%10
    mov dl, ah
    add dl, '0'
    push dx
    inc cx
    test al, al
    jnz .extract
.emit:
    pop dx
    mov [di], dl
    inc di
    loop .emit
    pop dx
    pop cx
    pop bx
    ret

; probe_sbdsp_delay: busy-wait one settle window. Matches the stage2 delay: about
; 0x4000 loop iterations, comfortably over the ~100us the DSP reset needs at the
; 386 clock. Preserves CX for the caller's poll counter.
probe_sbdsp_delay:
    push cx
    mov cx, 0x4000
.spin:
    loop .spin
    pop cx
    ret
