bits 16
org 0x8000

; Lotura unit-tester ports.
PORT_INDEX    equ 0xE4
PORT_DATA     equ 0xE5
PORT_COMMAND  equ 0xE6
CMD_EXIT      equ 3

; Register-file offsets (must match unittester.rs).
REG_EXIT_OFF  equ 12
SEL_OFF       equ 16
RES_ITER_OFF  equ 17
RES_AUX_OFF   equ 21
RES_STAT_OFF  equ 25

; Classic BYTE Sieve: 8190 flags, primes counted as i+i+3.
SIEVE_SIZE    equ 8190
SIEVE_SEG     equ 0x2000
SIEVE_ITER    equ 40

; x87 escape-time Mandelbrot over a 48x32 grid.
MANDEL_COLS   equ 48
MANDEL_ROWS   equ 32
MANDEL_PIXELS equ 1536            ; 48 * 32
MANDEL_MAXIT  equ 64

start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7000

    mov al, SEL_OFF
    out PORT_INDEX, al
    in  al, PORT_DATA          ; al = selector
    cmp al, 1
    je  .sieve
    cmp al, 3
    je  .fpmandel

    ; selector 0 or unknown: empty baseline, iterations 0, aux 0.
    xor ax, ax
    xor bx, bx
    jmp report

.sieve:
    call sieve                 ; bx = prime count of the last pass
    mov ax, SIEVE_ITER         ; iterations
    jmp report

.fpmandel:
    call fp_mandel             ; bx = 16-bit checksum of all pixel iter counts
    mov ax, MANDEL_PIXELS      ; iterations = pixel count (1536)
    jmp report

; report: ax = iterations, bx = aux. Writes both as little-endian u32 (low word
; then a zero high word), a status byte, exit code 0, then CMD_EXIT.
report:
    mov cx, ax                 ; save iterations
    mov dx, bx                 ; save aux
    mov al, RES_ITER_OFF
    out PORT_INDEX, al         ; index now walks ITER, AUX, STATUS contiguously
    mov ax, cx
    call emit_u16_padded       ; iterations -> [17..21]
    mov ax, dx
    call emit_u16_padded       ; aux -> [21..25]
    mov al, 1
    out PORT_DATA, al          ; status -> [25]
    mov al, REG_EXIT_OFF
    out PORT_INDEX, al
    xor al, al
    out PORT_DATA, al          ; exit code 0
    mov al, CMD_EXIT
    out PORT_COMMAND, al
.hang:
    hlt
    jmp .hang

; emit_u16_padded: write ax as the low word of a u32, then a zero high word,
; advancing the device index by 4. Clobbers ax. Preserves cx and dx.
emit_u16_padded:
    out PORT_DATA, al
    mov al, ah
    out PORT_DATA, al
    xor al, al
    out PORT_DATA, al
    out PORT_DATA, al
    ret

; sieve: run SIEVE_ITER passes of the 8190 sieve. Returns bx = prime count of
; the last pass (1899 for a correct run). Uses es:di into SIEVE_SEG.
sieve:
    push bp
    mov bp, SIEVE_ITER
.pass:
    mov ax, SIEVE_SEG
    mov es, ax
    xor di, di
    mov cx, SIEVE_SIZE
    mov al, 1
    rep stosb                  ; flags[0..SIZE] = 1

    xor bx, bx                 ; count = 0
    xor si, si                 ; i = 0
.outer:
    cmp si, SIEVE_SIZE
    jae .pass_done
    mov al, [es:si]
    test al, al
    jz .next
    mov dx, si
    add dx, si
    add dx, 3                  ; prime = i + i + 3
    mov di, si
    add di, dx                 ; j = i + prime
.inner:
    cmp di, SIEVE_SIZE
    jae .counted
    mov byte [es:di], 0
    add di, dx
    jmp .inner
.counted:
    inc bx                     ; count++
.next:
    inc si
    jmp .outer
.pass_done:
    dec bp
    jnz .pass
    pop bp
    ret

; fp_mandel: x87 escape-time Mandelbrot over a 48x32 grid, maxiter 64. Returns
; bx = 16-bit wrapping sum of the per-pixel iteration counts. Uses only base
; x87 (no FCOMI/FCMOV) so it runs in both 486 and 586 mode. The complex values
; live in memory dwords; the x87 stack never holds more than two entries and is
; balanced on every path.
;
; GP register use: si = column c, di = row r, dx = per-pixel iter, bx = checksum.
fp_mandel:
    push bp
    finit                          ; reset the x87 stack to a known state
    xor bx, bx                     ; checksum = 0
    xor di, di                     ; r = 0
.row:
    cmp di, MANDEL_ROWS
    jae .done
    ; cy = -1.0 + 0.0625 * r
    mov [m_tmpw], di
    fild word [m_tmpw]
    fmul dword [c_step]
    fadd dword [c_ybase]
    fstp dword [m_cy]
    xor si, si                     ; c = 0
.col:
    cmp si, MANDEL_COLS
    jae .col_done
    ; cx = -2.0 + 0.0625 * c
    mov [m_tmpw], si
    fild word [m_tmpw]
    fmul dword [c_step]
    fadd dword [c_xbase]
    fstp dword [m_cx]
    ; zx = 0.0, zy = 0.0
    fldz
    fst  dword [m_zx]
    fstp dword [m_zy]
    xor dx, dx                     ; iter = 0
.iter:
    cmp dx, MANDEL_MAXIT
    jae .pixel_done
    ; zx2 = zx*zx ; zy2 = zy*zy
    fld  dword [m_zx]
    fmul st0, st0
    fstp dword [m_zx2]
    fld  dword [m_zy]
    fmul st0, st0
    fstp dword [m_zy2]
    ; if zx2 + zy2 > 4.0 break
    fld  dword [m_zx2]
    fadd dword [m_zy2]
    fcomp dword [c_four]           ; compare (zx2+zy2) to 4.0, pop it
    fnstsw ax
    sahf
    ja   .pixel_done               ; C0/C2/C3 -> above means magnitude^2 > 4
    ; zy = 2.0*zx*zy + cy
    fld  dword [c_two]
    fmul dword [m_zx]
    fmul dword [m_zy]
    fadd dword [m_cy]
    ; zx = zx2 - zy2 + cx  (compute before overwriting m_zy so old zy is used)
    fld  dword [m_zx2]
    fsub dword [m_zy2]
    fadd dword [m_cx]
    ; now store: st0 = new zx, st1 = new zy
    fstp dword [m_zx]
    fstp dword [m_zy]
    inc dx
    jmp .iter
.pixel_done:
    add bx, dx                     ; checksum += iter (16-bit wrapping)
    inc si
    jmp .col
.col_done:
    inc di
    jmp .row
.done:
    pop bp
    ret

; x87 constants as IEEE-754 single dwords.
c_step   dd 0.0625
c_xbase  dd -2.0
c_ybase  dd -1.0
c_two    dd 2.0
c_four   dd 4.0

; Mandelbrot scratch (single-precision complex values + a word for FILD).
m_tmpw   dw 0
m_cx     dd 0.0
m_cy     dd 0.0
m_zx     dd 0.0
m_zy     dd 0.0
m_zx2    dd 0.0
m_zy2    dd 0.0

; The image build pads to a 1.44 MiB floppy; stage 2 must fit the 16 loaded
; sectors (8192 bytes), which this comfortably does.
