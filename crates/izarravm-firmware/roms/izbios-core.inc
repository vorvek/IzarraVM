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
    xor ax, ax
    mov ds, ax                  ; DS = 0 for the IVT and the result block

    ; Install IVT[09h] -> int09, IVT[16h] -> int16 (CS = ROM_SEG).
    mov word [0x09 * 4], int09
    mov word [0x09 * 4 + 2], ROM_SEG
    mov word [0x16 * 4], int16
    mov word [0x16 * 4 + 2], ROM_SEG

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
    ; Unmask IRQ1 (keyboard); leave IRQ0 (timer) masked - no INT 08h handler yet.
    in al, PORT_PIC_DATA
    and al, 0xfd
    out PORT_PIC_DATA, al

    call unreal_init            ; widen FS to 4 GB for peek32/poke32/read_margo_reg
    call result_init           ; lay the empty VDTS header
    sti
    call run_post              ; walk the POST step table, append each record
    call setup_maybe_enter     ; STREAM D setup-page hook (foundation stub: ret)
    jmp enter_setup_or_boot

; --- POST sequencer ---------------------------------------------------------
; Walk post_step_table (emitted last by izbios-tables.inc). Each entry is
; dw name_ptr, dw handler_ptr. Append a BEGIN suite record, then for each step
; call the handler (DS = BDA on entry) and append its result by name.
run_post:
    call console_init           ; mode 13h boot console + title (the main window)
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
    call console_step_line      ; draw the boot-log line (status AL, value DX)
    inc al                      ; map HRES_* (0/1/2) -> RESULT_* (1/2/3)
    call result_append
    pop bx
    add bx, 4
    jmp .next
.done:
    ret

; --- Boot / halt tail. The boot console is already on screen from run_post, so
; append a final ready line at the next row rather than clearing it. --
enter_setup_or_boot:
    xor ax, ax
    mov ds, ax
    mov ax, [CONSOLE_ROW_Y]     ; next free console row
    add ax, STATUS_LINE_DY      ; leave one blank line above the message
    mov di, STATUS_X
    push cs
    pop ds                      ; DS = CS so DS:SI reaches the ROM string
    mov si, str_ready
    mov bl, COLOR_TITLE
    mov bh, COLOR_BG
    call gfx_text
    ; POST is complete and there is no OS to boot yet, so keep the machine running
    ; in an idle loop rather than halting. Halting would read as the self test
    ; powering the computer off; the machine should stay on and ready. Interrupts
    ; stay enabled so the keyboard path keeps working.
    sti
.idle:
    jmp .idle
str_ready: db 'SYSTEM READY', 0

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
