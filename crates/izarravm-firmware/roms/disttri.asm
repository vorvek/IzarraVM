; disttri.asm — Distira slice 1 guest proof: hand-written x86 that finds the
; Distira 3D card via PCI configuration space, maps its BAR0 aperture,
; initializes the minimal SST-1 register state, and draws one flat-shaded
; triangle to the framebuffer, then signals completion and spins.
;
; This is deliberately Glide-free: it pokes the same SST registers real DOS
; Glide 2.x uses (direct triangleCMD register writes — see
; dev_docs/2026-07-02-distira-driver-plan.md section 1: the DOS/Voodoo1 Glide
; driver never uses the command FIFO, only direct register pokes), following
; the exact wire sequence already proven in
; crates/izarravm-machine/tests/distira.rs's distira_guest_* tests (which
; drive the device end-to-end from hand-assembled protected-mode x86 through
; a PCI-configured BAR), promoted here to a standalone buildable flat binary.
;
; Loaded as a full BIOS ROM image (`--bios disttri.bin`, or
; Machine::new(profile, DISTTRI_BIN)): the reset vector at the top of a
; 64 KiB image jumps into real-mode start, which enters flat 32-bit
; protected mode (same recipe as i386dx25-test.asm), then:
;
;   1. Scans PCI configuration mechanism 1 (ports 0xCF8/0xCFC) on bus 0,
;      device 0..31, function 0, looking for vendor 0x121a / device 0x0001
;      (3dfx Voodoo Graphics — see crates/izarravm-machine/src/lib.rs
;      DISTIRA_PCI_VENDOR_ID/DISTIRA_PCI_DEVICE_ID).
;   2. Programs BAR0 (config offset 0x10) with a chosen 32-bit base and sets
;      the memory-space-enable bit in the command register (offset 0x04).
;   3. Sets the Distira-native front-door width/height registers, then pokes
;      SST_fbzMode/vertex/color registers and SST_triangleCMD to rasterize a
;      flat-shaded triangle into the back buffer, and SST_swapbufferCMD to
;      present it.
;   4. Signals success through the Lotura unit-tester exit port
;      (0xE4 index / 0xE5 data / 0xE6 command, see
;      crates/izarravm-machine/src/unittester.rs) and halts.
;
; Build: nasm -f bin disttri.asm -o disttri.bin
bits 16
org 0

%define ROM_BASE 0x000f0000

; ---- PCI configuration mechanism 1 ----
%define PCI_ADDR_PORT 0x0cf8
%define PCI_DATA_PORT 0x0cfc
%define DISTIRA_VENDOR_DEVICE 0x0001_121a  ; device<<16 | vendor

; ---- Chosen BAR0 base for this program's own PCI scan/assign ----
; 0xE8000000 avoids 0xE0000000 (Margo's fixed 2D LFB, MARGO_LFB_BASE in
; crates/izarravm-machine/src/lib.rs) and 0xE1000000 (Distira's own
; power-on-default BAR, DISTIRA_MMIO_BASE), so this program's real PCI
; discovery+BAR-assignment path is exercised against a genuinely different
; address than either device's fixed decode window.
%define ASSIGNED_BAR 0xe800_0000
%define ASSIGNED_LFB_OFFSET 0x0040_0000

; ---- SST-1 register offsets (crates/izarravm-video/src/distira.rs) ----
%define SST_VERTEX_AX   0x008
%define SST_VERTEX_AY   0x00c
%define SST_VERTEX_BX   0x010
%define SST_VERTEX_BY   0x014
%define SST_VERTEX_CX   0x018
%define SST_VERTEX_CY   0x01c
%define SST_START_R     0x020
%define SST_START_G     0x024
%define SST_START_B     0x028
%define SST_START_A     0x030
%define SST_TRIANGLE_CMD 0x080
%define SST_FBZ_MODE    0x110
%define SST_LFB_MODE    0x114
%define SST_CLIP_LEFT_RIGHT   0x118
%define SST_CLIP_LOW_Y_HIGH_Y 0x11c
%define SST_SWAPBUFFER_CMD 0x128
%define SST_COLOR1      0x148

; ---- Distira-native front-door registers ----
%define DISTIRA_REG_FB_WIDTH  0xf020
%define DISTIRA_REG_FB_HEIGHT 0xf024

; ---- fbzMode bits ----
%define FBZ_RGB_WMASK 0x0200
%define FBZ_DRAW_BACK 0x4000

; ---- Unit-tester exit port protocol ----
%define UT_INDEX   0xe4
%define UT_DATA    0xe5
%define UT_COMMAND 0xe6
%define UT_REG_EXIT 12
%define UT_CMD_EXIT 3
%define EXIT_OK   0xa5
%define EXIT_NO_CARD 0xe1

start:
    cli
    cld
    mov ax, 0
    mov ss, ax
    mov sp, 0x9000
    ; DS = CS (0xF000 at reset) so [gdt_descriptor] resolves against this
    ; ROM's own segment rather than physical address 0: the reset-vector CS
    ; is 0xF000, so DS must match it for real-mode near memory operands to
    ; reach the GDT this same file defines (mirrors the working
    ; i386dx25-test.asm/protected_flat_rom precedent, both of which set DS
    ; to CS before their own lgdt).
    mov ax, cs
    mov ds, ax

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp dword 0x0008:(ROM_BASE + protected_entry)

align 8, db 0
gdt_start:
    dq 0x0000000000000000
    dq 0x00cf9a000000ffff      ; flat 32-bit code, base 0 limit 4G
    dq 0x00cf92000000ffff      ; flat 32-bit data, base 0 limit 4G
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd ROM_BASE + gdt_start

bits 32
protected_entry:
    mov ax, 0x0010
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov esp, 0x00090000

    ; ---- Scan PCI bus 0, devices 0..31, function 0 for the Distira card ----
    xor esi, esi                       ; esi = device number, 0..31
.scan_next:
    mov eax, 0x8000_0000
    mov ebx, esi
    shl ebx, 11
    or eax, ebx                        ; register 0 (vendor/device)
    mov dx, PCI_ADDR_PORT
    out dx, eax
    mov dx, PCI_DATA_PORT
    in eax, dx
    cmp eax, DISTIRA_VENDOR_DEVICE
    je .found
    inc esi
    cmp esi, 32
    jb .scan_next
    ; Not found: signal failure and halt.
    mov al, EXIT_NO_CARD
    jmp signal_exit

.found:
    ; ebx already holds (device << 11); rebuild the base select dword for
    ; this device's config registers as we program them.
    mov edi, ebx                       ; edi = device-select bits, reused below

    ; ---- Program BAR0 (config offset 0x10) with ASSIGNED_BAR ----
    mov eax, 0x8000_0010
    or eax, edi
    mov dx, PCI_ADDR_PORT
    out dx, eax
    mov eax, ASSIGNED_BAR
    mov dx, PCI_DATA_PORT
    out dx, eax

    ; ---- Enable memory space decode (command register, offset 0x04) ----
    mov eax, 0x8000_0004
    or eax, edi
    mov dx, PCI_ADDR_PORT
    out dx, eax
    mov eax, 0x0000_0002               ; bit1 = memory space enable
    mov dx, PCI_DATA_PORT
    out dx, eax

    ; ---- Set the Distira-native framebuffer size (4x4, tiny + deterministic) ----
    mov dword [ASSIGNED_BAR + DISTIRA_REG_FB_WIDTH], 4
    mov dword [ASSIGNED_BAR + DISTIRA_REG_FB_HEIGHT], 4

    ; ---- Clip to the full 4x4 frame ----
    mov dword [ASSIGNED_BAR + SST_CLIP_LEFT_RIGHT], 4
    mov dword [ASSIGNED_BAR + SST_CLIP_LOW_Y_HIGH_Y], 4

    ; ---- fbzMode: write RGB, draw into the back buffer ----
    mov dword [ASSIGNED_BAR + SST_FBZ_MODE], (FBZ_RGB_WMASK | FBZ_DRAW_BACK)

    ; ---- One flat-shaded triangle covering the whole 4x4 frame, solid green.
    ; Vertex registers are 12.4 fixed point (value << 4); color registers are
    ; 24-bit fixed point with a 12-bit fraction (value << 12), matching the
    ; conventions crates/izarravm-video/tests/distira.rs already exercises.
    mov dword [ASSIGNED_BAR + SST_VERTEX_AX], (0 << 4)
    mov dword [ASSIGNED_BAR + SST_VERTEX_AY], (0 << 4)
    mov dword [ASSIGNED_BAR + SST_VERTEX_BX], (4 << 4)
    mov dword [ASSIGNED_BAR + SST_VERTEX_BY], (0 << 4)
    mov dword [ASSIGNED_BAR + SST_VERTEX_CX], (0 << 4)
    mov dword [ASSIGNED_BAR + SST_VERTEX_CY], (4 << 4)
    mov dword [ASSIGNED_BAR + SST_START_R], (0 << 12)
    mov dword [ASSIGNED_BAR + SST_START_G], (0xff << 12)
    mov dword [ASSIGNED_BAR + SST_START_B], (0 << 12)
    mov dword [ASSIGNED_BAR + SST_START_A], (0xff << 12)
    mov dword [ASSIGNED_BAR + SST_TRIANGLE_CMD], 1

    ; ---- Present: swap back to front ----
    mov dword [ASSIGNED_BAR + SST_SWAPBUFFER_CMD], 1

    mov al, EXIT_OK

signal_exit:
    ; Unit-tester exit sequence: select REG_EXIT, write the code (also
    ; post-increments the index, harmless here), then CMD_EXIT.
    mov ah, al                         ; stash the exit code
    mov al, UT_REG_EXIT
    out UT_INDEX, al
    mov al, ah
    out UT_DATA, al
    mov al, UT_CMD_EXIT
    out UT_COMMAND, al
.spin:
    hlt
    jmp .spin

bits 16
times 0xfff0 - ($ - $$) db 0
reset_vector:
    jmp 0xf000:0x0000
times 0x10000 - ($ - $$) db 0
