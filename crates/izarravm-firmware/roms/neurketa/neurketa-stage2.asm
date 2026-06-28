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

    ; selector 0 or unknown: empty baseline, iterations 0, aux 0.
    xor ax, ax
    xor bx, bx
    jmp report

.sieve:
    call sieve                 ; bx = prime count of the last pass
    mov ax, SIEVE_ITER         ; iterations
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

; The image build pads to a 1.44 MiB floppy; stage 2 must fit the 16 loaded
; sectors (8192 bytes), which this comfortably does.
