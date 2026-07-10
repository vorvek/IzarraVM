; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; MULTIHLT.COM - enable interrupts, execute a real HLT five times in a loop,
; then terminate with DOS exit code 7. Catches drift across repeated guest
; HLTs under TOKAEMM's monitor emulation (e.g. a corrupted saved-register or
; stack-depth regression that only shows up on the second or later wake).
        cpu 8086
        org 0x100
        sti
        mov     cx, 5
.loop:
        hlt
        loop    .loop
        mov     ax, 0x4C07              ; AH=4Ch terminate, AL=7
        int     0x21
