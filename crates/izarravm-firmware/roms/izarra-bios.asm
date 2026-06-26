; izarra-bios.asm - Izarra 3000 clean-room real-mode BIOS (POST, RAM test,
; component/peripheral probes, mode-13h status + setup page).
; Assemble with: nasm -f bin izarra-bios.asm -o izarra-bios.bin
;
; This skeleton is the ONLY file that lists %includes. The order below is frozen;
; each work-stream owns exactly one .inc file so parallel agents never edit a
; shared file. izbios-tables.inc MUST stay last: it emits the POST step table that
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
%include "probes/probe-cpu.inc"      ; STREAM C: GSW-586 CPU (CPUID identity)
%include "probes/probe-margo.inc"    ; STREAM C: VEGA/Margo video (screen path)
%include "ramtest-core.inc"          ; STREAM B
%include "probe-table.inc"           ; STREAM C (shared, reserved)
%include "probes/probe-lotura.inc"   ; STREAM C
%include "probes/probe-kbd8042.inc"  ; STREAM C
%include "probes/probe-pit.inc"      ; STREAM C
%include "probes/probe-serial.inc"   ; STREAM C
%include "probes/probe-sbdsp.inc"    ; STREAM C
%include "probes/probe-opl.inc"      ; STREAM C
%include "probes/probe-floppy.inc"   ; STREAM C: floppy disk controller (FDC MSR)
%include "probes/probe-hdd.inc"      ; STREAM C: ATA hard disk (primary status)
%include "probes/probe-optical.inc"  ; STREAM C: ATAPI optical (secondary signature)
%include "setup-ui.inc"              ; STREAM D
%include "izbios-boot.inc"           ; STREAM E: INT 19h bootstrap (boot_entry)
%include "izbios-bootbox.inc"        ; STREAM E: boxed two-pane boot+speed menu
%include "izbios-chime.inc"          ; STREAM E: power-on PC-speaker chime (play_chime)
%include "izbios-logo.inc"           ; STREAM E: red "Izarra 3000" wordmark bitmap
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

; Reset vector at 0xFFFF0 (file offset 0xFFF0 in a 64 KiB ROM). The exact-64K
; tail and the far jump to ROM_SEG:reset mirror the other Izarra ROMs.
    times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp ROM_SEG:reset
    times 0x10000 - ($ - $$) db 0
