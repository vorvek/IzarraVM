; SEGKEEP.COM - verify mouse callbacks cannot leak segment registers.
;
; Runs after MOUSE.COM is resident. It installs a motion callback that deliberately
; clobbers DS and ES, then waits for host-injected motion. The interrupted program
; must resume with its ES unchanged.
;
; Assemble: nasm -f bin segkeep.asm -o SEGKEEP.COM
    cpu 386
    org 0x100

UT_INDEX        equ 0xE4
UT_DATA         equ 0xE5
UT_COMMAND      equ 0xE6
UT_REG_EXIT     equ 12
UT_CMD_EXIT     equ 3
POLL_CAP        equ 2000000
EXPECTED_ES     equ 0x1234

start:
    push cs
    pop ds
    xor ax, ax                         ; reset mouse
    int 0x33
    mov ax, 0x000C                     ; set user callback
    mov cx, 0x0001                     ; motion events
    push cs
    pop es
    mov dx, callback
    int 0x33

    mov ax, EXPECTED_ES
    mov es, ax
    sti
    mov ecx, POLL_CAP
.poll:
    cmp byte [callback_seen], 0
    jne .check_es
    dec ecx
    jnz .poll
    mov al, 1                          ; callback never fired
    jmp ut_exit

.check_es:
    push es
    pop ax
    cmp ax, EXPECTED_ES
    jne .bad_es
    xor al, al
    jmp ut_exit
.bad_es:
    mov al, 2                          ; callback leaked ES into interrupted code
    jmp ut_exit

callback:
    mov byte [cs:callback_seen], 1
    mov ax, 0xA000
    mov es, ax
    mov ax, 0xB800
    mov ds, ax
    retf

ut_exit:
    mov ah, al
    mov dx, UT_INDEX
    mov al, UT_REG_EXIT
    out dx, al
    mov dx, UT_DATA
    mov al, ah
    out dx, al
    mov dx, UT_COMMAND
    mov al, UT_CMD_EXIT
    out dx, al
.hang:
    jmp .hang

callback_seen   db 0
