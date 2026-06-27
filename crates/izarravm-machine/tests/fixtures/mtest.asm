; MTEST.COM - guest self-test for the Toka-DOS MOUSE.COM driver (INT 33h).
;
; Runs from AUTOEXEC.BAT after MOUSE.COM is resident. It drives the INT 33h
; dispatcher and the host-injected motion path, then reports a single pass/fail
; through the Lotura unit-tester (ports 0xE4-0xE6): exit code 0 = every check
; passed, a distinct nonzero code per failing check so the host test can tell
; which one broke.
;
; Stages:
;   A  reset (AX=0) returns AX=0xFFFF, BX=2.
;   B  static INT 33h checks with no host motion: setpos/getpos round-trip,
;      range clamp, AX=0x24 version, reset re-centres.
;   C  host-injected motion: open the full range, poll getpos until the
;      position moves, then confirm it moved right+down, the left button is set,
;      and the mickey-to-pixel ratio was applied (equal mickeys move less in y
;      than in x). The host injects the motion between run chunks.
;
; Assemble: nasm -f bin mtest.asm -o MTEST.COM
    cpu 386
    org 0x100

VIRT_MAX_X      equ 639
VIRT_MAX_Y      equ 199
CENTER_X        equ VIRT_MAX_X / 2          ; 319
CENTER_Y        equ VIRT_MAX_Y / 2          ; 99

; Lotura unit-tester ports and protocol.
UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3

; The poll cap for Stage C. Each iteration is a full INT 33h getpos, so this is
; generously large: the host injects motion well before the cap is reached.
POLL_CAP        equ 2000000

start:
    ; -------- Stage A: reset returns installed status --------
    xor ax, ax                          ; AX=0 reset
    int 0x33
    cmp ax, 0xFFFF
    jne fail_a
    cmp bx, 2
    jne fail_a

    ; -------- Stage B: static checks --------

    ; B1: setpos(100,50) then getpos -> CX=100, DX=50.
    mov ax, 0x0004
    mov cx, 100
    mov dx, 50
    int 0x33
    mov ax, 0x0003
    int 0x33
    cmp cx, 100
    jne fail_b1
    cmp dx, 50
    jne fail_b1

    ; B2: set a narrow range, setpos outside it, confirm the clamp.
    ; hrange(10,20), vrange(10,20); setpos(100,50) clamps to (20,20).
    mov ax, 0x0007                      ; set horizontal range
    mov cx, 10
    mov dx, 20
    int 0x33
    mov ax, 0x0008                      ; set vertical range
    mov cx, 10
    mov dx, 20
    int 0x33
    mov ax, 0x0004                      ; setpos(100,50), outside the range
    mov cx, 100
    mov dx, 50
    int 0x33
    mov ax, 0x0003                      ; getpos -> clamped
    int 0x33
    cmp cx, 20
    jne fail_b2
    cmp dx, 20
    jne fail_b2

    ; B3: AX=0x24 with BX=0 returns BX=0x0820, CX=0x0400.
    mov ax, 0x0024
    xor bx, bx
    int 0x33
    cmp bx, 0x0820
    jne fail_b3
    cmp cx, 0x0400
    jne fail_b3

    ; B4: reset re-centres to (CENTER_X, CENTER_Y).
    xor ax, ax
    int 0x33
    mov ax, 0x0003                      ; getpos
    int 0x33
    cmp cx, CENTER_X
    jne fail_b4
    cmp dx, CENTER_Y
    jne fail_b4

    ; -------- Stage C: host-injected motion --------
    ; Reset left the range narrowed (reset does not restore min/max), so open the
    ; full range back up before testing motion or the injected delta gets clamped.
    mov ax, 0x0007                      ; hrange(0, VIRT_MAX_X)
    mov cx, 0
    mov dx, VIRT_MAX_X
    int 0x33
    mov ax, 0x0008                      ; vrange(0, VIRT_MAX_Y)
    mov cx, 0
    mov dx, VIRT_MAX_Y
    int 0x33

    ; Re-centre and record the baseline so the poll detects any change.
    mov ax, 0x0004                      ; setpos to a known baseline
    mov cx, CENTER_X
    mov dx, CENTER_Y
    int 0x33
    mov word [base_x], CENTER_X
    mov word [base_y], CENTER_Y

    ; Poll getpos until the position changes from the baseline (the host injects
    ; motion mid-poll), or the cap is reached.
    mov ecx, POLL_CAP
.poll:
    push ecx
    mov ax, 0x0003                      ; getpos -> BX buttons, CX x, DX y
    int 0x33
    mov [last_btn], bx
    mov [last_x], cx
    mov [last_y], dx
    pop ecx
    mov ax, [last_x]
    cmp ax, [base_x]
    jne .moved
    mov ax, [last_y]
    cmp ax, [base_y]
    jne .moved
    dec ecx
    jnz .poll
    jmp fail_c_nomotion                 ; cap hit with no motion seen

.moved:
    ; The host injects inject_mouse(8, 8, 0x01) per chunk: equal mickeys in x and y
    ; with the left button held. Confirm the cursor moved right and down, the left
    ; button is set, and the mickey-to-pixel ratio was applied. With the default
    ; ratios (8 horizontal, 16 vertical) equal mickeys move the cursor farther in x
    ; than in y, so the y delta is strictly less than the x delta; without ratio
    ; scaling the two deltas would be equal. Compare deltas, not absolute magnitude,
    ; since the injection count is not fixed.
    mov ax, [last_x]
    cmp ax, [base_x]
    jle fail_c_dir                      ; must have moved right (x increased)
    mov ax, [last_y]
    cmp ax, [base_y]
    jle fail_c_dir                      ; must have moved down (y increased)
    test word [last_btn], 0x0001
    jz fail_c_btn                       ; left button must be set
    mov ax, [last_x]
    sub ax, [base_x]                    ; ax = x delta (pixels)
    mov bx, [last_y]
    sub bx, [base_y]                    ; bx = y delta (pixels)
    cmp bx, ax
    jge fail_c_ratio                    ; vertical ratio: y must move less than x

    ; Every stage passed.
    xor al, al                          ; code 0
    jmp ut_exit

fail_a:
    mov al, 1
    jmp ut_exit
fail_b1:
    mov al, 2
    jmp ut_exit
fail_b2:
    mov al, 3
    jmp ut_exit
fail_b3:
    mov al, 4
    jmp ut_exit
fail_b4:
    mov al, 5
    jmp ut_exit
fail_c_nomotion:
    mov al, 6
    jmp ut_exit
fail_c_dir:
    mov al, 7
    jmp ut_exit
fail_c_btn:
    mov al, 8
    jmp ut_exit
fail_c_ratio:
    mov al, 9
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

; ---- data ----
base_x          dw 0
base_y          dw 0
last_x          dw 0
last_y          dw 0
last_btn        dw 0
