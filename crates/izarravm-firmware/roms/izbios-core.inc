; izbios-core.inc - foundation: reset bring-up, PIC init, the POST sequencer,
; the >1 MiB / MMIO access helpers, and the boot/halt tail. Reset enters at
; bios_start (the skeleton's reset: at offset 0 jumps here).

; --- Bring-up --------------------------------------------------------------
bios_start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, STACK_TOP

    ; BDA keyboard ring: head = tail = start, flags = 0.
    mov ax, BDA_SEG
    mov ds, ax
    mov word [KB_BUF_HEAD], KB_BUF_START
    mov word [KB_BUF_TAIL], KB_BUF_START
    mov byte [KB_FLAGS], 0
    ; Cache the keyboard layout from CMOS 0x10 into the BDA so the INT 09h ISR
    ; translates against the right table. Clamp out-of-range values to en-US (0).
    ; The setup page refreshes this byte live when it changes the layout.
    mov al, KB_CMOS_LAYOUT
    out 0x70, al
    in al, 0x71
    cmp al, 17                  ; 17 imported layouts (0..16)
    jb .layout_ok
    xor al, al
.layout_ok:
    mov [KB_LAYOUT], al
    xor ax, ax
    mov ds, ax                  ; DS = 0 for the IVT and the result block
    ; Seed the boot-device choice from CMOS 0x11 (boot order: 0 floppy-first,
    ; 1 disk-first). The Tab boot menu re-reads this for its default highlight; the
    ; copy here is what boot_entry uses when no menu runs. Clamp to floppy.
    mov al, BOOT_CMOS_ORDER
    out 0x70, al
    in al, 0x71
    cmp al, 1
    jbe .order_ok
    xor al, al
.order_ok:
    mov [BOOT_CHOICE], al
    mov byte [POST_HOTKEY], 0

    ; Install the interrupt vectors (CS = ROM_SEG for every entry):
    ;   INT 08h -> int08   timer tick (IRQ0), maintains the BDA tick count
    ;   INT 09h -> int09   keyboard ISR (IRQ1)
    ;   INT 16h -> int16   keyboard services
    ;   INT 1Ch -> int1c   user timer-tick hook, a bare IRET by default
    ; Then point every remaining hardware-IRQ slot at a stub that just sends EOI
    ; and returns. The RAM count-up and the chime drive PIT channel 0 to pace on,
    ; so IRQ0 fires during POST; without these handlers a hardware interrupt would
    ; vector through a zeroed IVT entry (CS=0000) and run away through low memory.
    mov word [0x08 * 4], int08
    mov word [0x08 * 4 + 2], ROM_SEG
    mov word [0x09 * 4], int09
    mov word [0x09 * 4 + 2], ROM_SEG
    mov word [0x16 * 4], int16
    mov word [0x16 * 4 + 2], ROM_SEG
    mov word [0x1c * 4], int1c
    mov word [0x1c * 4 + 2], ROM_SEG
    ; Master IRQ2..7 -> INT 0Ah..0Fh: spurious stub that EOIs the master PIC.
    mov cx, 6
    mov di, 0x0a * 4
.fill_master:
    mov word [di], irq_stub_master
    mov word [di + 2], ROM_SEG
    add di, 4
    loop .fill_master
    ; Slave IRQ8..15 -> INT 70h..77h: spurious stub that EOIs both PICs.
    mov cx, 8
    mov di, 0x70 * 4
.fill_slave:
    mov word [di], irq_stub_slave
    mov word [di + 2], ROM_SEG
    add di, 4
    loop .fill_slave

    ; Give IRQ12 (INT 74h) a real PS/2 mouse ISR; the other slave IRQs keep the
    ; EOI-only stub. DS is 0 here, so these stores land in IVT segment 0.
    mov word [0x74 * 4], int74
    mov word [0x74 * 4 + 2], ROM_SEG

    ; 8259 PIC init (verbatim from kbd-bios): IRQ0..7 -> INT 08h..0Fh.
    mov al, 0x11
    out PORT_PIC_CMD, al
    out PORT_PIC2_CMD, al
    mov al, 0x08
    out PORT_PIC_DATA, al
    mov al, 0x70
    out PORT_PIC2_DATA, al
    mov al, 0x04
    out PORT_PIC_DATA, al
    mov al, 0x02
    out PORT_PIC2_DATA, al
    mov al, 0x01
    out PORT_PIC_DATA, al
    out PORT_PIC2_DATA, al
    ; Unmask IRQ0 (timer) and IRQ1 (keyboard) on the master; the timer ISR is now
    ; installed, so the IRQ0 edges raised while pacing the count-up land on int08.
    in al, PORT_PIC_DATA
    and al, 0xf8                 ; unmask IRQ0 timer, IRQ1 keyboard, IRQ2 cascade
    out PORT_PIC_DATA, al
    ; Unmask slave IRQ12 (the PS/2 mouse) so INT 74h fires; the slave reaches the
    ; CPU through the master cascade on IR2, unmasked just above.
    in al, PORT_PIC2_DATA
    and al, 0xef                 ; clear bit 4 (IRQ12)
    out PORT_PIC2_DATA, al

    call unreal_init            ; widen FS to 4 GB for peek32/poke32/read_margo_reg
    call result_init           ; lay the empty VDTS header
    sti
    call run_post              ; walk the POST step table, append each record
    call setup_maybe_enter     ; STREAM D setup-page hook (foundation stub: ret)
    ; Load the boot code-page font (CMOS 0x13) into the VGA character generator.
    ; This runs after POST so the BDA character height (0040:0085) is settled and
    ; the host has seeded the font tables for the default 8x16 text mode.
    call boot_load_codepage
    jmp enter_setup_or_boot

; --- Hardware-interrupt service routines ------------------------------------
; INT 08h: IRQ0 timer tick. Increment the BDA tick count (a dword at 0040:006C),
; rolling over after 24 hours (0x1800B0 ticks ~= 18.2 Hz * 86400 s) and setting
; the rollover flag at 0040:0070, then chain INT 1Ch (the user tick hook) and
; send a non-specific EOI to the master PIC. The standard PC timer-tick contract,
; here mainly so the count-up and chime can pace on PIT channel 0 without the IRQ
; vectoring through a null IVT entry; DOS and games rely on this tick later too.
int08:
    push ax
    push ds
    mov ax, BDA_SEG
    mov ds, ax
    add word [BDA_TICKS], 1
    adc word [BDA_TICKS + 2], 0
    ; Rollover at 1,573,040 ticks (one day): zero the count and flag the wrap.
    cmp word [BDA_TICKS + 2], (BDA_TICK_ROLLOVER >> 16)
    jb .no_rollover
    cmp word [BDA_TICKS], (BDA_TICK_ROLLOVER & 0xffff)
    jb .no_rollover
    mov word [BDA_TICKS], 0
    mov word [BDA_TICKS + 2], 0
    mov byte [BDA_TICK_FLAG], 1
.no_rollover:
    int 0x1c                     ; user timer-tick hook (default: a bare IRET)
    mov al, 0x20
    out PORT_PIC_CMD, al         ; non-specific EOI to the master PIC
    pop ds
    pop ax
    iret

; INT 1Ch: the user timer-tick hook. The BIOS default is a bare IRET; software
; that wants a per-tick callback repoints this vector.
int1c:
    iret

; irq_stub_master: spurious master-PIC interrupt (IRQ2..7 with no installed
; handler). Send the master EOI so the line does not stay in service, then IRET.
irq_stub_master:
    push ax
    mov al, 0x20
    out PORT_PIC_CMD, al
    pop ax
    iret

; irq_stub_slave: spurious slave-PIC interrupt (IRQ8..15). EOI both the slave and
; the master (the cascade owes a master EOI on IR2), then IRET.
irq_stub_slave:
    push ax
    mov al, 0x20
    out PORT_PIC2_CMD, al        ; EOI the slave first
    out PORT_PIC_CMD, al         ; then the master cascade line (IR2)
    pop ax
    iret

; int74: IRQ12 (PS/2 mouse). Reads exactly ONE aux byte per interrupt (the model
; re-latches the next aux byte on each 0x60 read, so a drain loop would desync the
; 3-byte assembly), stores it in the EBDA packet buffer, and on the third byte
; far-calls the INT 15h C207-registered handler with the 4-word PS/2 frame.
int74:
    push ax
    push bx
    push si
    push ds
    mov ax, EBDA_SEG
    mov ds, ax
    in al, PORT_KBD_CMD              ; 0x64 status
    test al, KBC_STATUS_OBF
    jz .eoi                          ; nothing waiting
    test al, KBC_STATUS_AUX
    jnz .aux
    in al, PORT_KBD_DATA             ; a keyboard byte: drain to clear OBF, ignore
    jmp .eoi
.aux:
    in al, PORT_KBD_DATA             ; one aux byte
    mov bl, [EBDA_MOUSE_IDX]
    xor bh, bh
    mov si, EBDA_MOUSE_PKT
    add si, bx
    mov [si], al                     ; packet[index] = byte
    inc bl
    cmp bl, 3
    jb .save_idx
    mov byte [EBDA_MOUSE_IDX], 0     ; packet complete, reset index
    call int74_dispatch
    jmp .eoi
.save_idx:
    mov [EBDA_MOUSE_IDX], bl
.eoi:
    mov al, 0x20
    out PORT_PIC2_CMD, al            ; EOI slave
    out PORT_PIC_CMD, al             ; EOI master cascade (IR2)
    pop ds
    pop si
    pop bx
    pop ax
    iret

; int74_dispatch: build the RBIL 4-word frame from EBDA_MOUSE_PKT and far-call the
; registered handler if non-null. DS = EBDA_SEG on entry. Clobbers ax,bx,cx,dx.
int74_dispatch:
    mov ax, [EBDA_MOUSE_HANDLER]     ; offset
    mov dx, [EBDA_MOUSE_HANDLER + 2] ; segment
    mov cx, ax
    or cx, dx
    jz .done                         ; null handler, nothing to call
    ; WORD1 status = packet byte 0 (bit0 left, bit1 right, bit3 reserved, bit4/5
    ; X/Y sign), high byte 0.
    mov al, [EBDA_MOUSE_PKT]
    xor ah, ah
    push ax
    ; WORD2 X = packet byte 1, high byte 0 (sign carried in status bit4).
    mov al, [EBDA_MOUSE_PKT + 1]
    xor ah, ah
    push ax
    ; WORD3 Y = packet byte 2, high byte 0 (sign carried in status bit5).
    mov al, [EBDA_MOUSE_PKT + 2]
    xor ah, ah
    push ax
    ; WORD4 = 0.
    xor ax, ax
    push ax
    call far [EBDA_MOUSE_HANDLER]    ; indirect far call through DS:EBDA_MOUSE_HANDLER
    add sp, 8                        ; drop the four words after the handler RETF
.done:
    ret

; --- POST sequencer ---------------------------------------------------------
; Walk post_step_table (emitted last by izbios-tables.inc). Each entry is
; dw name_ptr, dw handler_ptr. Append a BEGIN suite record, then for each step
; call the handler (DS = BDA on entry) and append its result by name.
run_post:
    ; No power-on chime: Toka-DOS will own the startup sound (future work). The
    ; chime used to program PIT channel 0 for the POST's real-time pacing, so do it
    ; here instead: mode 2, reload 0 = 65536 = ~18.2 Hz, the standard timer tick that
    ; post_step_pace and the RAM count-up poll (and that IRQ0/INT 08h runs on).
    mov al, 0x34
    out PORT_PIT_CTRL, al
    xor al, al
    out PORT_PIT_CH0, al
    out PORT_PIT_CH0, al
    call console_init           ; paint the graphical POST screen
    call com1_init              ; bring up COM1 and write the POST log header
    mov al, RESULT_BEGIN
    mov si, str_suite_name
    call result_append          ; BEGIN suite.izarra
    xor bx, bx                  ; byte index into the table
.next:
    cmp bx, post_step_count * 4
    jae .done
    push bx
    mov si, [cs:post_step_table + bx]   ; name pointer (ROM)
    push si
    mov ax, BDA_SEG
    mov ds, ax                  ; handler entry contract: DS = BDA
    call word [cs:post_step_table + bx + 2]
    mov dx, si                  ; MEASURE value offset (segment 0), if any
    xor cx, cx
    mov ds, cx                  ; DS = 0 for result_append
    pop si                      ; name pointer
    call console_step_line      ; light this component's icon grey->colour on PASS
    call post_step_pace         ; brief real-time pause so the reveal is visible
    call com1_log_step          ; mirror the line to COM1 (status AL, name SI)
    inc al                      ; map HRES_* (0/1/2) -> RESULT_* (1/2/3)
    call result_append
    pop bx
    add bx, 4
    jmp .next
.done:
    ret

; post_step_pace: under slow POST, pause ~110 ms (two PIT reloads) after a step so
; each component's icon lights visibly in sequence rather than all at once in a
; single frame. Skipped under fast POST (headless/tests). chime_pit_wait is
; wall-time paced (PIT channel 0), so the cadence is the same at any CPU clock.
; Preserves all registers (the sequencer reuses AL/SI after this call).
post_step_pace:
    push ax
    call su_poll_post_hotkeys
    in al, PORT_FAST_POST
    test al, 1
    jnz .done                   ; fast POST: no cosmetic delay
    call chime_pit_wait
    call chime_pit_wait
    call su_poll_post_hotkeys
.done:
    pop ax
    ret

; --- Boot / halt tail. The graphical POST screen is already on the LFB from
; run_post, so draw the handoff line on it (lfb_text), then hand to INT 19h. --
enter_setup_or_boot:
    push cs
    pop ds                      ; DS = CS so DS:SI reaches the ROM string
    mov si, str_post_toka
    mov di, ART_HDR_X
    mov ax, ART_TOKA_Y
    mov bl, ART_INK_INDEX
    mov bh, ART_FIELD_INDEX
    call lfb_text
    ; POST is complete: hand off to the INT 19h bootstrap. If a bootable floppy is
    ; mounted, boot_entry far jumps into its boot sector and never returns. With no
    ; media (or a read error) it returns here and the machine idles rather than
    ; halting, so the self test does not read as powering the computer off.
    ; Interrupts stay enabled so the keyboard path keeps working.
    sti
    call boot_entry
.idle:
    jmp .idle
str_post_toka: db 'Starting Toka-DOS...', 0

; --- COM1 POST log mirror ---------------------------------------------------
; The graphical screen and the serial console carry the same POST progress. The
; emulator captures bytes written to the COM1 transmit register (0x3F8, DLAB
; clear) and pins the line status register to "transmitter empty", so the poll
; loop never stalls. Init programs the standard 8N1, 9600-baud setup once.
COM1_THR      equ PORT_COM1_BASE          ; transmit holding register (DLAB=0)
COM1_DLL      equ PORT_COM1_BASE          ; divisor latch low (DLAB=1)
COM1_DLM      equ PORT_COM1_BASE + 1      ; divisor latch high (DLAB=1)
COM1_LCR      equ PORT_COM1_BASE + 3      ; line control register
COM1_THRE_BIT equ 0x20                    ; LSR bit 5: transmit holding empty

; com1_init: set 8N1 at 9600 baud (divisor 12 for a 1.8432 MHz UART clock).
; Then write a one-line header so a log reader knows where POST starts.
; Preserves all caller registers.
com1_init:
    push ax
    push dx
    push ds
    mov dx, COM1_LCR
    mov al, 0x80                 ; DLAB = 1: expose the divisor latch
    out dx, al
    mov dx, COM1_DLL
    mov al, 12                   ; 1843200 / (16 * 9600) = 12
    out dx, al
    mov dx, COM1_DLM
    xor al, al
    out dx, al
    mov dx, COM1_LCR
    mov al, 0x03                 ; DLAB = 0, 8 data bits, no parity, 1 stop bit
    out dx, al
    push cs
    pop ds                       ; DS = CS so DS:SI reaches the ROM string
    mov si, com1_str_header
    call com1_puts
    pop ds
    pop dx
    pop ax
    ret

; com1_log_step: write "<STATUS> <name>" + CRLF to COM1.
;   AL = HRES_* status (0 PASS, 1 FAIL, 2 MEASURE), SI = CS-relative name ptr.
; Enter with DS = 0 (the sequencer's contract before result_append). Reads the
; prefix and the name from ROM via CS overrides. Preserves all caller registers.
com1_log_step:
    push ax
    push bx
    push si
    push ds
    push cs
    pop ds                       ; DS = CS for the prefix/name ROM strings
    push si                      ; save the name pointer
    movzx bx, al
    shl bx, 1
    mov si, [com1_prefix_table + bx]
    call com1_puts               ; "PASS " / "FAIL " / "MEASURE "
    pop si                       ; name pointer
    call com1_puts               ; the dotted step name
    ; MEASURE steps carry a value string in segment 0 at offset DX; append " <value>".
    cmp al, HRES_MEASURE
    jne .crlf
    mov si, com1_str_space
    call com1_puts               ; one space, still DS = CS
    mov si, dx                   ; SI = value offset in segment 0
    xor bx, bx
    mov ds, bx                   ; DS = 0 for the value string
    call com1_puts               ; the measured value (ASCIIZ)
    push cs
    pop ds                       ; restore DS = CS for the trailing CRLF
.crlf:
    mov si, com1_str_crlf
    call com1_puts
    pop ds
    pop si
    pop bx
    pop ax
    ret

; com1_puts: write the ASCIIZ string at DS:SI to COM1, polling LSR THRE before
; each byte. Stops at (does not send) the terminator. Preserves AX, SI, DX.
com1_puts:
    push ax
    push dx
    push si
.next:
    mov al, [si]
    inc si
    test al, al
    jz .done
    mov ah, al                   ; hold the data byte while polling the status
    mov dx, COM1_LCR + 2         ; LSR = base + 5
.wait:
    in al, dx
    test al, COM1_THRE_BIT
    jz .wait
    mov dx, COM1_THR
    mov al, ah
    out dx, al
    jmp .next
.done:
    pop si
    pop dx
    pop ax
    ret

com1_str_header: db 'Izarra 3000 POST', 0x0d, 0x0a, 0
com1_str_crlf:   db 0x0d, 0x0a, 0
com1_str_space:  db ' ', 0
; HRES_PASS=0, HRES_FAIL=1, HRES_MEASURE=2 select these prefixes.
com1_prefix_pass:    db 'PASS ', 0
com1_prefix_fail:    db 'FAIL ', 0
com1_prefix_measure: db 'MEASURE ', 0
com1_prefix_table:   dw com1_prefix_pass, com1_prefix_fail, com1_prefix_measure

; --- Unreal-mode flat access helpers ---------------------------------------
; unreal_init: enter protected mode briefly, widen FS to a 4 GB flat segment,
; return to real mode. FS keeps the cached 4 GB limit (base 0) and is never
; reloaded, so 32-bit [fs:ebx] reaches any physical address. Verified against
; this emulator before the foundation froze.
unreal_init:
    push ds
    push es
    push bx
    cli
    lgdt [cs:unreal_gdt_ptr]
    mov eax, cr0
    or al, 1
    mov cr0, eax
    mov bx, UNREAL_SEL
    mov fs, bx
    mov eax, cr0
    and al, 0xfe
    mov cr0, eax
    pop bx
    pop es
    pop ds
    ret

; ramtest_peek32: EBX = physical address -> EAX = dword at that address.
ramtest_peek32:
    mov eax, [fs:ebx]
    ret

; ramtest_poke32: EBX = physical address, EAX = dword to store.
ramtest_poke32:
    mov [fs:ebx], eax
    ret

; read_margo_reg: BX = Margo register offset -> EAX = 32-bit register value.
read_margo_reg:
    movzx ebx, bx
    add ebx, MARGO_MMIO_BASE
    mov eax, [fs:ebx]
    ret

align 8
unreal_gdt:
    dq 0                                  ; null descriptor
    dw 0xffff, 0x0000                     ; flat data: limit 0-15, base 0-15
    db 0x00, 0x92, 0xcf, 0x00             ; base 16-23, access, gran+limit16-19, base24-31
unreal_gdt_end:
unreal_gdt_ptr:
    dw unreal_gdt_end - unreal_gdt - 1
    dd unreal_gdt + (ROM_SEG << 4)        ; GDT linear base (ROM is at 0xF0000)
