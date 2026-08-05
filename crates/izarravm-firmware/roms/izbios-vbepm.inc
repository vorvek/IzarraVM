; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; izbios-vbepm.inc - the VBE 2.0 protected-mode interface block returned by
; INT 10h AX=4F0Ah, BL=00h.
;
; WHY THIS IS REAL CODE. Every other VBE function in this machine is host-side
; HLE reached through INT 10h. Function 4F0Ah cannot be: it hands the client the
; ADDRESS of three routines that the client then far-calls directly, with no
; interrupt involved. That is the entire point of the function -- a protected-
; mode client asks for it precisely so it can bank and flip without paying a
; v86 round trip per call. So these three routines have to be genuine x86 that
; pokes genuine device registers.
;
; That is what Margo's extension register file at MARGO_EXT_INDEX/MARGO_EXT_DATA
; exists for. The registers ALIAS the state the INT 10h path owns, so a client
; that sets its mode through INT 10h and then banks through this block is
; talking to one window, not two.
;
; THE CLIENT COPIES THIS BLOCK. Everything here must be position independent:
; relative CALL and Jcc only, no absolute references to anything outside the
; block, and no assumption about which selector it is running under.
;
; KEEP THE PORT AND INDEX NUMBERS IN SYNC with `MARGO_EXT_*` in
; crates/izarravm-machine/src/vega.rs. `vbe_pm_stub_drives_the_margo_registers`
; executes this assembled block against that decode, so a drift fails a test
; instead of shipping a stub that writes nowhere.

VBEPM_EXT_INDEX     equ 0x03cb
VBEPM_EXT_DATA      equ 0x03cd
VBEPM_SEGSEL_LO     equ 0x00
VBEPM_SEGSEL_HI     equ 0x01
VBEPM_DISPX_LO      equ 0x02
VBEPM_DISPX_HI      equ 0x03
VBEPM_DISPY_LO      equ 0x04
VBEPM_DISPY_HI      equ 0x05
VBEPM_DISPCTL       equ 0x06
VBEPM_DAC_WINDEX    equ 0x03c8
VBEPM_DAC_DATA      equ 0x03c9

; CX for 4F0Ah, emitted immediately BEFORE the block so the host can read the
; length out of the ROM instead of carrying a second copy of it in Rust that
; would silently rot the next time a routine here grows. The client is handed
; vbe_pm_block, so these two bytes are invisible to it.
    dw  vbe_pm_block_end - vbe_pm_block

vbe_pm_block:
    dw  vbe_pm_setwindow   - vbe_pm_block
    dw  vbe_pm_setdisplay  - vbe_pm_block
    dw  vbe_pm_setpalette  - vbe_pm_block
    dw  vbe_pm_portlist    - vbe_pm_block

; ---- helpers -------------------------------------------------------------
; Both clobber AX and preserve everything else, which is what lets the callers
; below keep CX and DX live across a whole register sequence.

; AL = extension register index, AH = value to store.
vbe_pm_ext_write:
    push    dx
    push    ax
    mov     dx, VBEPM_EXT_INDEX
    out     dx, al                  ; AL is the index
    pop     ax                      ; AL = index again, AH = value
    mov     dx, VBEPM_EXT_DATA
    mov     al, ah
    out     dx, al
    pop     dx
    ret

; AL = extension register index, returns the value in AL.
vbe_pm_ext_read:
    push    dx
    mov     dx, VBEPM_EXT_INDEX
    out     dx, al
    mov     dx, VBEPM_EXT_DATA
    in      al, dx
    pop     dx
    ret

; ---- 4F05h: Set/Get Logical Window ---------------------------------------
; BH = 00h set / 01h get, BL = window (only A exists), DX = window number in
; window-granularity units. Same inputs as the INT 10h function, minus AX.
vbe_pm_setwindow:
    or      bl, bl
    jnz     .done                   ; window B is absent: leave DX untouched
    cmp     bh, 0x01
    je      .get
    push    ax
    mov     al, VBEPM_SEGSEL_LO
    mov     ah, dl
    call    vbe_pm_ext_write
    mov     al, VBEPM_SEGSEL_HI
    mov     ah, dh
    call    vbe_pm_ext_write
    pop     ax
.done:
    retf
.get:
    push    ax
    mov     al, VBEPM_SEGSEL_LO
    call    vbe_pm_ext_read
    mov     dl, al
    mov     al, VBEPM_SEGSEL_HI
    call    vbe_pm_ext_read
    mov     dh, al
    pop     ax
    retf

; ---- 4F07h: Set Display Start --------------------------------------------
; BL = 00h now / 80h at the next vertical retrace, CX = first displayed pixel
; in the scan line, DX = first displayed scan line.
;
; The pixel-to-byte arithmetic stays on the device: it needs the active mode's
; pitch and depth, and duplicating that here would be a second copy of a
; calculation INT 10h already owns. DISPCTL latches what DISPX/DISPY hold.
vbe_pm_setdisplay:
    push    ax
    push    cx
    push    dx
    mov     al, VBEPM_DISPX_LO
    mov     ah, cl
    call    vbe_pm_ext_write
    mov     al, VBEPM_DISPX_HI
    mov     ah, ch
    call    vbe_pm_ext_write
    mov     al, VBEPM_DISPY_LO
    mov     ah, dl
    call    vbe_pm_ext_write
    mov     al, VBEPM_DISPY_HI
    mov     ah, dh
    call    vbe_pm_ext_write
    ; Carry the caller's retrace request into the register even though the
    ; latch applies at the frame boundary either way, so the device sees the
    ; request the client actually made.
    mov     ah, 0x01
    test    bl, 0x80
    jz      .strobe
    or      ah, 0x80
.strobe:
    mov     al, VBEPM_DISPCTL
    call    vbe_pm_ext_write
    pop     dx
    pop     cx
    pop     ax
    retf

; ---- 4F09h: Set Primary Palette Data -------------------------------------
; BL = 00h now / 80h at the next vertical retrace, CX = entry count,
; DX = first entry, ES:DI = table of 4-byte entries.
;
; Entry order is blue, green, red, alignment -- the same order the INT 10h
; path reads (see `vbe_palette_data`), and the reverse of the DAC's own
; red-green-blue write order, which is why the loads below are not sequential.
vbe_pm_setpalette:
    push    ax
    push    cx
    push    dx
    push    di
    jcxz    .done
    mov     ax, dx
    mov     dx, VBEPM_DAC_WINDEX
    out     dx, al                  ; first entry index
    inc     dx                      ; VBEPM_DAC_DATA
.entry:
    mov     al, [es:di + 2]         ; red
    out     dx, al
    mov     al, [es:di + 1]         ; green
    out     dx, al
    mov     al, [es:di]             ; blue
    out     dx, al
    add     di, 4                   ; the fourth byte is alignment padding
    loop    .entry
.done:
    pop     di
    pop     dx
    pop     cx
    pop     ax
    retf

; ---- the port and memory list --------------------------------------------
; A DPMI client reads this to learn which locations the routines above touch,
; so it can grant I/O access to exactly those and no more. Word-sized port
; numbers terminated by FFFFh, then the memory-location list -- empty here,
; because every register these routines reach is an I/O port -- terminated by
; its own FFFFh.
vbe_pm_portlist:
    dw  VBEPM_EXT_INDEX
    dw  VBEPM_EXT_DATA
    dw  VBEPM_DAC_WINDEX
    dw  VBEPM_DAC_DATA
    dw  0xffff
    dw  0xffff
vbe_pm_block_end:

; OemStringPtr's target, placed immediately after the block so the host can find
; it at (block + length) without a second fixed offset to keep in step. It is
; OUTSIDE the block, so a client that copies CX bytes does not carry it along --
; which is correct, because the pointer it is given is a ROM address.
vbe_oem_string:
    db  "Izarra 3000 VEGA/Margo VBE", 0
