; probes/probe-hdd.inc - STREAM C: ATA hard disk probe (the HDD icon). The boot
; disk is the primary-channel master at command block 0x1F0-0x1F7. Its status
; register (0x1F7) is the presence handle: a mounted drive reads back DRDY
; (bit 6) at idle, while an empty channel reads the all-ones floating bus (the
; bus returns open-bus 0xFF when no ATA disk is attached). PASS only when a real
; drive answers; FAIL leaves the icon grey.
;
; Note: drive C: is normally HLE-backed (the Toka service port), not a real ATA
; image, so this probe FAILs on the default machine by design -- the icon reports
; the actual hardware, lighting only when a genuine ATA disk is mounted.

%define ATA_STATUS     0x1f7         ; primary-channel status register
%define ATA_DRDY       0x40          ; bit 6: device ready

POST_STEP "component.disk_hdd", probe_hdd

probe_hdd:
    mov dx, ATA_STATUS               ; port > 0xFF needs the DX form of IN
    in al, dx
    cmp al, 0xff                     ; floating bus = no drive on the channel
    je .fail
    test al, ATA_DRDY                ; a present drive holds DRDY at idle
    jz .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
