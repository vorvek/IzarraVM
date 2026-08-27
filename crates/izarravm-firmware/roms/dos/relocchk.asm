; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; RELOCCHK.EXE - regression fixture for the kernel's .EXE relocation loop.
;
; A hand-crafted MZ image with NRELOC=130 relocation entries. The count
; crosses the kernel's RELOC_SPAN=32 staging span four times and leaves a
; 2-entry remainder, so one load exercises the full-span path, the
; span-boundary hand-off, and the partial final span. Each fixup points at
; one slot word whose link-time value is its own index i; a correct loader
; leaves slot[i] == i + CS. A skipped fixup keeps i; a doubled fixup lands
; at i + 2*CS; a misaligned span shifts which slots move. The canary word
; carries NO relocation entry and must still read 0x1234 after load - it
; catches a span that applies one entry too many.
;
; Exit codes: 42 = every fixup applied exactly once; 1 = a slot mismatch;
; 2 = the canary moved.
;
; Build: nasm -f bin relocchk.asm -o relocchk.exe
        cpu 8086
        bits 16

NRELOC   equ 130
HDR_PARA equ 64                 ; 1024-byte header holds the 520-byte table

header_start:
        db 'M', 'Z'
        dw file_size % 512      ; bytes in last 512-byte page
        dw (file_size + 511) / 512
        dw NRELOC
        dw HDR_PARA
        dw 16                   ; minalloc: slack past the image
        dw 0xffff               ; maxalloc
        dw 0                    ; initial SS, relocated by the loader
        dw stack_top            ; initial SP
        dw 0                    ; checksum, unused
        dw 0                    ; initial IP = image start
        dw 0                    ; initial CS, relocated by the loader
        dw reloc_table - header_start
        dw 0                    ; overlay number
        times 0x40 - ($ - header_start) db 0
reloc_table:
%assign i 0
%rep NRELOC
        dw (slots - image_start) + 2 * i, 0
%assign i i + 1
%endrep
        times (HDR_PARA * 16) - ($ - header_start) db 0

image_start:
start:
        mov     ax, cs
        mov     ds, ax
        xor     di, di                  ; slot index, also the expected delta
        mov     si, slots - image_start
.check:
        mov     ax, cs
        add     ax, di                  ; expected: link-time i plus load segment
        cmp     ax, [si]
        jne     .bad_slot
        add     si, 2
        inc     di
        cmp     di, NRELOC
        jb      .check
        cmp     word [canary - image_start], 0x1234
        jne     .bad_canary
        mov     ax, 0x4c2a              ; exit 42: pass
        int     0x21
.bad_slot:
        mov     ax, 0x4c01              ; exit 1: a fixup is missing or doubled
        int     0x21
.bad_canary:
        mov     ax, 0x4c02              ; exit 2: an unlisted word was patched
        int     0x21

canary: dw 0x1234
slots:
%assign i 0
%rep NRELOC
        dw i
%assign i i + 1
%endrep
        times 128 db 0                  ; startup stack
        times (16 - (($ - image_start) % 16)) % 16 db 0
stack_top equ $ - image_start
file_size equ $ - header_start
