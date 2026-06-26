; kbd-layouts.inc - keyboard stream: six Set-1 scancode -> ASCII tables, each an
; unshifted, shifted, and AltGr table. The INT 09h ISR (izbios-kbd.inc) picks the
; active layout from the cached CMOS layout byte and indexes these.
;
; Layout indices match the CMOS 0x10 / setup-page order:
;   0 en-US   1 UK   2 ES   3 FR (AZERTY)   4 DE (QWERTZ)   5 IT (QWERTZ)
;
; Table geometry: each layout is one 384-byte block: unshifted, shifted, then
; AltGr, each 128 bytes. The ISR computes
;   base = kbd_layout_tables + layout*384 + modifier offset
; then reads [base + scancode]. A 0 byte means the key emits no ASCII here.
;
; Coverage is the main typing block (scancodes 0x00..0x39); 0x3a and up read 0.
; A few keys carry non-ASCII letters (the Spanish n-tilde, the German eszett,
; the UK pound sign); those are emitted as their CP437 code points. There is no
; dead-key composition this slice: a key that is a dead key on real hardware
; emits its base character.
;
; Each layout is written out in full rather than patched from a base, so the
; column alignment matches across all six and a single key is easy to read off.

; KBD_LAYOUT_STRIDE is defined in izbios-defs.inc (256 = 128 unshifted + 128 shifted).

align 2
kbd_layout_tables:

; ----------------------------- 0: en-US ------------------------------------
.us_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0','-','=', 8,  9     ; 00-0f
    db 'q','w','e','r','t','y','u','i','o','p','[',']', 13, 0,  'a','s'      ; 10-1f
    db 'd','f','g','h','j','k','l',';',39, '`', 0,  92, 'z','x','c','v'      ; 20-2f
    db 'b','n','m',',','.','/', 0,  '*', 0,  ' '                            ; 30-39
    times 0x80 - ($ - .us_lo) db 0
.us_hi:
    db 0,    27,  '!','@','#','$','%','^','&','*','(',')','_','+', 8,  9     ; 00-0f
    db 'Q','W','E','R','T','Y','U','I','O','P','{','}', 13, 0,  'A','S'      ; 10-1f
    db 'D','F','G','H','J','K','L',':',34, '~', 0,  '|','Z','X','C','V'      ; 20-2f
    db 'B','N','M','<','>','?', 0,  '*', 0,  ' '                            ; 30-39
    times 0x80 - ($ - .us_hi) db 0
.us_altgr:
    times 0x80 db 0

; ------------------------------- 1: UK -------------------------------------
; British ISO. Letters match US. 2 carries the double quote, 3 the pound sign
; (CP437 0x9C), the apostrophe key carries @, and the extra ISO key 0x2b carries
; # and ~. The key left of 1 keeps the backtick and gives the not sign shifted
; (CP437 0xAA).
.uk_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0','-','=', 8,  9     ; 00-0f
    db 'q','w','e','r','t','y','u','i','o','p','[',']', 13, 0,  'a','s'      ; 10-1f
    db 'd','f','g','h','j','k','l',';',39, '`', 0,  '#','z','x','c','v'      ; 20-2f
    db 'b','n','m',',','.','/', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .uk_lo) db 0
    db '<'                                                                  ; 0x56 ISO <
    times 0x80 - ($ - .uk_lo) db 0
.uk_hi:
    db 0,    27,  '!','"',0x9c,'$','%','^','&','*','(',')','_','+', 8,  9    ; 00-0f
    db 'Q','W','E','R','T','Y','U','I','O','P','{','}', 13, 0,  'A','S'      ; 10-1f
    db 'D','F','G','H','J','K','L',':','@', 0xaa,0, '~','Z','X','C','V'      ; 20-2f
    db 'B','N','M','<','>','?', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .uk_hi) db 0
    db '>'                                                                  ; 0x56 ISO >
    times 0x80 - ($ - .uk_hi) db 0
.uk_altgr:
    times 0x80 db 0

; ------------------------------- 2: ES -------------------------------------
; Spanish QWERTY. The headline key is n-tilde on the scancode US uses for the
; semicolon (0x27): n-tilde lower (CP437 0xA4), N-tilde upper (CP437 0xA5).
; The apostrophe key (0x28) carries a dead acute base here. The brace key (0x2b)
; carries c-cedilla (CP437 0x87 / 0x80). The key left of 1 (0x29) carries
; masculine/feminine ordinal indicator (CP437 0xA7 / 0xA6). 0x0d carries
; inverted punctuation (CP437 0xAD / 0xA8). The ISO 102nd key (0x56) carries
; < and >.
.es_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0',39, 0xad,8,  9      ; 00-0f  (0x0c=', 0x0d=¡)
    db 'q','w','e','r','t','y','u','i','o','p', 0x60,'+', 13, 0,  'a','s'     ; 10-1f  (0x1a=` dead grave)
    db 'd','f','g','h','j','k','l',0xa4,0x27,0xa7,0,  0x87,'z','x','c','v'      ; 20-2f  (0x27=ñ, 0x28=´ dead acute, 0x29=º, 0x2b=ç)
    db 'b','n','m',',','.','-', 0,  '*', 0,  ' '                            ; 30-39  (0x35=-)
    times 0x56 - ($ - .es_lo) db 0
    db '<'                                                                  ; 0x56 ISO <
    times 0x80 - ($ - .es_lo) db 0
.es_hi:
    db 0,    27,  '!','"',0xfa,'$','%','&','/','(',')','=','?', 0xa8,8,  9    ; 00-0f  (0x04=·, 0x0d=¿)
    db 'Q','W','E','R','T','Y','U','I','O','P', 0x5e,'*', 13, 0,  'A','S'     ; 10-1f  (0x1a=^ dead circumflex)
    db 'D','F','G','H','J','K','L',0xa5,0x22,0xa6,0,  0x80,'Z','X','C','V'      ; 20-2f  (0x27=Ñ, 0x28=¨ dead diaeresis, 0x29=ª, 0x2b=Ç)
    db 'B','N','M',';',':','_', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .es_hi) db 0
    db '>'                                                                  ; 0x56 ISO >
    times 0x80 - ($ - .es_hi) db 0
.es_altgr:
    db 0, 0, '|','@','#','~',0,0xaa,0,0,0,0, 0,  0,  0,  0                 ; 00-0f
    db 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '[',']', 0, 0, 0, 0                 ; 10-1f
    db 0, 0, 0, 0, 0, 0, 0, 0,'{',92, 0, '}', 0, 0, 0, 0                 ; 20-2f
    times 0x80 - ($ - .es_altgr) db 0

; ------------------------------- 3: FR -------------------------------------
; French AZERTY. A and Q swap, Z and W swap, and M moves to the scancode US uses
; for the semicolon (0x27); the US M position (0x32) carries the comma. The digit
; row keeps the digits (rather than the accented unshifted symbols) so the BIOS
; setup and boot prompts stay typeable without dead keys.
.fr_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0', ')','=', 8,  9    ; 00-0f
    db 'a','z','e','r','t','y','u','i','o','p', '^','$', 13, 0,  'q','s'     ; 10-1f
    db 'd','f','g','h','j','k','l','m','%', '*', 0,  '*','w','x','c','v'      ; 20-2f  (0x27 = m)
    db 'b','n',',',';',':','!', 0,  '*', 0,  ' '                            ; 30-39  (0x32 = ,)
    times 0x56 - ($ - .fr_lo) db 0
    db '<'                                                                  ; 0x56 ISO <
    times 0x80 - ($ - .fr_lo) db 0
.fr_hi:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0', 0xf8,'+', 8,  9   ; 00-0f
    db 'A','Z','E','R','T','Y','U','I','O','P', 0xa8,0x9c,13,0,  'Q','S'     ; 10-1f
    db 'D','F','G','H','J','K','L','M','%', 0xe6,0, 0xe6,'W','X','C','V'      ; 20-2f
    db 'B','N','?','.','/', 0xa7,0, '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .fr_hi) db 0
    db '>'                                                                  ; 0x56 ISO >
    times 0x80 - ($ - .fr_hi) db 0
.fr_altgr:
    db 0, 0, 0,'~','#','{','[','|','`',92,'^','@',']','}',0,0             ; 00-0f
    times 0x80 - ($ - .fr_altgr) db 0

; ------------------------------- 4: DE -------------------------------------
; German QWERTZ. Y and Z swap. The scancode US uses for minus (0x0c) carries the
; eszett (CP437 0xE1); the bracket keys carry the u-umlaut and o-umlaut bases
; (emitted as CP437 letters), and the semicolon key carries the a-umlaut.
; Accent dead keys emit a base character.
.de_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0', 0xe1,39, 8,  9    ; 00-0f  (0x0c = eszett)
    db 'q','w','e','r','t','z','u','i','o','p', 0x81,'+', 13, 0,  'a','s'    ; 10-1f  (0x15 = z, 0x1a = u-umlaut)
    db 'd','f','g','h','j','k','l', 0x94,0x84,'^',0, '#','y','x','c','v'      ; 20-2f  (0x27 = o-umlaut, 0x28 = a-umlaut, 0x2c = y)
    db 'b','n','m',',','.','-', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .de_lo) db 0
    db '<'                                                                  ; 0x56 ISO <
    times 0x80 - ($ - .de_lo) db 0
.de_hi:
    db 0,    27,  '!','"', 0x15,'$','%','&','/','(',')','=','?', '`', 8,  9  ; 00-0f
    db 'Q','W','E','R','T','Z','U','I','O','P', 0x9a,'*', 13, 0,  'A','S'    ; 10-1f
    db 'D','F','G','H','J','K','L', 0x99,0x8e,0xf8,0, 39, 'Y','X','C','V'     ; 20-2f
    db 'B','N','M',';',':','_', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .de_hi) db 0
    db '>'                                                                  ; 0x56 ISO >
    times 0x80 - ($ - .de_hi) db 0
.de_altgr:
    db 0, 0, 0,0,0,0,0,0,'{','[',']','}',92,0,0,0                        ; 00-0f
    db '@',0,0,0,0,0,0,0,0,0,0,0,0,0,0,0                                ; 10-1f
    times 0x80 - ($ - .de_altgr) db 0

; ------------------------------- 5: IT -------------------------------------
; Italian, treated as QWERTZ for this slice (Y and Z swap). The scancode US uses
; for minus (0x0c) carries the apostrophe, which also separates IT from DE on that
; key. The accented vowel keys (o-grave, a-grave, e-grave, u-grave, i-grave) sit on
; the right-hand symbol scancodes and emit their CP437 base letters.
.it_lo:
    db 0,    27,  '1','2','3','4','5','6','7','8','9','0', 39, 0x8d,8,  9    ; 00-0f  (0x0c = ')
    db 'q','w','e','r','t','z','u','i','o','p', 0x8a,'+', 13, 0,  'a','s'    ; 10-1f  (0x15 = z, 0x1a = e-grave)
    db 'd','f','g','h','j','k','l', 0x95,0x85,92, 0,  0x97,'y','x','c','v'    ; 20-2f  (0x27 = o-grave, 0x28 = a-grave, 0x2b = u-grave, 0x2c = y)
    db 'b','n','m',',','.','-', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .it_lo) db 0
    db '<'                                                                  ; 0x56 ISO <
    times 0x80 - ($ - .it_lo) db 0
.it_hi:
    db 0,    27,  '!','"',0x9c,'$','%','&','/','(',')','=','?','^', 8,  9    ; 00-0f
    db 'Q','W','E','R','T','Z','U','I','O','P', 0x82,'*', 13, 0,  'A','S'    ; 10-1f
    db 'D','F','G','H','J','K','L','@','#', '|', 0,  0xa7,'Y','X','C','V'     ; 20-2f
    db 'B','N','M',';',':','_', 0,  '*', 0,  ' '                            ; 30-39
    times 0x56 - ($ - .it_hi) db 0
    db '>'                                                                  ; 0x56 ISO >
    times 0x80 - ($ - .it_hi) db 0
.it_altgr:
    db 0, 0, 0,0,0,0,0,0,0,0,0,0,0,0,0,0                             ; 00-0f
    db 0, 0, 0,0,0,0,0,0,0,0,'[',']',0,0,0,0                           ; 10-1f
    db 0, 0, 0,0,0,0,0,'@','#',0,0,0,0,0,0,0                           ; 20-2f
    times 0x80 - ($ - .it_altgr) db 0

; --- Dead-key composition data (consumed by kb_deadkey) ---------------------
; Accent ids: 1 grave, 2 acute, 3 circumflex, 4 diaeresis.

; Descriptor: which (layout, scancode, shift) is a dead key. Each row is
; layout, scancode, shift (0=unshifted,1=shifted), accent id. 0xFF terminates.
; AltGr keys are never dead. Only ES is wired this round.
kbd_deadkey_desc:
    db 2, 0x1a, 0, 1       ; ES `  grave
    db 2, 0x1a, 1, 3       ; ES ^  circumflex
    db 2, 0x28, 0, 2       ; ES acute
    db 2, 0x28, 1, 4       ; ES diaeresis
    db 0xff

; Spacing glyph emitted when a dead key is flushed (dead+space or dead+non-vowel).
; Indexed by accent id - 1.
kbd_deadkey_flush:
    db 0x60, 0x27, 0x5e, 0x22   ; ` ' ^ "

; Composition: accent id - 1 selects the row; column is the base letter's index
; in kbd_deadkey_bases. 0 = no composed form (caller flushes instead).
kbd_deadkey_bases:
    db 'a','e','i','o','u','y','A','E','I','O','U','Y'
kbd_deadkey_comp:
    ; a     e     i     o     u     y     A     E     I     O     U     Y
    db 0x85,0x8a,0x8d,0x95,0x97,0,   0xb7,0xd4,0xde,0xe3,0xeb,0     ; grave
    db 0xa0,0x82,0xa1,0xa2,0xa3,0,   0xb5,0x90,0xd6,0xe0,0xe9,0     ; acute
    db 0x83,0x88,0x8c,0x93,0x96,0,   0xb6,0xd2,0xd7,0xe2,0xea,0     ; circumflex
    db 0x84,0x89,0x8b,0x94,0x81,0x98,0x8e,0xd3,0xd8,0x99,0x9a,0     ; diaeresis (y -> 0x98)

; kb_deadkey: dead-key state machine, shared by both INT 09h ISRs.
; In:  AL = layout ASCII for this key, BL = make scancode, DS = BDA segment.
; Out: CF=0 -> caller enqueues AL as usual (AL may be a composed char).
;      CF=1 -> caller enqueues nothing (key swallowed or fully handled here).
kb_deadkey:
    push cx
    push si
    push di
    cmp byte [KB_DEAD], 0
    jne .have_pending
    call .lookup_dead          ; -> AH = accent id (0 if not dead)
    test ah, ah
    jz .pass
    mov [KB_DEAD], ah
    jmp .swallow
.have_pending:
    call .lookup_dead          ; -> AH = accent id (0 if not dead)
    test ah, ah
    jz .compose
    push ax
    call .flush
    pop ax
    mov [KB_DEAD], ah
    jmp .swallow
.compose:
    movzx di, byte [KB_DEAD]
    dec di
    mov si, kbd_deadkey_bases
    xor cx, cx
.col_scan:
    cmp cx, 12
    jae .no_match
    mov ah, [cs:si]
    cmp ah, al
    je .col_found
    inc cx
    inc si
    jmp .col_scan
.col_found:
    ; CX = column (0..11), DI = row (0..3), BL = make scancode
    ; want: kbd_deadkey_comp[di*12 + cx]
    push bx                         ; save make scancode
    mov bx, cx                      ; BX = column
    mov si, di
    imul si, si, 12                 ; SI = row * 12
    add si, bx                      ; SI = row*12 + col
    add si, kbd_deadkey_comp
    mov ah, [cs:si]
    pop bx                          ; restore make scancode
    test ah, ah
    jz .no_match
    mov al, ah
    mov byte [KB_DEAD], 0
    jmp .pass
.no_match:
    push ax
    call .flush
    pop ax
    mov byte [KB_DEAD], 0
    cmp al, ' '
    je .swallow
    jmp .pass
.pass:
    pop di
    pop si
    pop cx
    clc
    ret
.swallow:
    pop di
    pop si
    pop cx
    stc
    ret
.lookup_dead:
    cmp byte [KB_ALTGR], 0
    jne .ld_none
    mov ah, 0
    test byte [KB_FLAGS], 0x03
    jz .ld_noshift
    mov ah, 1
.ld_noshift:
    mov cl, [KB_LAYOUT]
    mov si, kbd_deadkey_desc
.ld_loop:
    mov ch, [cs:si]
    cmp ch, 0xff
    je .ld_none
    cmp ch, cl
    jne .ld_next
    mov ch, [cs:si + 1]
    cmp ch, bl
    jne .ld_next
    mov ch, [cs:si + 2]
    cmp ch, ah
    jne .ld_next
    mov ah, [cs:si + 3]
    ret
.ld_next:
    add si, 4
    jmp .ld_loop
.ld_none:
    xor ah, ah
    ret
.flush:
    movzx si, byte [KB_DEAD]
    dec si
    mov al, [cs:kbd_deadkey_flush + si]
    mov ah, bl
    call kb_enqueue
    ret
