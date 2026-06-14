# VirtualDOS Boot Test Suite

This directory contains the first BIOS-bootable compatibility target for VirtualDOS.

- `boot.asm` is a 512-byte real-mode boot sector. A real PC BIOS loads it at `0000:7C00`; it loads stage 2 with INT 13h.
- `stage2.asm` is the freestanding test harness loaded at `0000:8000`.
- `results.inc` is the generated first result table used by stage 2 and by emulator-side parsers.
- `virtualdos-test.img` is the checked-in 1.44 MiB boot image artifact.

The suite is capability-revealing. Missing or older hardware reports failed feature tests instead of aborting the run.
