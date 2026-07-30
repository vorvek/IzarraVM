; This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
; SPDX-License-Identifier: GPL-3.0-only

; Tab boot-menu shim and INT 19h bootstrap. The menu implementation is in
; izbios-bootbox.inc. Rust owns the primary/fallback plan for both power-on and
; guest-invoked INT 19h, including boot-sector validation and El Torito CD boot.

; Boot-device codes stored in BOOT_CHOICE.
%define BOOT_DEV_FLOPPY  0
%define BOOT_DEV_DISK    1
%define BOOT_DEV_CDROM   2

; boot_menu: shim. The two-pane boxed boot+speed menu lives in izbios-bootbox.inc
; as bootbox_menu. setup-ui.inc calls this label by name (call boot_menu), so the
; label must stay resolvable here; it just forwards into the new menu. Keep the
; name; do NOT rename it.
boot_menu:
    jmp bootbox_menu

; --- INT 19h bootstrap ------------------------------------------------------
; The machine-side INT 19h planner owns the primary and fallback policy for both
; power-on and guest-invoked bootstrap requests.
boot_entry:
    int 0x19
    ret
