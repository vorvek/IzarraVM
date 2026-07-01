; v86spike.asm — SP-4b M0 Task 2 standalone V86 spike.
;
; Increment 1: boot (real mode) -> build GDT/IDT/TSS + identity page tables in RAM
; -> enter protected mode + paging -> IRETD into a Virtual-8086 stub -> the stub
; signals exit code 0xA5 through the unit-tester port (OUT is permitted by the
; all-zero TSS I/O bitmap, so no #GP monitor is needed yet). This de-risks the
; real-mode -> PM -> paging -> IRETD-into-V86 transition in isolation.
;
; Physical memory map (identity-paged, all < 1 MiB):
;   0x01000 page directory      (PDE[0] -> PT)
;   0x02000 page table 0        (256 identity PTEs = maps 0..1 MiB)
;   0x03000 GDT                 (null, ring0 code 0x08, ring0 data 0x10, TSS 0x18)
;   0x04000 IDT                 (empty in increment 1 — no faults expected)
;   0x05000 TSS                 (SS0:ESP0 + all-zero I/O bitmap)
;   0x06000 ring-0 stack top    (ESP0 = 0x6000, grows down)
;   0x07c00 this boot code + the V86 stub
;
; Built with: nasm -f bin v86spike.asm -o v86spike.bin
cpu 386
bits 16
org 0x7c00

start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7000

    ; --- zero the low structure area 0x1000..0x6000 ---
    mov di, 0x1000
    mov cx, (0x6000 - 0x1000) / 2
    xor ax, ax
    rep stosw

    ; --- page directory @ 0x1000: PDE[0] -> PT @ 0x2000, present+rw+user ---
    mov dword [0x1000], 0x2000 | 7

    ; --- page table @ 0x2000: 256 identity entries (0..1 MiB), present+rw+user ---
    mov di, 0x2000
    mov eax, 0x0000_0007
    mov cx, 256
.fill_pt:
    mov [di], eax
    add eax, 0x1000
    add di, 4
    loop .fill_pt

    ; --- GDT @ 0x3000 ---
    ; [0x08] ring-0 code, 32-bit, base 0 limit 4G  = 0x00CF9B000000FFFF
    mov dword [0x3008], 0x0000FFFF
    mov dword [0x300C], 0x00CF9B00
    ; [0x10] ring-0 data, base 0 limit 4G          = 0x00CF93000000FFFF
    mov dword [0x3010], 0x0000FFFF
    mov dword [0x3014], 0x00CF9300
    ; [0x18] TSS, base 0x5000, limit 0x0088, access 0x89 (present 32-bit TSS)
    ;   low  = base[15:0]<<16 | limit[15:0]      = 0x5000_0088
    ;   high = base[31:24]<<24 | 0x00_89_00 | base[23:16]
    mov dword [0x3018], 0x50000088
    mov dword [0x301C], 0x00008900

    ; --- TSS @ 0x5000 (already zeroed): ESP0, SS0, I/O-map base ---
    mov dword [0x5004], 0x6000     ; ESP0
    mov word  [0x5008], 0x0010     ; SS0 = ring-0 data selector
    mov word  [0x5066], 0x0068     ; I/O-map base = 0x68 (bitmap follows, all zero)
    ; bitmap 0x5068..0x5088 stays zero (all ports permitted); TSS limit 0x88 covers it

    ; --- load descriptor tables ---
    lgdt [gdtr]
    lidt [idtr]

    ; --- enable protected mode + paging ---
    mov eax, 0x1000
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80000001            ; CR0.PG | CR0.PE
    mov cr0, eax
    jmp dword 0x08:pm_entry

bits 32
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x6000
    mov ax, 0x18
    ltr ax

    ; --- build the V86 IRET frame and drop into the stub ---
    ; iretd (from CPL0 with popped EFLAGS.VM=1) pops EIP,CS,EFLAGS,ESP,SS,ES,DS,FS,GS,
    ; so push them high-to-low: GS,FS,DS,ES,SS,ESP,EFLAGS,CS,EIP.
    push dword 0                  ; GS
    push dword 0                  ; FS
    push dword 0                  ; DS
    push dword 0                  ; ES
    push dword 0                  ; SS  (V86 stub uses no stack)
    push dword 0x1000             ; ESP (unused)
    push dword 0x00020002         ; EFLAGS: VM (0x20000) | reserved bit1, IOPL 0
    push dword 0                  ; CS = 0 (base 0)
    push dword v86_stub           ; EIP = linear address of the stub (CS base 0)
    iretd

bits 16
v86_stub:
    ; Now executing in V86. Signal success through the unit-tester exit port.
    ; OUT consults the TSS I/O bitmap (all zero -> permitted), so no #GP here.
    mov al, 12                    ; REG_EXIT index
    out 0xE4, al
    mov al, 0xA5                  ; exit code
    out 0xE5, al
    mov al, 3                     ; CMD_EXIT -> machine stops with TestExit{0xA5}
    out 0xE6, al
.hang:
    jmp .hang

gdtr:
    dw 0x1F                       ; 4 descriptors * 8 - 1
    dd 0x3000
idtr:
    dw 0                          ; empty IDT (no faults expected in increment 1)
    dd 0x4000

    times 510 - ($ - $$) db 0
    dw 0xAA55
