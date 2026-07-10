; GSW CPU probe (the CPU icon).
; Toggle EFLAGS.AC first. If it is fixed, this is either 386 clock mode. If AC
; changes, toggle EFLAGS.ID. A fixed ID marks the 486. Only the 586 executes
; CPUID, where leaf 0 must report GenuineIntel for the modeled P55C.
;
; The original EFLAGS value and the registers not used for the result are
; restored before returning. NASM character constants pack the first character
; into the low byte, so 'Genu' matches the little-endian CPUID vendor word.

POST_STEP "component.cpu_gsw", probe_cpu

probe_cpu:
    push ebx
    push ecx
    push edx
    push esi

    pushfd
    pop esi                       ; preserve the original EFLAGS image

    mov eax, esi
    xor eax, 1 << 18              ; AC toggles on 486 and later
    push eax
    popfd
    pushfd
    pop eax
    xor eax, esi
    test eax, 1 << 18
    jz .pass                      ; 386 or 386-slow

    mov eax, esi
    xor eax, 1 << 21              ; ID toggles when CPUID is available
    push eax
    popfd
    pushfd
    pop eax
    xor eax, esi
    test eax, 1 << 21
    jz .pass                      ; 486

    push esi
    popfd                         ; CPUID must see the caller's flags
    xor eax, eax
    cpuid                         ; leaf 0: max leaf in EAX, vendor in EBX:EDX:ECX
    cmp ebx, 'Genu'               ; vendor[0..4]
    jne .fail
    cmp edx, 'ineI'               ; vendor[4..8]
    jne .fail
    cmp ecx, 'ntel'               ; vendor[8..12]
    jne .fail
.pass:
    push esi
    popfd
    pop esi
    pop edx
    pop ecx
    pop ebx
    mov al, HRES_PASS
    ret
.fail:
    push esi
    popfd
    pop esi
    pop edx
    pop ecx
    pop ebx
    mov al, HRES_FAIL
    ret
