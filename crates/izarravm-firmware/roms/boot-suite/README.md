<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IzarraVM Boot Test Suite

This directory contains the first BIOS-bootable compatibility target for IzarraVM.

- `boot.asm` is a 512-byte real-mode boot sector. A real PC BIOS loads it at `0000:7C00`; it loads stage 2 with INT 13h.
- `stage2.asm` is the freestanding test harness loaded at `0000:8000`.
- `results.inc` is the generated first result table used by stage 2 and by emulator-side parsers.
- `izarravm-test.img` is the checked-in 1.44 MiB boot image artifact.

The suite is capability-revealing. Missing or older hardware reports failed feature tests instead of aborting the run.

## Rebuild the image

No script or CI step builds `izarravm-test.img`. Rebuild it by hand after each change to `boot.asm`, `stage2.asm` or `results.inc`. Use NASM. Version 3.01 gives the checked-in bytes.

Run these commands in this directory:

    nasm -f bin -I . boot.asm -o boot.bin
    nasm -f bin -I . stage2.asm -o stage2.bin

`boot.bin` is 512 bytes. `stage2.bin` is 8,192 bytes. Write `boot.bin` at offset 0 and `stage2.bin` at offset 512 of a 1,474,560-byte file. All other bytes are zero.

```powershell
$boot = [IO.File]::ReadAllBytes("boot.bin")
$stage2 = [IO.File]::ReadAllBytes("stage2.bin")
$img = New-Object byte[] 1474560
[Array]::Copy($boot, 0, $img, 0, $boot.Length)
[Array]::Copy($stage2, 0, $img, 512, $stage2.Length)
[IO.File]::WriteAllBytes("izarravm-test.img", $img)
```

Before you change a source file, assemble the sources and make sure that the result is the same as the checked-in image. After the change, make sure that only bytes in the stage 2 area (offsets 512 to 8,703) are different. Do not commit `boot.bin` or `stage2.bin`.

`LICENSE_MANIFEST.tsv` in the repository root holds the SHA-256 of the image. Put the new value in the row of `izarravm-test.img`. `python scripts/check_file_policy.py` makes sure that the value is correct.

Then run the suite in each CPU mode. Each run must exit with code 0.

    cargo run -p izarravm -- --cpu 386slow --headless-boot-suite
    cargo run -p izarravm -- --cpu 386 --headless-boot-suite
    cargo run -p izarravm -- --cpu 486 --headless-boot-suite
    cargo run -p izarravm -- --cpu 586 --headless-boot-suite

The headless runner stops after half a second of guest time. A test that fails must also reach `HLT` in that time in the `386slow` mode, or the runner reports that the suite did not halt.
