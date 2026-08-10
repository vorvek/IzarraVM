; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; SNDMIXER.COM - ReSonique 2 Volume Mixer.
;
; Six vertical faders over the card's mixer: MASTER, FMSYNTH, WAVE, CD-ROM,
; MIDI and PC-SPEAKER. Sibling to SNDCTRL.COM, which assigns the card's
; resources; this one sets its levels. Same box, same palette, same key map.
;
; There is deliberately no line or microphone fader: the machine models no
; recording path, so those registers control nothing and a fader over them
; would be a lie about what moving it does.
;
; Usage:
;   SNDMIXER                     full-screen mixer
;   SNDMIXER /L                  print the current levels and exit
;   SNDMIXER /CFG file           load levels from a file and apply them
;   SNDMIXER /M 8 /F 6           set channels from the command line and exit
;   SNDMIXER /M 8 /CFG file      set channels, apply, and save them to the file
;   SNDMIXER /S                  silent: no output at all (for AUTOEXEC)
;   SNDMIXER /?                  usage
;
; /CFG alone RESTORES (read the file, write the hardware). /CFG together with
; any channel switch SAVES (write the hardware, then write the file). That one
; rule is what lets the boot line and the "remember this" line be the same
; switch. The full-screen F10 always saves to C:\VOLCONF.CFG, the file the boot
; line reads, so an interactive change survives the next boot without the user
; having to name a file. The default sits in the ROOT, not in C:\DOS: a
; host-folder-mounted C: is the GUI's default and need not contain a DOS
; directory at all, and a save into a directory that is not there fails. The
; root of a mounted drive always exists.
;
; =============================================================================
; THE FADER LAW
; =============================================================================
; The CT1745's 5-bit level registers are 2 dB per step over 0..31, i.e. a
; 62 dB range. A fader that spread ten UI steps evenly over the LEVEL numbers
; would be useless: levels 31 down to 25 are the top 12 dB and would take up
; seven of the ten steps' worth of travel, while the bottom of the scale, where
; every remaining dB lives, would be crammed into the last one. The steps have
; to be even in dB, not in level.
;
; So: 4 dB per UI step, which is exactly TWO hardware steps, so every stop
; lands on a real register value with nothing rounded away.
;
;     step  level  register  attenuation
;       10     31      0xF8     0 dB     <- the card's power-on level
;        9     29      0xE8    -4 dB
;        8     27      0xD8    -8 dB
;        7     25      0xC8   -12 dB
;        6     23      0xB8   -16 dB
;        5     21      0xA8   -20 dB
;        4     19      0x98   -24 dB
;        3     17      0x88   -28 dB
;        2     15      0x78   -32 dB
;        1     13      0x68   -36 dB
;        0      0      0x00    mute
;
; 4 dB is a step you can hear (a halving of amplitude is 6 dB, so each stop is
; a clearly different loudness), and -36 dB at step 1 is quiet-but-present
; rather than the -62 dB floor, which would waste three or four stops on levels
; nobody can tell apart from silence. Step 0 is the register's own hard mute,
; not the bottom of the ladder.
;
; Step 10 is the level the card powers on at, and the top of the fader, on
; purpose: the mix reserves its headroom AFTER the mixer (one scalar on the
; summing node, so a single full-scale source cannot clip), which is what makes
; it safe for every leg to sit at 0 dB. The faders own the range below that.
; Reading a register back rounds to the nearest stop, so a level some game left
; behind -- 24, say, the Guide's own -14 dB default -- shows as the step it is
; closest to (7) instead of refusing to display.
;
; WAVE drives two devices. The CT1745 voice pair (0x32/0x33) is the SB16 DSP's
; level; the AD1848 output attenuators (I6/I7) are the WSS codec's. They are
; the same thing to a listener -- digital audio playback -- and no title uses
; both at once, so one fader moves both and the CT1745 side is what the fader
; READS back, because the codec powers on muted and would otherwise drag the
; display to 0 the moment a machine booted. The codec's attenuators are 1.5 dB
; per step, so each UI stop takes the nearest count to the same dB figure; see
; wss_atten.
;
; PC-SPEAKER is TWO BITS on the CT1745 (register 0x3B), not five: four stops,
; not eleven. Rather than pretend otherwise by offering ten positions that
; produce four distinct loudnesses -- the exact failure this fader law exists
; to avoid -- the speaker fader stops at 0, 3, 7 and 10, which is where its
; four hardware positions actually sit. A value from the command line snaps up
; to the nearest stop at or above it (any non-zero request stays audible).
;
; MIDI has no CT1745 register at all: on a real SB16 the pair at 0x34/0x35
; called "MIDI" is the FM synthesiser bus, which is this card's FMSYNTH fader.
; The wavetable MPU's synthesis is mixed on-card the way an AWE32 mixes its
; EMU8000, and the ReSonique 2 gives it a register pair of its own at
; 0x50/0x51, on the card's own register file and at the card's own 5-bit level
; scale. That is a ReSonique 2 extension, not a CT1745 register.
;
; That pair alone has a mute BIT, D0, and this fader is the reason. The
; wavetable is the one source in the machine with no second control anywhere --
; no GUI slider, no other register, no game setup screen reaches it -- so the
; card refuses to read a level of 0 there as silence and floors it at the
; quietest audible step instead; a guest clearing a block of mixer registers
; would otherwise silence the machine's MIDI for the session with nothing on
; screen to say so. Step 0 on this fader therefore writes D0, which IS silence,
; and the fader keeps a real mute without the register having to guess.
;
; Build: nasm -f bin sndmixer.asm -o sndmixer.com
    cpu 386
    org 0x100

; ---- hardware ---------------------------------------------------------------
SB_BASE        equ 0x220
SB_RESET       equ SB_BASE + 6         ; 0x226
SB_READ_DATA   equ SB_BASE + 0x0A      ; 0x22A
SB_READ_STATUS equ SB_BASE + 0x0E      ; 0x22E
SB_MIXER_IDX   equ SB_BASE + 4         ; 0x224
SB_MIXER_DAT   equ SB_BASE + 5         ; 0x225

WSS_BASE       equ 0x530
WSS_ID         equ WSS_BASE            ; board/version ID; 0xFF means absent
WSS_INDEX      equ WSS_BASE + 4        ; R0 index address
WSS_DATA       equ WSS_BASE + 5        ; R1 indexed data
WSS_LEFT_DAC   equ 6                   ; I6 left output attenuation
WSS_RIGHT_DAC  equ 7                   ; I7 right
WSS_DAC_MUTE   equ 0x80

; ---- Izarra 3000 palette ----------------------------------------------------
; Identical to SNDCTRL.COM's, deliberately: the two tools are one setup screen
; split in half and must not look like two programs.
A_BOX     equ 0xF0       ; black on bright white: body, borders, fixed values
A_TITLE   equ 0xF4       ; red on bright white: branding and section titles
A_FIELD   equ 0x0F       ; white on black: an editable value, drawn as an input
A_SEL     equ 0x4F       ; white on red: the selected input
A_SHADOW  equ 0x80       ; dark grey block under and beside the box

BOX_ROW   equ 1
BOX_COL   equ 4
BOX_W     equ 72
BOX_H     equ 21

TRACK_TOP equ 5          ; the track's top border row
TRACK_BOT equ 16         ; its bottom border row; the ten cells are 6..15
VALUE_ROW equ 17
NAME_ROW  equ 18
INFO_ROW  equ 19

; ---- channel records (16 bytes each) ----------------------------------------
CH_KIND    equ 0
CH_REG     equ 1          ; left mixer register (right is +1), or 0x3B
CH_COL     equ 2          ; track column (5 cells wide)
CH_NAMECOL equ 3
CH_STEP    equ 4          ; current step, 0..10
CH_SAVED   equ 5          ; the step this run opened on, for Esc
CH_DEV     equ 6          ; DEV_SB or DEV_ANY
                          ; 7 is spare; the command-line letters live in
                          ; sw_table, which is what parse_tail searches
CH_NAME    equ 8          ; word -> ASCIIZ label
CH_KEY     equ 10         ; word -> ASCIIZ config-file keyword
CH_DESC    equ 12         ; word -> ASCIIZ one-line description
CH_SIZE    equ 16

K_CT5 equ 0               ; a CT1745 5-bit pair
K_WAV equ 1               ; the CT1745 voice pair AND the AD1848 attenuators
K_SPK equ 2               ; the CT1745 2-bit PC-speaker register
K_MID equ 3               ; the ReSonique 2 wavetable pair, which has a mute bit

; D0 of 0x50/0x51. The wavetable leg is the only source in the machine with no
; other control anywhere, so a level of 0 there is floored to the quietest
; audible step rather than taken as silence -- a guest clearing a block of mixer
; registers would otherwise silence the machine's MIDI for the session with
; nothing to show for it. This bit is how silence is asked for on purpose, and
; it is what step 0 on the MIDI fader writes.
WT_MUTE equ 0x01

DEV_SB  equ 0             ; needs the card
DEV_ANY equ 1             ; the card or the codec will do

C_MASTER  equ 0
C_FM      equ 1
C_WAVE    equ 2
C_CD      equ 3
C_MIDI    equ 4
C_SPK     equ 5
C_COUNT   equ 6

TOKEN_MAX equ 16
PATH_MAX  equ 80
CFG_MAX   equ 1024

%define CSTEP(i) (channels + (i) * CH_SIZE + CH_STEP)

; =============================================================================
start:
    cld
    call probe_hardware
    ; /S is read out of the whole tail BEFORE anything can print, so it silences
    ; the run wherever the user put it. Reading it in order instead made
    ; `SNDMIXER /M 99 /S` report the error it was told not to report.
    call prescan_silent
    call parse_tail
    jc .usage_error
    cmp byte [want_usage], 0
    jne .usage
    ; A restore is "/CFG with nothing else to say": read the file and apply it.
    ; With channel switches present the direction reverses, and the file is
    ; where the run's result is written rather than where it came from.
    cmp byte [want_apply], 0
    jne .cli_apply
    cmp byte [have_cfg], 0
    jne .restore
    cmp byte [want_list], 0
    jne .list
    jmp interactive

.usage_error:
    cmp byte [want_silent], 0
    jne .quiet_error
    cmp byte [bad_kind], 0
    jne .value_error
    mov si, msg_bad_switch
    call print
    mov si, token
    call print
    mov si, msg_crlf
    call print
.usage:
    cmp byte [want_silent], 0
    jne .quiet_error
    mov si, msg_usage
    call print
    mov ax, 0x4c01
    int 0x21
.value_error:
    mov si, msg_bad_value
    call print
    movzx si, byte [cur_arg]
    shl si, 4
    add si, channels
    mov si, [si + CH_NAME]
    call print
    mov si, msg_crlf
    call print
.quiet_error:
    mov ax, 0x4c01
    int 0x21

.list:
    call read_levels
    call report
    mov ax, 0x4c00
    int 0x21

; The boot path. A missing or unreadable file is not an error: nothing has been
; saved yet, so there is nothing to restore, and the card keeps its power-on
; levels. Saying so would put a line in an AUTOEXEC that /S exists to keep
; clean, so it is said only when the tool was not asked to be silent.
.restore:
    call read_levels            ; a channel the file omits keeps the card's level
    call cfg_load
    jc .no_cfg
    call apply_all
    call report_applied
    mov ax, 0x4c00
    int 0x21
.no_cfg:
    call report_no_cfg
    mov ax, 0x4c00
    int 0x21

; A command line that sets anything applies it and exits without drawing.
.cli_apply:
    call read_levels            ; unnamed channels keep what the card holds
    call merge_pending
    call apply_all
    cmp byte [have_cfg], 0
    je .cli_done
    call cfg_save
.cli_done:
    call report
    mov ax, 0x4c00
    int 0x21

; =============================================================================
; Hardware probe.
; =============================================================================
probe_hardware:
    call probe_sb
    call probe_wss
    ret

; The documented Sound Blaster detection, byte for byte as SNDCTRL.COM does it:
; pulse the reset port, then wait for the DSP to raise data-available and hand
; back 0xAA. The wait is bounded by the BIOS tick counter rather than an
; iteration count, because how many polls fit into the DSP's ~100us settle
; depends entirely on the CPU persona.
probe_sb:
    mov dx, SB_RESET
    mov al, 1
    out dx, al
    mov cx, 200
.settle:
    in al, dx
    loop .settle
    xor al, al
    out dx, al
    push es
    xor ax, ax
    mov es, ax
    mov bx, [es:0x46C]          ; BIOS timer tick, low word
    xor cx, cx                  ; 65536 polls
.wait:
    mov dx, SB_READ_STATUS
    in al, dx
    test al, 0x80
    jnz .ready
    mov ax, [es:0x46C]
    sub ax, bx                  ; unsigned difference, so midnight wrap is fine
    cmp ax, 3
    jae .absent
    dec cx
    jnz .wait
    jmp .absent
.ready:
    mov dx, SB_READ_DATA
    in al, dx
    cmp al, 0xAA
    jne .absent
    mov byte [sb_present], 1
.absent:
    pop es
    ret

; The codec's board/version ID. An absent card leaves the port on open bus.
probe_wss:
    mov dx, WSS_ID
    in al, dx
    cmp al, 0xFF
    je .absent
    mov byte [wss_present], 1
.absent:
    ret

; =============================================================================
; Levels: reading them off the hardware and writing them back.
; =============================================================================

; AL = index -> AL = value.
mixer_read:
    push dx
    mov dx, SB_MIXER_IDX
    out dx, al
    jmp short $+2
    inc dx
    in al, dx
    pop dx
    ret

; AH = register, AL = value.
mixer_write:
    push dx
    push ax
    mov dx, SB_MIXER_IDX
    mov al, ah
    out dx, al
    jmp short $+2
    pop ax
    inc dx
    out dx, al
    pop dx
    ret

; AH = AD1848 register index, AL = value. The index port also carries INIT,
; MCE and TRD, so the other four bits are read back and put in front of the
; index rather than assumed clear -- clearing MCE behind a driver's back would
; start an autocalibrate it did not ask for.
wss_write:
    push dx
    push ax
    mov dx, WSS_INDEX
    in al, dx
    and al, 0xF0
    or al, ah
    out dx, al
    jmp short $+2
    pop ax
    mov dx, WSS_DATA
    out dx, al
    pop dx
    ret

; Fill every channel's CH_STEP from the hardware, and keep a copy in CH_SAVED
; so Esc can put back exactly what was there.
read_levels:
    xor bx, bx
.loop:
    cmp bl, C_COUNT
    jae .done
    movzx si, bl
    shl si, 4
    add si, channels
    push bx
    call read_channel           ; AL = step
    pop bx
    mov [si + CH_STEP], al
    mov [si + CH_SAVED], al
    inc bl
    jmp .loop
.done:
    ret

; SI -> channel record. AL = the step the hardware is nearest to. A channel
; whose device is absent reads 0 and is never drawn as an input.
read_channel:
    call channel_enabled
    jnc .absent
    cmp byte [si + CH_KIND], K_SPK
    je .speaker
    cmp byte [si + CH_KIND], K_MID
    je .wavetable
    mov al, [si + CH_REG]
    call mixer_read
    shr al, 3                   ; the level lives in D7-D3
    jmp level_to_step
; The wavetable pair reads back its mute bit as well as its level, and a level
; of 0 there is NOT silence: the register floors it at the quietest step, so
; that is the step to show.
.wavetable:
    mov al, [si + CH_REG]
    call mixer_read
    test al, WT_MUTE
    jnz .muted
    shr al, 3
    test al, al
    jz .floored
    jmp level_to_step
.floored:
    mov al, 1
    ret
.muted:
    xor al, al
    ret
.speaker:
    mov al, [si + CH_REG]
    call mixer_read
    shr al, 6                   ; two bits, D7-D6
    movzx bx, al
    mov al, [spk_step + bx]
    ret
.absent:
    xor al, al
    ret

; AL = 5-bit level -> AL = the nearest fader step.
;
; The stops are levels 13,15,...,31, so `(level - 10) / 2` lands on the nearest
; one and never needs a search: level 24 (the Guide's -14 dB power-on) gives 7,
; level 31 gives 10. Everything at or below level 11 clamps to step 1 rather
; than to 0, because 0 is the register's hard mute and a card that is merely
; very quiet is not muted.
level_to_step:
    test al, al
    jz .mute
    cmp al, 12
    jb .floor
    sub al, 10
    shr al, 1
    cmp al, 10
    jbe .done
    mov al, 10
.done:
    ret
.floor:
    mov al, 1
    ret
.mute:
    xor al, al
    ret

; Write every channel's step to the hardware it owns.
apply_all:
    xor bx, bx
.loop:
    cmp bl, C_COUNT
    jae .done
    movzx si, bl
    shl si, 4
    add si, channels
    push bx
    call apply_channel
    pop bx
    inc bl
    jmp .loop
.done:
    mov byte [applied], 1
    ret

; SI -> channel record. Writes the step in CH_STEP to the device(s) behind it.
apply_channel:
    call channel_enabled
    jnc .done
    movzx di, byte [si + CH_STEP]
    cmp byte [si + CH_KIND], K_SPK
    je .speaker
    ; The 5-bit pair: one byte, both channels. The mixer is mono per channel
    ; here on purpose -- a balance control is a separate idea from a level, and
    ; this tool does not offer one, so the two registers always agree.
    mov al, [step_level + di]
    shl al, 3
    cmp byte [si + CH_KIND], K_MID
    jne .level_ready
    test al, al                 ; step 0 on the wavetable is the mute BIT, not
    jnz .level_ready            ; level 0, which that register floors
    mov al, WT_MUTE
.level_ready:
    mov bh, al
    mov ah, [si + CH_REG]
    mov al, bh
    call mixer_write
    mov ah, [si + CH_REG]
    inc ah
    mov al, bh
    call mixer_write
    cmp byte [si + CH_KIND], K_WAV
    jne .done
    cmp byte [wss_present], 0
    je .done
    mov al, [wss_atten + di]
    mov bh, al
    mov ah, WSS_LEFT_DAC
    mov al, bh
    call wss_write
    mov ah, WSS_RIGHT_DAC
    mov al, bh
    call wss_write
.done:
    ret
.speaker:
    mov al, [spk_level + di]
    shl al, 6
    mov ah, [si + CH_REG]
    call mixer_write
    ret

; SI -> channel record. CF=1 when the device behind it answered the probe.
channel_enabled:
    cmp byte [si + CH_DEV], DEV_ANY
    je .any
    cmp byte [sb_present], 0
    je .no
    stc
    ret
.any:
    cmp byte [sb_present], 0
    jne .yes
    cmp byte [wss_present], 0
    je .no
.yes:
    stc
    ret
.no:
    clc
    ret

; BL = channel index. CF=1 when it is editable. Preserves SI.
index_enabled:
    push si
    movzx si, bl
    shl si, 4
    add si, channels
    call channel_enabled
    pop si
    ret

; Clamp AL into 0..10 and, for the PC speaker, snap it to the nearest stop at
; or ABOVE it. Snapping up is what keeps `/P 1` audible: a request for a little
; is a request for some, and the nearest stop downwards from 1 is silence.
; SI -> channel record. Clobbers BX.
normalize_step:
    cmp al, 10
    jbe .in_range
    mov al, 10
.in_range:
    cmp byte [si + CH_KIND], K_SPK
    jne .done
    movzx bx, al
    mov al, [spk_snap + bx]
.done:
    ret

; Walk the tail looking only for /S, before a single byte has been printed.
;
; The main parse cannot do this job: it acts on switches as it meets them, so a
; flag that silences output only silences what comes after it. This pass reads
; the same tokens with the same reader and sets nothing else.
prescan_silent:
    mov cl, [0x80]
    xor ch, ch
    mov si, 0x81
.next:
    call skip_blanks
    jcxz .done
    mov al, [si]
    cmp al, 13
    je .done
    cmp al, '/'
    je .switch
    cmp al, '-'
    je .switch
    inc si                      ; not a switch: step over it and keep looking
    dec cx
    jmp .next
.switch:
    inc si
    dec cx
    call read_keyword
    jc .done
    mov di, sw_s
    call token_is
    jc .found
    mov di, sw_silent
    call token_is
    jc .found
    jmp .next
.found:
    mov byte [want_silent], 1
.done:
    ret

; Is `token` the ASCIIZ at DI? CF=1 when it is. Preserves SI.
token_is:
    push si
    mov si, token
    call str_eq
    pop si
    ret

; =============================================================================
; Command tail parsing (PSP:0x80 length, PSP:0x81 text, CR-terminated).
;
; Values are taken as a separate token (`/M 8`), which is how the spec writes
; them, and also after ':' or '=' (`/M:8`), which is how SNDCTRL.COM writes
; them. Both, because a user who has just come from the other tool should not
; have to remember which of the pair takes which punctuation.
; =============================================================================
parse_tail:
    mov cl, [0x80]
    xor ch, ch
    mov si, 0x81
.next:
    call skip_blanks
    jcxz .finish
    mov al, [si]
    cmp al, 13
    jne .token
.finish:
    clc
    ret
.token:
    cmp al, '/'
    je .switch
    cmp al, '-'                 ; DOS tools took either lead-in
    je .switch
    call copy_bad_token
    stc
    ret
.bad:
    stc
    ret
.bad_value:
    mov byte [bad_kind], 1
    stc
    ret
.switch:
    inc si
    dec cx
    call read_keyword
    jc .bad
    call find_switch            ; BX -> entry
    jc .bad
    mov al, [bx + 2]
    mov [cur_kind], al
    mov al, [bx + 3]
    mov [cur_arg], al
    ; Kinds 0-2 are the flags; 3 is a channel level; 4 is the config path.
    cmp byte [cur_kind], 3
    je .channel
    cmp byte [cur_kind], 4
    je .config
    cmp byte [cur_kind], 0
    jne .flag_list
    mov byte [want_usage], 1
    jmp .next
.flag_list:
    cmp byte [cur_kind], 1
    jne .flag_silent
    mov byte [want_list], 1
    jmp .next
.flag_silent:
    cmp byte [cur_kind], 2      ; guard, not an else: an unrouted future kind
    jne .bad                    ; must be refused, not silently become /S
    mov byte [want_silent], 1
    jmp .next

.channel:
    call read_value             ; AX = value
    jc .bad
    cmp ax, 10
    ja .bad_value
    movzx di, byte [cur_arg]
    mov [pending + di], al
    mov byte [pending_set + di], 1
    mov byte [want_apply], 1
    jmp .next

.config:
    call read_path
    jc .bad
    mov byte [have_cfg], 1
    jmp .next

; Read a switch value: either the rest of this token after ':' or '=', or the
; next whitespace-separated token. CF=1 when neither is a number.
read_value:
    jcxz .separate
    mov al, [si]
    cmp al, ':'
    je .inline
    cmp al, '='
    je .inline
.separate:
    call skip_blanks
    jmp read_number
.inline:
    inc si
    dec cx
    jmp read_number

; Read the config-file path: the rest of this token after ':' or '=', or the
; next whitespace-separated token. CF=1 when there is nothing there.
read_path:
    jcxz .separate
    mov al, [si]
    cmp al, ':'
    je .inline
    cmp al, '='
    je .inline
.separate:
    call skip_blanks
    jmp copy_path
.inline:
    inc si
    dec cx
    jmp copy_path

; Copy a whitespace-terminated token at SI/CX into `cfg_path`, uppercased the
; way DOS treats a path. CF=1 when it is empty or would not fit.
copy_path:
    mov di, cfg_path
    xor bx, bx
.loop:
    jcxz .done
    mov al, [si]
    cmp al, ' '
    je .done
    cmp al, 9
    je .done
    cmp al, 13
    je .done
    cmp bx, PATH_MAX - 1
    jae .bad
    cmp al, 'a'
    jb .store
    cmp al, 'z'
    ja .store
    sub al, 0x20
.store:
    mov [di], al
    inc di
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    mov byte [di], 0
    test bx, bx
    jz .bad
    clc
    ret
.bad:
    mov byte [di], 0
    stc
    ret

; Copy the rest of the current token into `token` so an error can quote it.
copy_bad_token:
    push cx
    push si
    mov di, token
    xor bx, bx
.loop:
    jcxz .done
    mov al, [si]
    cmp al, ' '
    je .done
    cmp al, 9
    je .done
    cmp al, 13
    je .done
    cmp bx, TOKEN_MAX - 1
    jae .done
    mov [di], al
    inc di
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    mov byte [di], 0
    pop si
    pop cx
    ret

; Read the switch keyword into `token`, uppercased. CF=1 when empty or too long.
read_keyword:
    mov di, token
    xor bx, bx
.loop:
    jcxz .done
    mov al, [si]
    cmp al, ':'
    je .done
    cmp al, '='
    je .done
    cmp al, ' '
    je .done
    cmp al, 9
    je .done
    cmp al, 13
    je .done
    cmp bx, TOKEN_MAX - 1
    jae .bad
    cmp al, 'a'
    jb .store
    cmp al, 'z'
    ja .store
    sub al, 0x20
.store:
    mov [di], al
    inc di
    inc bx
    inc si
    dec cx
    jmp .loop
.done:
    mov byte [di], 0
    test bx, bx
    jz .bad
    clc
    ret
.bad:
    mov byte [di], 0
    stc
    ret

; Find `token` in the switch table. BX -> entry on success, CF=1 when unknown.
find_switch:
    push si
    mov bx, sw_table
.loop:
    mov ax, [bx]
    test ax, ax
    jz .no
    mov si, token
    mov di, ax
    call str_eq
    jc .yes
    add bx, 4
    jmp .loop
.yes:
    pop si
    clc
    ret
.no:
    pop si
    stc
    ret

; ASCIIZ compare, SI vs DI. CF=1 on match. Clobbers AX, SI, DI.
str_eq:
    mov al, [si]
    cmp al, [di]
    jne .no
    test al, al
    jz .yes
    inc si
    inc di
    jmp str_eq
.yes:
    stc
    ret
.no:
    clc
    ret

; Parse decimal digits at SI/CX into AX. CF=1 when there were none.
;
; A number too big for AX SATURATES at 0xFFFF rather than wrapping. Every
; caller bounds the result against a fader step, so a wrapped value is not a
; harmless wrong number: `/M 65536` wrapped to 0 and silently MUTED the master
; with a clean exit, and `/M 65546` came back as 10. The ceiling is a value no
; caller accepts, so an overlong number is refused for being overlong.
read_number:
    push bx
    xor ax, ax
    xor bx, bx
.loop:
    jcxz .done
    mov dl, [si]
    cmp dl, '0'
    jb .done
    cmp dl, '9'
    ja .done
    sub dl, '0'
    push dx
    mov dx, 10
    mul dx                      ; DX:AX; a non-zero DX is the overflow
    test dx, dx
    pop dx                      ; POP does not disturb the flags
    jnz .saturate
    xor dh, dh
    add ax, dx
    jc .saturate                ; and so is a carry out of the add
    inc bx
    inc si
    dec cx
    jmp .loop
.saturate:
    mov ax, 0xFFFF
    inc bx                      ; digits were seen, so this is not "no number"
.eat:
    ; Consume the rest of the digits so the caller resumes at the same place a
    ; number that fitted would have left it.
    jcxz .done
    mov dl, [si]
    cmp dl, '0'
    jb .done
    cmp dl, '9'
    ja .done
    inc si
    dec cx
    jmp .eat
.done:
    test bx, bx
    pop bx
    jz .none
    clc
    ret
.none:
    stc
    ret

; Advance SI/CX past spaces and tabs.
skip_blanks:
    jcxz .done
    mov al, [si]
    cmp al, ' '
    je .advance
    cmp al, 9
    jne .done
.advance:
    inc si
    dec cx
    jmp skip_blanks
.done:
    ret

; Fold the channels the command line named into the levels read off the card,
; so `/M 8` moves the master and leaves everything else exactly where it was.
merge_pending:
    xor bx, bx
.loop:
    cmp bl, C_COUNT
    jae .done
    movzx di, bl
    cmp byte [pending_set + di], 0
    je .skip
    movzx si, bl
    shl si, 4
    add si, channels
    mov al, [pending + di]
    push bx
    call normalize_step
    pop bx
    mov [si + CH_STEP], al
.skip:
    inc bl
    jmp .loop
.done:
    ret

; =============================================================================
; The config file.
;
; Plain text, one `CHANNEL=step` per line, because the file is meant to be
; readable and editable with the machine's own TYPE and TOKAEDIT. Anything the
; parser does not recognise is skipped rather than refused: a file a user has
; commented is still a file this tool has to be able to read.
; =============================================================================

; Read cfg_path into the channel steps. CF=1 when the file could not be read,
; which is not an error -- see the `.restore` arm.
cfg_load:
    mov ax, 0x3D00
    mov dx, cfg_path
    int 0x21
    jc .missing
    mov bx, ax
    mov ah, 0x3F
    mov cx, CFG_MAX
    mov dx, cfg_buf
    int 0x21
    pushf
    push ax
    mov ah, 0x3E
    int 0x21
    pop ax
    popf
    jc .missing
    mov [cfg_len], ax
    add ax, cfg_buf
    mov [cfg_end], ax
    call cfg_parse
    clc
    ret
.missing:
    stc
    ret

cfg_parse:
    mov si, cfg_buf
.line:
    cmp si, [cfg_end]
    jae .done
    ; Skip leading blanks, then blank and comment lines whole.
.blanks:
    cmp si, [cfg_end]
    jae .done
    mov al, [si]
    cmp al, ' '
    je .eat
    cmp al, 9
    jne .content
.eat:
    inc si
    jmp .blanks
.content:
    cmp al, 13
    je .skip_line
    cmp al, 10
    je .skip_line
    cmp al, ';'
    je .skip_line
    cmp al, '#'
    je .skip_line
    ; KEYWORD = digits
    mov di, token
    xor bx, bx
.name:
    cmp si, [cfg_end]
    jae .skip_line
    mov al, [si]
    cmp al, '='
    je .named
    cmp al, ' '                 ; `MASTER = 8` is a line a person types
    je .name_end
    cmp al, 9
    je .name_end
    cmp al, 13
    je .skip_line
    cmp al, 10
    je .skip_line
    cmp bx, TOKEN_MAX - 1
    jae .skip_line
    cmp al, 'a'
    jb .store
    cmp al, 'z'
    ja .store
    sub al, 0x20
.store:
    mov [di], al
    inc di
    inc bx
    inc si
    jmp .name
; The name ended on a blank, so the '=' is still ahead of it -- unless the line
; is something else entirely, in which case it is skipped like any other line
; the parser does not recognise.
.name_end:
    mov byte [di], 0
.name_blanks:
    cmp si, [cfg_end]
    jae .skip_line
    mov al, [si]
    cmp al, ' '
    je .name_blank
    cmp al, 9
    je .name_blank
    cmp al, '='
    jne .skip_line
    jmp .separator
.name_blank:
    inc si
    jmp .name_blanks
.named:
    mov byte [di], 0
.separator:
    inc si                      ; past '='
.value_blanks:
    cmp si, [cfg_end]
    jae .skip_line
    mov al, [si]
    cmp al, ' '
    je .value_blank
    cmp al, 9
    jne .value_start
.value_blank:
    inc si
    jmp .value_blanks
.value_start:
    call find_channel_key       ; BL = index, CF=1 on a match
    jnc .skip_line
    mov [cfg_channel], bl
    ; The line's number, bounded by the end of the buffer.
    mov cx, [cfg_end]
    sub cx, si
    call read_number
    jc .skip_line
    cmp ax, 10
    ja .skip_line
    movzx di, byte [cfg_channel]
    shl di, 4
    add di, channels
    push si
    mov si, di
    call normalize_step
    mov [si + CH_STEP], al
    pop si
.skip_line:
    cmp si, [cfg_end]
    jae .done
    mov al, [si]
    inc si
    cmp al, 10
    jne .skip_line
    jmp .line
.done:
    ret

; Match `token` against the channel keywords. BL = index, CF=1 on a match.
find_channel_key:
    push si
    xor bl, bl
.loop:
    cmp bl, C_COUNT
    jae .no
    movzx di, bl
    shl di, 4
    mov di, [channels + di + CH_KEY]
    mov si, token
    call str_eq
    jc .yes
    inc bl
    jmp .loop
.yes:
    pop si
    stc
    ret
.no:
    pop si
    clc
    ret

; Write the current steps to cfg_path. Sets cfg_result.
cfg_save:
    mov byte [cfg_result], CFG_ERROR
    mov di, cfg_buf
    mov si, cfg_head
    call copy_str
    xor bx, bx
.line:
    cmp bl, C_COUNT
    jae .write
    push bx
    movzx si, bl
    shl si, 4
    add si, channels
    push si
    mov si, [si + CH_KEY]
    call copy_str
    mov byte [di], '='
    inc di
    pop si
    mov al, [si + CH_STEP]
    call u8dec
    mov si, msg_crlf
    call copy_str
    pop bx
    inc bl
    jmp .line
.write:
    mov ax, di
    sub ax, cfg_buf
    mov [cfg_len], ax
    mov ah, 0x3C
    xor cx, cx
    mov dx, cfg_path
    int 0x21
    jc .done
    mov bx, ax
    mov ah, 0x40
    mov cx, [cfg_len]
    mov dx, cfg_buf
    int 0x21
    pushf
    push ax
    mov ah, 0x3E
    int 0x21
    pop ax
    popf
    jc .done
    cmp ax, [cfg_len]
    jne .done
    mov byte [cfg_result], CFG_WRITTEN
.done:
    ret

; =============================================================================
; Full-screen interface.
;
; Levels are applied to the hardware as the fader moves, not on F10: a mixer
; you cannot hear while you set it is not a mixer. F10 therefore SAVES (and
; leaves), and Esc puts back the levels the run opened on -- which is a real
; undo precisely because the writes already happened.
; =============================================================================
interactive:
    call read_levels
    call video_init
    call draw_screen
    call first_channel
    call draw_faders
    call draw_info
.loop:
    call getkey
    cmp al, 27
    je .cancel
    cmp ah, 0x44                ; F10
    je .save
    cmp al, 9                   ; Tab
    je .forward
    cmp ah, 0x0F                ; Shift+Tab
    je .backward
    cmp ah, 0x4B                ; Left
    je .backward
    cmp ah, 0x4D                ; Right
    je .forward
    cmp ah, 0x48                ; Up
    je .louder
    cmp ah, 0x50                ; Down
    je .quieter
    cmp ah, 0x47                ; Home
    je .full
    cmp ah, 0x4F                ; End
    je .mute
    cmp al, '0'
    jb .loop
    cmp al, '9'
    ja .loop
    sub al, '0'
    jmp .set
.backward:
    call prev_channel
    call draw_faders
    call draw_info
    jmp .loop
.forward:
    call next_channel
    call draw_faders
    call draw_info
    jmp .loop
; Up and Down move one POSITION, which for five of the faders is one step and
; for the PC speaker is one of its four hardware stops. Stepping the speaker by
; one and then snapping would not move at all -- from a stop, the nearest stop
; at or above step+1 is the stop you started on -- so the speaker counts in
; register levels and converts back.
.louder:
    call selected
    cmp byte [si + CH_KIND], K_SPK
    je .spk_up
    mov al, [si + CH_STEP]
    cmp al, 10
    jae .loop
    inc al
    jmp .set_al
.spk_up:
    movzx bx, byte [si + CH_STEP]
    mov bl, [spk_level + bx]
    cmp bl, 3
    jae .loop
    inc bl
    mov al, [spk_step + bx]
    jmp .set_al
.quieter:
    call selected
    cmp byte [si + CH_KIND], K_SPK
    je .spk_down
    mov al, [si + CH_STEP]
    test al, al
    jz .loop
    dec al
    jmp .set_al
.spk_down:
    movzx bx, byte [si + CH_STEP]
    mov bl, [spk_level + bx]
    test bl, bl
    jz .loop
    dec bl
    mov al, [spk_step + bx]
    jmp .set_al
.full:
    mov al, 10
    jmp .set
.mute:
    xor al, al
.set:
    push ax
    call selected
    pop ax
.set_al:
    ; The speaker snaps to its four stops, so stepping "up" from a stop that is
    ; already the highest below the next one has to keep moving rather than
    ; land back where it started; normalize_step rounds up, which is exactly
    ; that.
    call normalize_step
    mov [si + CH_STEP], al
    call apply_channel
    call draw_faders
    call draw_info
    jmp .loop
.save:
    ; Always the default path. The full-screen mixer is only ever reached with
    ; no /CFG on the line -- /CFG alone restores and exits, /CFG with a channel
    ; switch saves and exits -- so a branch here on `have_cfg` was code that
    ; could not run. Writing a different file is the command line's job.
    mov si, s_default_cfg
    mov di, cfg_path
    call copy_str
    mov byte [di], 0
    call cfg_save
    call video_done
    call report
    mov ax, 0x4c00
    int 0x21
.cancel:
    call restore_saved
    call apply_all
    call video_done
    mov si, msg_cancelled
    call print
    mov ax, 0x4c00
    int 0x21

; SI -> the selected channel's record.
selected:
    movzx si, byte [cur_sel]
    shl si, 4
    add si, channels
    ret

restore_saved:
    xor bx, bx
.loop:
    cmp bl, C_COUNT
    jae .done
    movzx si, bl
    shl si, 4
    add si, channels
    mov al, [si + CH_SAVED]
    mov [si + CH_STEP], al
    inc bl
    jmp .loop
.done:
    ret

first_channel:
    mov byte [cur_sel], C_COUNT - 1
    call next_channel
    ret

next_channel:
    mov bl, [cur_sel]
    mov cx, C_COUNT
.loop:
    inc bl
    cmp bl, C_COUNT
    jb .check
    xor bl, bl
.check:
    call index_enabled
    jc .found
    loop .loop
    ret
.found:
    mov [cur_sel], bl
    ret

prev_channel:
    mov bl, [cur_sel]
    mov cx, C_COUNT
.loop:
    test bl, bl
    jnz .step
    mov bl, C_COUNT
.step:
    dec bl
    call index_enabled
    jc .found
    loop .loop
    ret
.found:
    mov [cur_sel], bl
    ret

; 80x25 colour text, blink off so bright backgrounds are solid rather than
; blinking foregrounds, and no cursor.
video_init:
    mov ax, 0x0003
    int 0x10
    mov ax, 0x1003
    xor bx, bx
    int 0x10
    mov ah, 0x01
    mov cx, 0x2000
    int 0x10
    mov ax, 0xB800
    mov es, ax
    ret

video_done:
    mov ax, 0x0003
    int 0x10
    mov ax, 0x1003
    mov bx, 1
    int 0x10
    push ds
    pop es
    ret

getkey:
    mov ah, 0
    int 0x16
    ret

draw_screen:
    call cls
    call draw_shadow
    call draw_box
    call draw_static
    ret

cls:
    xor di, di
    mov ax, 0x0020
    mov cx, 80 * 25
    rep stosw
    ret

; AL = row, AH = column -> DI = text-buffer offset. Preserves everything else.
screen_at:
    push ax
    push bx
    push dx
    mov bl, ah
    xor bh, bh
    xor ah, ah
    mov dx, 160
    mul dx
    shl bx, 1
    add ax, bx
    mov di, ax
    pop dx
    pop bx
    pop ax
    ret

; SI -> ASCIIZ, DI = offset, AH = attribute. DI ends past the text.
puts:
    lodsb
    test al, al
    jz .done
    stosw
    jmp puts
.done:
    ret

draw_box:
    mov al, BOX_ROW
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xDA
    stosw
    mov al, 0xC4
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xBF
    stosw
    mov bl, BOX_ROW + 1
.row:
    mov al, bl
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    mov al, ' '
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xB3
    stosw
    inc bl
    cmp bl, BOX_ROW + BOX_H - 1
    jb .row
    mov al, BOX_ROW + BOX_H - 1
    mov ah, BOX_COL
    call screen_at
    mov ah, A_BOX
    mov al, 0xC0
    stosw
    mov al, 0xC4
    mov cx, BOX_W - 2
    rep stosw
    mov al, 0xD9
    stosw
    ret

draw_shadow:
    mov al, BOX_ROW + BOX_H
    mov ah, BOX_COL + 2
    call screen_at
    mov ax, (A_SHADOW << 8) | ' '
    mov cx, BOX_W
    rep stosw
    mov bl, BOX_ROW + 1
.row:
    mov al, bl
    mov ah, BOX_COL + BOX_W
    call screen_at
    mov ax, (A_SHADOW << 8) | ' '
    mov cx, 2
    rep stosw
    inc bl
    cmp bl, BOX_ROW + BOX_H
    jb .row
    ret

draw_static:
    mov si, static_text
.loop:
    mov al, [si]
    cmp al, 0xFF
    je .done
    mov ah, [si + 1]
    call screen_at
    mov ah, [si + 2]
    push si
    mov si, [si + 3]
    call puts
    pop si
    add si, 5
    jmp .loop
.done:
    ret

draw_faders:
    xor bl, bl
.loop:
    cmp bl, C_COUNT
    jae .done
    push bx
    call draw_fader
    pop bx
    inc bl
    jmp .loop
.done:
    ret

; BL = channel index.
draw_fader:
    movzx si, bl
    shl si, 4
    add si, channels
    mov [tmp_chan], bl
    ; The track's frame, drawn whether or not the device is there: an absent
    ; channel shows an empty track with an asterisk where its number goes,
    ; which is the same "nothing to edit here" the sibling tool draws.
    mov al, TRACK_TOP
    mov ah, [si + CH_COL]
    call screen_at
    mov ah, A_BOX
    mov al, 0xDA
    stosw
    mov al, 0xC4
    mov cx, 3
    rep stosw
    mov al, 0xBF
    stosw
    mov al, TRACK_BOT
    mov ah, [si + CH_COL]
    call screen_at
    mov ah, A_BOX
    mov al, 0xC0
    stosw
    mov al, 0xC4
    mov cx, 3
    rep stosw
    mov al, 0xD9
    stosw
    ; The ten cells, filled from the bottom up.
    call channel_enabled
    jc .cells
    xor bh, bh                  ; absent: no fill at all
    jmp .cell_loop
.cells:
    mov bh, [si + CH_STEP]
.cell_loop:
    mov bl, TRACK_TOP + 1
.cell:
    cmp bl, TRACK_BOT
    jae .name
    mov al, bl
    mov ah, [si + CH_COL]
    call screen_at
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    ; This row is filled when the step reaches it: the bottom cell is step 1
    ; and the top is step 10, so the row's own step is TRACK_BOT - row.
    mov al, TRACK_BOT
    sub al, bl
    mov ah, A_BOX
    cmp al, bh
    ja .empty
    ; The selected fader's column is drawn in the title colour so the eye can
    ; find it without hunting for the highlighted number underneath.
    mov al, [tmp_chan]
    cmp al, [cur_sel]
    jne .filled
    mov ah, A_TITLE
.filled:
    mov al, 0xDB
    jmp .put
.empty:
    mov al, 0xB0                ; a light hatch, so the empty track is visible
    mov ah, A_BOX
.put:
    mov cx, 3
    rep stosw
    mov ah, A_BOX
    mov al, 0xB3
    stosw
    inc bl
    jmp .cell
.name:
    ; The number, in an input's colours.
    mov al, VALUE_ROW
    mov ah, [si + CH_COL]
    call screen_at
    mov ah, A_FIELD
    mov bl, [tmp_chan]
    cmp bl, [cur_sel]
    jne .value_attr
    mov ah, A_SEL
.value_attr:
    mov [tmp_attr], ah
    mov al, ' '
    mov cx, 5
    rep stosw
    call channel_enabled
    jnc .absent
    mov al, [si + CH_STEP]
    call fmt_step               ; numbuf, CX = length
    mov bx, 5
    sub bx, cx
    shr bx, 1
    mov al, VALUE_ROW
    mov ah, [si + CH_COL]
    add ah, bl
    call screen_at
    mov ah, [tmp_attr]
    push si
    mov si, numbuf
    call puts
    pop si
    jmp .label
.absent:
    mov al, VALUE_ROW
    mov ah, [si + CH_COL]
    add ah, 2
    call screen_at
    mov ah, A_BOX
    mov al, '*'
    stosw
.label:
    mov al, NAME_ROW
    mov ah, [si + CH_NAMECOL]
    call screen_at
    mov ah, A_BOX
    push si
    mov si, [si + CH_NAME]
    call puts
    pop si
    ret

; The one-line description of the selected channel, plus what its current step
; costs in dB. Redrawn on every change, so the number on screen is always the
; number the hardware is holding.
draw_info:
    mov al, INFO_ROW
    mov ah, BOX_COL + 2
    call screen_at
    mov ax, (A_BOX << 8) | ' '
    mov cx, BOX_W - 4
    rep stosw
    call selected
    mov al, INFO_ROW
    mov ah, BOX_COL + 2
    call screen_at
    mov ah, A_BOX
    push si
    mov si, [si + CH_DESC]
    call puts
    pop si
    mov al, INFO_ROW
    mov ah, BOX_COL + 52
    call screen_at
    mov ah, A_TITLE
    push si
    call db_text                ; SI -> the dB string for this step
    call puts
    pop si
    ret

; SI -> channel record. Returns SI -> the ASCIIZ dB label for its current step.
;
; DI is preserved because both callers are mid-write when they ask: the screen
; painter is holding a text-buffer offset and the reporter is holding a
; `linebuf` cursor. Leaving DI as the step number pointed the next write at
; offset 0x0A of the PSP.
db_text:
    push di
    movzx di, byte [si + CH_STEP]
    cmp byte [si + CH_KIND], K_SPK
    je .speaker
    shl di, 1
    mov si, [db_ct5 + di]
    pop di
    ret
.speaker:
    movzx di, byte [spk_level + di]
    shl di, 1
    mov si, [db_spk + di]
    pop di
    ret

; AL = step -> numbuf ASCIIZ, CX = length.
fmt_step:
    push di
    mov di, numbuf
    call u8dec
    mov byte [di], 0
    pop di
    ret

; AL (0..19) as decimal at [DI]. DI advances; CX = digits written.
u8dec:
    mov cx, 1
    cmp al, 10
    jb .ones
    mov byte [di], '1'
    inc di
    sub al, 10
    mov cx, 2
.ones:
    add al, '0'
    mov [di], al
    inc di
    ret

; Copy the ASCIIZ at SI to [DI] without its terminator. CX = bytes copied.
copy_str:
    xor cx, cx
.loop:
    mov al, [si]
    test al, al
    jz .done
    mov [di], al
    inc si
    inc di
    inc cx
    jmp .loop
.done:
    ret

; =============================================================================
; Reporting.
; =============================================================================
print:
    cmp byte [want_silent], 0
    jne .done
    push ax
    push dx
    push si
.loop:
    mov dl, [si]
    test dl, dl
    jz .end
    mov ah, 0x02
    int 0x21
    inc si
    jmp .loop
.end:
    pop si
    pop dx
    pop ax
.done:
    ret

; Print `linebuf` as built so far, then reset it.
flush_line:
    mov byte [di], 0
    push si
    mov si, linebuf
    call print
    pop si
    ret

report:
    cmp byte [want_silent], 0
    jne .done
    mov si, msg_head
    call print
    cmp byte [sb_present], 0
    jne .rows
    cmp byte [wss_present], 0
    jne .rows
    mov si, msg_no_card
    call print
    ret
.rows:
    xor bx, bx
.row:
    cmp bl, C_COUNT
    jae .after
    push bx
    movzx si, bl
    shl si, 4
    add si, channels
    call channel_enabled
    jnc .row_done
    mov di, linebuf
    push si
    mov si, s_indent
    call copy_str
    pop si
    push si
    mov si, [si + CH_NAME]
    call copy_str
    pop si
    ; Pad the name out to a fixed column so the numbers line up.
    mov bx, 12
    sub bx, cx
.pad:
    test bx, bx
    jle .value
    mov byte [di], ' '
    inc di
    dec bx
    jmp .pad
.value:
    ; Right-align the step in two columns so the dB figures beside them line up.
    mov al, [si + CH_STEP]
    cmp al, 10
    jae .digits
    mov byte [di], ' '
    inc di
.digits:
    call u8dec
    push si
    mov si, s_gap
    call copy_str
    pop si
    push si
    call db_text
    call copy_str
    pop si
    mov si, msg_crlf
    call copy_str
    call flush_line
.row_done:
    pop bx
    inc bl
    jmp .row
.after:
    cmp byte [applied], 0
    je .cfg
    mov si, msg_applied
    call print
.cfg:
    cmp byte [cfg_result], CFG_WRITTEN
    jne .cfg_error
    mov di, linebuf
    mov si, msg_saved
    call copy_str
    mov si, cfg_path
    call copy_str
    mov si, msg_dot
    call copy_str
    call flush_line
    ret
.cfg_error:
    cmp byte [cfg_result], CFG_ERROR
    jne .done
    mov di, linebuf
    mov si, msg_save_failed
    call copy_str
    mov si, cfg_path
    call copy_str
    mov si, msg_dot
    call copy_str
    call flush_line
.done:
    ret

report_applied:
    mov di, linebuf
    mov si, msg_restored
    call copy_str
    mov si, cfg_path
    call copy_str
    mov si, msg_dot
    call copy_str
    call flush_line
    ret

report_no_cfg:
    mov di, linebuf
    mov si, msg_no_file
    call copy_str
    mov si, cfg_path
    call copy_str
    mov si, msg_dot
    call copy_str
    call flush_line
    ret

; =============================================================================
; Data.
; =============================================================================
CFG_NONE    equ 0
CFG_WRITTEN equ 1
CFG_ERROR   equ 2

; The fader law, as three tables the code indexes by step. Keeping them as data
; rather than arithmetic is what makes the mapping in the header comment
; checkable by eye against what the tool actually writes.
;
; 5-bit levels: step 0 is the register's hard mute, then 13..31 in twos, which
; is -36 dB to 0 dB in fours.
step_level: db 0, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31

; AD1848 output attenuation for the same steps: the codec moves in 1.5 dB, so
; each stop takes the count nearest the same dB figure. Reading the table below
; from step 10 down to step 1, counts 0, 3, 5, 8, 11, 13, 16, 19, 21, 24 are
; -0, -4.5, -7.5, -12, -16.5, -19.5, -24, -28.5, -31.5, -36 dB -- ten counts and
; ten figures. Step 0 sets the register's own mute bit rather than counting all
; the way down to it.
wss_atten:  db WSS_DAC_MUTE, 24, 21, 19, 16, 13, 11, 8, 5, 3, 0

; The 2-bit PC-speaker register has four positions, so a step maps onto one of
; them and the fader shows the step that position IS (0, 3, 7, 10) rather than
; the one the user asked for. spk_snap rounds a request up to the nearest stop
; at or above it, so a request for any audible level gets an audible one, and
; the four canonical steps are fixed points of it.
spk_level:  db 0, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3
spk_snap:   db 0, 3, 3, 3, 7, 7, 7, 7, 10, 10, 10
spk_step:   db 0, 3, 7, 10

; kind, reg, col, namecol, step, saved, dev, spare / name, key, desc, pad
channels:
    db K_CT5, 0x30,  8,  7, 10, 10, DEV_SB, 0
    dw nm_master, key_master, ds_master, 0
    db K_CT5, 0x34, 19, 18, 10, 10, DEV_SB, 0
    dw nm_fm, key_fm, ds_fm, 0
    db K_WAV, 0x32, 30, 30, 10, 10, DEV_ANY, 0
    dw nm_wave, key_wave, ds_wave, 0
    db K_CT5, 0x36, 41, 40, 10, 10, DEV_SB, 0
    dw nm_cd, key_cd, ds_cd, 0
    db K_MID, 0x50, 52, 52, 10, 10, DEV_SB, 0
    dw nm_midi, key_midi, ds_midi, 0
    db K_SPK, 0x3B, 63, 62, 10, 10, DEV_SB, 0
    dw nm_spk, key_spk, ds_spk, 0

nm_master: db 'MASTER', 0
nm_fm:     db 'FMSYNTH', 0
nm_wave:   db 'WAVE', 0
nm_cd:     db 'CD-ROM', 0
nm_midi:   db 'MIDI', 0
nm_spk:    db 'SPEAKER', 0

key_master: db 'MASTER', 0
key_fm:     db 'FMSYNTH', 0
key_wave:   db 'WAVE', 0
key_cd:     db 'CD', 0
key_midi:   db 'MIDI', 0
key_spk:    db 'SPEAKER', 0

ds_master: db 'MASTER    everything the card plays', 0
ds_fm:     db 'FMSYNTH   OPL3 FM synthesis, the music bus', 0
ds_wave:   db 'WAVE      digital audio: SB16 DSP and WSS codec', 0
ds_cd:     db 'CD-ROM    Red Book audio from the CD drive', 0
ds_midi:   db 'MIDI      wavetable synthesis from the MPU-401', 0
ds_spk:    db 'SPEAKER   the PC speaker, through the card', 0

; The dB each step costs, as text, for the two ladders on the card.
db_ct5:
    dw s_mute, s_m36, s_m32, s_m28, s_m24, s_m20
    dw s_m16, s_m12, s_m8, s_m4, s_m0
db_spk:
    dw s_mute, s_m14, s_m7, s_m0

s_mute: db 'mute', 0
s_m36:  db '-36 dB', 0
s_m32:  db '-32 dB', 0
s_m28:  db '-28 dB', 0
s_m24:  db '-24 dB', 0
s_m20:  db '-20 dB', 0
s_m16:  db '-16 dB', 0
s_m14:  db '-14 dB', 0
s_m12:  db '-12 dB', 0
s_m8:   db '-8 dB', 0
s_m7:   db '-7 dB', 0
s_m4:   db '-4 dB', 0
s_m0:   db '0 dB', 0

; Switch keyword, kind (0 usage, 1 list, 2 silent, 3 channel level, 4 config
; path), argument (the channel index for kind 3).
; Routing rule enforced in parse_tail: kinds 3 and 4 are tested for by name and
; every other kind is a flag that MUST get an explicit, guarded arm in the
; .flag chain -- an unrouted kind is refused, not quietly treated as /S.
sw_table:
    dw sw_question
    db 0, 0
    dw sw_h
    db 0, 0
    dw sw_help
    db 0, 0
    dw sw_l
    db 1, 0
    dw sw_list
    db 1, 0
    dw sw_s
    db 2, 0
    dw sw_silent
    db 2, 0
    dw sw_m
    db 3, C_MASTER
    dw sw_master
    db 3, C_MASTER
    dw sw_f
    db 3, C_FM
    dw sw_fm
    db 3, C_FM
    dw sw_w
    db 3, C_WAVE
    dw sw_wave
    db 3, C_WAVE
    dw sw_c
    db 3, C_CD
    dw sw_cd
    db 3, C_CD
    dw sw_i
    db 3, C_MIDI
    dw sw_midi
    db 3, C_MIDI
    dw sw_p
    db 3, C_SPK
    dw sw_spk
    db 3, C_SPK
    dw sw_cfg
    db 4, 0
    dw 0
    db 0, 0

sw_question: db '?', 0
sw_h:        db 'H', 0
sw_help:     db 'HELP', 0
sw_l:        db 'L', 0
sw_list:     db 'LIST', 0
sw_s:        db 'S', 0
sw_silent:   db 'SILENT', 0
sw_m:        db 'M', 0
sw_master:   db 'MASTER', 0
sw_f:        db 'F', 0
sw_fm:       db 'FMSYNTH', 0
sw_w:        db 'W', 0
sw_wave:     db 'WAVE', 0
sw_c:        db 'C', 0
sw_cd:       db 'CD', 0
sw_i:        db 'I', 0
sw_midi:     db 'MIDI', 0
sw_p:        db 'P', 0
sw_spk:      db 'SPEAKER', 0
sw_cfg:      db 'CFG', 0

; row, col, attribute, text
static_text:
    db  3, 27, A_TITLE
    dw t_title
    db 20,  6, A_BOX
    dw t_keys
    db 0xFF

t_title: db 'ReSonique 2 Volume Mixer', 0
t_keys:  db 'Left/Right  channel    Up/Down  level    Home/End  full/mute', 0

s_indent:      db '  ', 0
s_gap:         db '   ', 0
s_default_cfg: db 'C:\VOLCONF.CFG', 0

cfg_head:
    db '; ReSonique 2 volume levels, written by SNDMIXER.COM.', 13, 10
    db '; One channel per line: 0 mutes, 10 is full. Spaces around the', 13, 10
    db '; = are fine. SPEAKER has four positions on the card, so it', 13, 10
    db '; reads back as 0, 3, 7 or 10.', 13, 10, 0

msg_head:        db 'ReSonique 2 volume levels', 13, 10, 0
msg_no_card:     db '  No ReSonique 2 card detected.', 13, 10, 0
msg_applied:     db 'Applied to the mixer.', 13, 10, 0
msg_saved:       db 'Saved in ', 0
msg_save_failed: db 'Could not write ', 0
msg_restored:    db 'Volume levels restored from ', 0
msg_no_file:     db 'No saved volume levels in ', 0
msg_cancelled:   db 'Cancelled; the levels this run opened on are back.', 13, 10, 0
msg_bad_switch:  db 'Unrecognised option: ', 0
msg_bad_value:   db 'A level must be 0 to 10, on ', 0
msg_dot:         db '.', 13, 10, 0
msg_crlf:        db 13, 10, 0
msg_usage:
    db 'SNDMIXER - ReSonique 2 Volume Mixer', 13, 10, 13, 10
    db '  SNDMIXER                full-screen mixer', 13, 10
    db '  SNDMIXER /L             list the current levels', 13, 10
    db '  SNDMIXER /CFG file      restore the levels saved in a file', 13, 10
    db '  SNDMIXER /M n           MASTER      0 (mute) to 10 (full)', 13, 10
    db '  SNDMIXER /F n           FMSYNTH     OPL3 music', 13, 10
    db '  SNDMIXER /W n           WAVE        SB16 DSP and WSS codec', 13, 10
    db '  SNDMIXER /C n           CD-ROM      Red Book audio', 13, 10
    db '  SNDMIXER /I n           MIDI        wavetable synthesis', 13, 10
    db '  SNDMIXER /P n           PC speaker  four positions: 0 3 7 10', 13, 10
    db '  SNDMIXER /S             say nothing at all', 13, 10, 13, 10
    db 'Each step is 4 dB, so all ten are worth having; step 10 is the', 13, 10
    db 'level the card powers on at. /CFG on its own restores a saved', 13, 10
    db 'file; /CFG with any channel switch saves the result into it.', 13, 10
    db 'In the full-screen mixer F10 saves and Esc puts back what was', 13, 10
    db 'there. Levels move the hardware as you set them.', 13, 10, 0

; ---- state ------------------------------------------------------------------
sb_present:  db 0
wss_present: db 0
want_usage:  db 0
want_list:   db 0
want_silent: db 0
want_apply:  db 0
have_cfg:    db 0
applied:     db 0
cfg_result:  db CFG_NONE
cur_kind:    db 0
cur_arg:     db 0
bad_kind:    db 0
cur_sel:     db 0
tmp_attr:    db 0
tmp_chan:    db 0
cfg_channel: db 0
pending:     times C_COUNT db 0
pending_set: times C_COUNT db 0

cfg_len:     dw 0
cfg_end:     dw 0

; ---- buffers ----------------------------------------------------------------
; Declared past the end of the image rather than inside it: a .COM is handed the
; whole 64K segment, so these cost address space but not file size.
image_end:
    absolute image_end
token:       resb TOKEN_MAX
numbuf:      resb 8
linebuf:     resb 128
cfg_path:    resb PATH_MAX
cfg_buf:     resb CFG_MAX
