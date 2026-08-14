; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; izarra-bios.asm - Izarra3000 clean-room real-mode BIOS (POST, RAM test,
; component/peripheral probes, mode-13h status + setup page).
; Assemble with: nasm -f bin izarra-bios.asm -o izarra-bios.bin
;
; This is the only file that lists includes. The order is fixed.
; izbios-tables.inc must stay last because it emits the POST step table that
; POST_STEP accumulated across every prior include. izbios-art.inc (generated)
; sits right after the reset jump and before core/gfx/lfb because its geometry
; %defines are textual and must precede every routine that references them.
bits 16
org 0

%include "izbios-defs.inc"      ; foundation: shared constants (emits no bytes)

reset:                          ; ROM offset 0; the reset vector far-jumps here
    jmp bios_start

%include "izbios-art.inc"       ; generated art: palette + RLE bg/icons/boot box (geometry %defines used by core/gfx/lfb below)
%include "izbios-core.inc"      ; foundation: bring-up, PIC, POST sequencer, helpers
%include "izbios-gfx.inc"       ; foundation: mode-13h primitives + 8x8 font
%include "izbios-codepage.inc"  ; foundation: code-page font loader (Lotura 0xE7 + INT 10h)
%include "izbios-lfb.inc"       ; foundation: 320x240x8 LFB draw primitives
%include "izbios-kbd.inc"       ; foundation: INT 09h/16h + kb_getkey/kb_flush
%include "kbd-layouts.inc"      ; foundation: scancode -> ASCII layout tables (17 layouts)
%include "kbd-layout-meta.inc"  ; generated: kbd_layout_codepage table (cp index per layout)
%include "izbios-result.inc"    ; foundation: POST_STEP macro + result_append
%include "probes/probe-cpu.inc"      ; GSW CPU mode detection
%include "probes/probe-margo.inc"    ; VEGA/Margo video screen path
%include "ramtest-core.inc"          ; RAM test
%include "probe-table.inc"           ; shared probe table
%include "probes/probe-lotura.inc"   ; Lotura controller
%include "probes/probe-kbd8042.inc"  ; keyboard controller
%include "probes/probe-pit.inc"      ; PIT
%include "probes/probe-serial.inc"   ; serial port
%include "probes/probe-sbdsp.inc"    ; Sound Blaster DSP
%include "probes/probe-opl.inc"      ; OPL
%include "probes/probe-floppy.inc"   ; floppy disk controller
%include "probes/probe-hdd.inc"      ; ATA hard disk
%include "probes/probe-optical.inc"  ; ATAPI optical drive
%include "setup-ui.inc"              ; setup UI
%include "izbios-boot.inc"           ; INT 19h bootstrap
%include "izbios-bootbox.inc"        ; boot and speed menu
%include "izbios-chime.inc"          ; power-on speaker chime
%include "izbios-logo.inc"           ; Izarra3000 wordmark
%include "izbios-tables.inc"    ; foundation: MUST be last (emits the step table)

; INT 13h ROM entry at ROM offset 0xF000 (linear 0xFF000, i.e. FF00:0000).
; Period PC booters often repoint IVT[0x13] to FF00:0000 to chain disk calls
; through the ROM-BIOS handler, then issue INT 13h. The host services the disk
; work by intercepting the INT 13h instruction itself (keyed on the vector
; number, not the IVT target), so the redirected vector only needs a valid IRET
; to land on. This stub provides that return point.
    times 0xf000 - ($ - $$) db 0
int13_rom_entry:
    iret

; VBE 2.0 protected-mode interface block at ROM offset 0xF100, so INT 10h
; AX=4F0Ah can hand the client the far pointer F000:F100. The offset is fixed
; rather than derived because Rust has to name it (IZARRA_BIOS_VBE_PM_OFFSET)
; and the ROM ships as a checked-in binary; `vbe_pm_block_sits_at_its_fixed_rom_offset`
; asserts the assembled bytes really are here.
    times 0xf0fe - ($ - $$) db 0
%include "izbios-vbepm.inc"

; Reset vector at 0xFFFF0 (file offset 0xFFF0 in a 64 KiB ROM). The exact-64K
; tail and the far jump to ROM_SEG:reset mirror the other Izarra ROMs.
    times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp ROM_SEG:reset
    times 0x10000 - ($ - $$) db 0
