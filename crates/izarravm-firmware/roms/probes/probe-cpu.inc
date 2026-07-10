; probes/probe-cpu.inc - STREAM C: GSW-586 CPU probe (the CPU icon).
; The 586 persona models a Pentium MMX P55C. Firmware can issue CPUID before
; the saved GSW mode is applied, so leaf 0 is the identity check:
; EBX:EDX:ECX spell "GenuineIntel". PASS when both fixed words match.
;
; CPUID clobbers EAX/EBX/ECX/EDX; we save and restore the three we do not return
; through (run_post also brackets the handler with push/pop bx, but the probe
; keeps its own house in order). The compare immediates use NASM character
; constants, which pack the first character into the least-significant byte, so
; 'Genu' == 0x756E6547 == the little-endian vendor word the core returns.

POST_STEP "component.cpu_gsw", probe_cpu

probe_cpu:
    push ebx
    push ecx
    push edx
    xor eax, eax
    cpuid                         ; leaf 0: max leaf in EAX, vendor in EBX:EDX:ECX
    cmp ebx, 'Genu'               ; vendor[0..4]
    jne .fail
    cmp ecx, 'ntel'               ; vendor[8..12]
    jne .fail
    pop edx
    pop ecx
    pop ebx
    mov al, HRES_PASS
    ret
.fail:
    pop edx
    pop ecx
    pop ebx
    mov al, HRES_FAIL
    ret
