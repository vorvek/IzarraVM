; WHEELTEST.COM - guest self-test for the Toka-DOS TOKAMOUS scroll-wheel path.
;
; Runs from AUTOEXEC.BAT after MOUSE.COM (TOKAMOUS) is resident. It drives the
; INT 33h wheel API and the host-injected wheel-detent path, then reports a
; single pass/fail through the Lotura unit-tester (ports 0xE4-0xE6): exit code
; 0 = every check passed, a distinct nonzero code per failing check so the host
; test can tell which one broke.
;
; Stages:
;   A  wheel API present: AX=0x0011 returns AX=0x574D ("WM").       -> fail 1
;   B  host-injected wheel seen with the right sign: poll AX=0x0003,
;      read BH (signed wheel counter). The host injects positive detents
;      (inject_mouse_wheel(1)), so BH must become non-zero AND positive
;      (top bit clear). No motion within the poll cap                -> fail 3
;      wrong sign (BH negative, top bit set)                         -> fail 2
;
; The whole stack runs for real: boot -> DOS -> AUTOEXEC -> MOUSE TSR ->
; INT 15h C205/C200 IntelliMouse enable -> host inject_mouse_wheel -> 8042 ->
; IRQ12 -> BIOS INT 74h ISR (sign-extends the Z byte) -> TOKAMOUS packet
; handler (accumulates [wheel]) -> INT 33h fn 0x03 BH.
;
; Assemble: nasm -f bin wheeltest.asm -o WHEELTEST.COM
    cpu 386
    org 0x100

; Lotura unit-tester ports and protocol.
UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3

; The poll cap for Stage B. Each iteration is a full INT 33h getpos, so this is
; generously large: the host injects the wheel detent well before the cap.
POLL_CAP        equ 2000000

start:
    ; -------- Stage A: wheel API present --------
    mov ax, 0x0011                      ; get wheel capabilities (CuteMouse API)
    int 0x33
    cmp ax, 0x574D                      ; "WM" = wheel supported
    jne fail_a

    ; -------- Stage B: host-injected wheel detent, positive sign --------
    ; Poll getpos until BH (the signed wheel counter) goes non-zero. The host
    ; injects inject_mouse_wheel(1) between run chunks; fn 0x03 consumes the
    ; accumulated detents into BH each call, so once a detent has been delivered
    ; the very next poll reads it. A read of 0 just means none yet - keep polling.
    mov ecx, POLL_CAP
.poll:
    push ecx
    mov ax, 0x0003                      ; getpos -> BX buttons+wheel, CX x, DX y
    int 0x33
    pop ecx
    test bh, bh                         ; signed wheel counter
    jnz .got_wheel
    dec ecx
    jnz .poll
    jmp fail_no_wheel                   ; cap hit with no wheel movement seen

.got_wheel:
    ; BH is non-zero. The host injected positive detents, so the accumulated
    ; counter must be positive: the sign bit (0x80) must be clear.
    test bh, 0x80
    jnz fail_wrong_sign                 ; top bit set -> negative -> wrong sign

    ; Every stage passed.
    xor al, al                          ; code 0
    jmp ut_exit

fail_a:
    mov al, 1
    jmp ut_exit
fail_wrong_sign:
    mov al, 2
    jmp ut_exit
fail_no_wheel:
    mov al, 3
    jmp ut_exit

; Stop the machine through the Lotura unit-tester with the code in AL.
ut_exit:
    mov ah, al                          ; keep the code in AH across the OUTs
    mov dx, UT_INDEX
    mov al, UT_REG_EXIT
    out dx, al                          ; select the exit-code register
    mov dx, UT_DATA
    mov al, ah
    out dx, al                          ; store the exit code
    mov dx, UT_COMMAND
    mov al, UT_CMD_EXIT
    out dx, al                          ; run Exit
.hang:
    jmp .hang                           ; the run loop stops us before the next insn
