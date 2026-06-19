; Resident keyboard BIOS for the HLE DOS machine: INT 09h and INT 16h only, no
; reset and no echo loop. Installed at F000:0000 by new_dos_program. The two
; header words give the installer the handler entry offsets.
bits 16
org 0

    dw int09                    ; header word 0: INT 09h entry offset
    dw int16                    ; header word 1: INT 16h entry offset

%include "kbd-bios-core.inc"
