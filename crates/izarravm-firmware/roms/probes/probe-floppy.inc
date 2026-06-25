; probes/probe-floppy.inc - STREAM C: floppy disk controller probe (the floppy
; icon). The NEC-765/82077-class FDC lives at ports 0x3F2-0x3F7; its main status
; register (MSR, 0x3F4) is the presence handle. A live controller drives RQM
; (bit 7, "data register ready") and never reads back the all-ones floating bus a
; missing controller would. We do not command a seek/recalibrate: the MSR alone
; proves the controller, and a recalibrate would block on drive mechanics with no
; media. PASS when the MSR is a real value with RQM set.

%define FDC_MSR        0x3f4         ; main status register
%define FDC_MSR_RQM    0x80          ; bit 7: data register ready for transfer

POST_STEP "component.floppy_fdc", probe_floppy

probe_floppy:
    mov dx, FDC_MSR                  ; port > 0xFF needs the DX form of IN
    in al, dx
    cmp al, 0xff                     ; all-ones = floating bus (no controller)
    je .fail
    test al, FDC_MSR_RQM             ; a live FDC holds RQM ready at idle
    jz .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
