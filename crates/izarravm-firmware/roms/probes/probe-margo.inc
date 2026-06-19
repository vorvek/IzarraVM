; probes/probe-margo.inc - STREAM C3: the Margo (VEGA 2D) probe.
; Two POST steps: component.video_margo (PASS/FAIL on the identity register) and
; video.margo_caps (MEASURE of the capability bits). Both read Margo's 32-bit
; register block through read_margo_reg (BX = register offset, EAX = the dword at
; MARGO_MMIO_BASE + offset via the unreal FS segment).
;
; Register map (crates/izarravm-video/src/margo.rs):
;   0x00 ID   -> 0x4D470100  ('M','G', version 1.00)
;   0x04 CAPS -> 0x00000fff  (twelve feature bits, low 16 reported here)

MARGO_REG_ID   equ 0x00
MARGO_REG_CAPS equ 0x04
MARGO_ID       equ 0x4d470100

; --- component.video_margo: PASS when the identity register matches. ---
POST_STEP "component.video_margo", probe_margo
probe_margo:
    mov bx, MARGO_REG_ID
    call read_margo_reg         ; EAX = identity dword
    cmp eax, MARGO_ID
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret

; --- video.margo_caps: MEASURE the low 16 capability bits as 4 hex digits. ---
; Reads CAPS, formats its low word into PROBE_SCRATCH as a fixed-width lowercase
; hex ASCIIZ, and returns SI = that segment-0 offset (HRES_MEASURE). DS is 0x0040
; on entry, so we point it at segment 0 to reach the scratch buffer. The stores
; go through DI register-indirect (not a direct memory offset) to match the
; foundation's gfx_puthex encoding.
POST_STEP "video.margo_caps", probe_margo_caps
probe_margo_caps:
    mov bx, MARGO_REG_CAPS
    call read_margo_reg         ; EAX = caps dword; AX = low 16 bits
    xor cx, cx
    mov ds, cx                  ; DS = 0 (PROBE_SCRATCH lives in segment 0)
    mov di, PROBE_SCRATCH
    mov bp, ax                  ; value to format
    mov cx, 4
.nib:
    rol bp, 4                   ; bring the next high nibble into bits 3-0
    mov ax, bp
    and al, 0x0f
    cmp al, 10
    jb .dec
    add al, 'a' - 10
    jmp .store
.dec:
    add al, '0'
.store:
    mov [di], al
    inc di
    loop .nib
    mov byte [di], 0            ; ASCIIZ terminator
    mov si, PROBE_SCRATCH       ; SI = segment-0 offset of the value string
    mov al, HRES_MEASURE
    ret
