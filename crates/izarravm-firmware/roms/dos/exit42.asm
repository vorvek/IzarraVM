; EXIT42.COM - terminate immediately with DOS exit code 42 (0x2A). The katea-run
; e2e fixture: proves RUNNER captures and reports a child's exit code.
        cpu 8086
        org 0x100
        mov     ax, 0x4C2A              ; AH=4Ch terminate, AL=2Ah=42
        int     0x21
