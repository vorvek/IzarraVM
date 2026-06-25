; probes/probe-optical.inc - STREAM C: ATAPI optical drive probe (the optical
; icon). The CD-ROM is the secondary-channel device at command block 0x170-0x177.
; ATAPI devices are identified by the signature they place in the cylinder
; (LBA mid/high) registers: 0x14 in LBA mid (0x174) and 0xEB in LBA high (0x175).
; The channel asserts this signature at power-on, so reading the two registers
; detects the drive without issuing a command. A drive is present (media
; optional), so this normally PASSes; a non-signature read leaves the icon grey.

%define ATAPI_LBA_MID  0x174         ; secondary-channel LBA mid (signature low)
%define ATAPI_LBA_HIGH 0x175         ; secondary-channel LBA high (signature high)
%define ATAPI_SIG_MID  0x14
%define ATAPI_SIG_HIGH 0xeb

POST_STEP "component.optical_atapi", probe_optical

probe_optical:
    mov dx, ATAPI_LBA_MID
    in al, dx
    cmp al, ATAPI_SIG_MID
    jne .fail
    mov dx, ATAPI_LBA_HIGH
    in al, dx
    cmp al, ATAPI_SIG_HIGH
    jne .fail
    mov al, HRES_PASS
    ret
.fail:
    mov al, HRES_FAIL
    ret
