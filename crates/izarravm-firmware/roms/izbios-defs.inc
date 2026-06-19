; izbios-defs.inc - foundation: shared constants for the Izarra 3000 BIOS.
; This file emits NO bytes (only %define / equ), so reset: stays at ROM offset 0.
; Read-only contract: streams use these names and never redefine them.

%define ROM_SEG        0xf000      ; the BIOS is mapped at 0xF0000
%define VGA13_SEG      0xa000      ; mode 13h framebuffer segment (linear, chain-4)
%define BDA_SEG        0x0040      ; BIOS data area segment
%define STACK_TOP      0x7000      ; SS:SP top at boot (grows down, clear of RAM map)

; BDA keyboard ring (offsets into BDA_SEG), shared by core bring-up and the
; INT 09h/16h handlers in izbios-kbd.inc.
%define KB_BUF_START   0x001e
%define KB_BUF_HEAD    0x001a
%define KB_BUF_TAIL    0x001c
%define KB_FLAGS       0x0017

; Unreal-mode flat data selector (GDT index 1) for ramtest_peek32/poke32 and
; read_margo_reg. FS is loaded with this once at bring-up and never reloaded.
%define UNREAL_SEL     0x08

; VDTS result block (parsed by izarravm_firmware::parse_result_block).
%define RESULT_BLOCK   0x9000      ; physical offset in segment 0
%define RESULT_LIMIT   0x9800      ; result block reserved through here (2 KiB)

; result_append status codes (the record verb).
%define RESULT_BEGIN   0
%define RESULT_PASS    1
%define RESULT_FAIL    2
%define RESULT_MEASURE 3

; POST handler return codes (AL on handler return). The sequencer maps these to
; the RESULT_* codes by adding 1 (PASS->RESULT_PASS, etc.).
%define HRES_PASS      0
%define HRES_FAIL      1
%define HRES_MEASURE   2

; Setup hotkey: Del (AT scancode set 1 make code), per Phase 0 decision 5.
%define SETUP_HOTKEY   0x53

; Status panel layout (mode 13h pixels, 8x8 glyphs).
%define STATUS_X       8
%define STATUS_TITLE_Y 8
%define STATUS_Y       24
%define STATUS_LINE_DY 10
%define COLOR_BG       0x00        ; black
%define COLOR_TITLE    0x0f        ; white
%define COLOR_PASS     0x0a        ; bright green
%define COLOR_FAIL     0x0c        ; bright red

; --- Scratch RAM map (segment 0). ROM writes are dropped by the bus, so all
; mutable state lives here. Each region is disjoint and owned by one stream;
; the foundation hands these offsets out and streams never overlap. The map skips
; 0x0600: the emulator installs its BIOS IRET stub there (the target of the
; INT 10h/13h/15h/20h/21h vectors), so writing it would corrupt those services. ---
%define BIOS_VALBUF      0x0500    ; foundation: ASCIIZ value/format buffer (32 B)
%define BIOS_SCRATCH     0x0520    ; foundation: general scratch (32 B)
%define RAMTEST_SCRATCH  0x0540    ; STREAM B: detected size + flags (160 B, ends 0x5DF)
; 0x0600: emulator BIOS IRET stub. Reserved; never write here.
%define SETUP_SCRATCH    0x0610    ; STREAM D: setup working copy (256 B, 0x610-0x70F)
%define PROBE_SCRATCH    0x0710    ; STREAM C: per-probe value buffers (240 B, 0x710-0x7FF)
; 0x0800..STACK area free; stack top STACK_TOP; result block at RESULT_BLOCK.

; --- Device ports (read-only contract) ---
%define PORT_LOTURA_ID   0xe0      ; reads LOTURA_ID_VALUE
%define PORT_LOTURA_MODE 0xe1      ; GSW-586 live mode register (write 0/1/2)
%define LOTURA_ID_VALUE  0x5a
%define MARGO_MMIO_BASE  0xe0400000 ; Margo register block (32-bit MMIO)
%define PORT_DSP_RESET   0x226
%define PORT_DSP_READ    0x22a
%define PORT_DSP_WSTATUS 0x22c
%define PORT_DSP_RSTATUS 0x22e
%define PORT_OPL_ADDR    0x388
%define PORT_OPL_DATA    0x389
%define PORT_KBD_DATA    0x60
%define PORT_KBD_CMD     0x64
%define PORT_PIT_CH0     0x40
%define PORT_PIT_CTRL    0x43
%define PORT_PIC_CMD     0x20
%define PORT_PIC_DATA    0x21
%define PORT_PIC2_CMD    0xa0
%define PORT_PIC2_DATA   0xa1
%define PORT_COM1_BASE   0x3f8
%define PORT_COM1_LSR    0x3fd
%define PORT_COM1_SCR    0x3ff
