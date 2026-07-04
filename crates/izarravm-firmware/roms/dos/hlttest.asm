; HLTTEST.COM - enable interrupts, execute a real HLT, then terminate with DOS
; exit code 1. The katea-run e2e fixture for guest HLT under TOKAEMM (V86):
; proves the CPU's HLT-is-privileged #GP(0) and TOKAEMM's .hlt emulation
; (real ring-0 sti;hlt, then resume past the F4 byte) round-trip correctly on
; the real machine, not just the CPU-crate monitor stand-in.
        cpu 8086
        org 0x100
        sti
        hlt
        mov     ax, 0x4C01              ; AH=4Ch terminate, AL=1
        int     0x21
