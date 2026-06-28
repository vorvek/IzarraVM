# SP-1 boot fault log

Running record of observations booting FreeDOS (`freedos-spike.img`) and the
resolutions applied.  Feeds SP-2 (vendored source build).

Image: freedos-spike.img (FreeDOS minimal-config), 1,474,560 bytes.
Cycle budget reaching prompt: **< 500,000,000 cycles** (3.04 s wall-clock at
gsw_386/22 MHz; prompt appears well within budget — budget exhausted after the
prompt is already visible).

---

## Boot path (confirmed working — ZERO CPU faults)

```
INT 19h  →  FreeDOS 1.4 boot sector
         →  KERNEL.SYS (FreeDOS kernel 2043, build 2043 OEM:0xfd)
         →  COMMAND.COM  (FreeCom 0.86 - WATCOMC - XMS_Swap)
         →  FDAUTO.BAT   (@ECHO OFF — no-op)
         →  A:\>
```

No missing opcodes, no CPU faults, no `CpuError` stop reason.  The planned
LSS/LFS/LGS contingency (plan Task 3) was not needed.

---

## Obstacle: x86BOOT.img is the FreeDOS 1.4 installer disk

The prebuilt `x86BOOT.img` image carries an `FDCONFIG.SYS` that displays a
10-second language selection MENU (default 1) then runs `FDAUTO.BAT` →
`SETUP.BAT` (the full installer).  There is no unmodified path to a bare shell
prompt:

- The 10-second MENU wastes most of a 500 M-cycle budget at 22 MHz.
- Even if F5 "skip config" were pressed, there is no `COMMAND.COM` in the root
  directory; the real shell lives at `\FREEDOS\BIN\COMMAND.COM`, so the
  FreeDOS kernel's default shell search fails.
- `FDAUTO.BAT` (1,476 bytes) calls the installer `SETUP.BAT` (39,560 bytes),
  which would never produce an interactive prompt.

### Intermediate finding: DATE/TIME prompt

With a first-pass `FDCONFIG.SYS` that set `SHELL=...\COMMAND.COM ... /P`
(no batch argument), FreeCom displayed:

```
FreeCom version 0.86 - WATCOMC - XMS_Swap [Dec 30 2024 22:10:51]
Current date is Thu 01-01-2026
Enter new date (mm-dd-[cc]yy):
```

FreeCom's `/P` flag (without `=batchfile`) unconditionally runs the interactive
DATE and TIME prompts.  `SWITCHES=/N` in FDCONFIG.SYS does not suppress them.

---

## Resolution: `scripts/prep-freedos-spike.py`

The script derives a minimal-config boot disk from `x86BOOT.img`:

- **KERNEL.SYS** — byte-for-byte unchanged (cluster 1193, 46,485 bytes)
- **COMMAND.COM** (`\FREEDOS\BIN\COMMAND.COM`) — byte-for-byte unchanged
- **FDCONFIG.SYS** — replaced (cluster 96) with:

  ```
  SWITCHES=/N
  LASTDRIVE=Z
  FILES=40
  SHELL=\FREEDOS\BIN\COMMAND.COM \FREEDOS\BIN /E:2048 /P=\FDAUTO.BAT
  ```

  `/P=\FDAUTO.BAT` makes COMMAND.COM permanent and runs the named batch as
  its autoexec.  This skips the DATE/TIME interactive prompts.

- **FDAUTO.BAT** — replaced (cluster 93) with `@ECHO OFF` (11 bytes, no-op).
  The installer and any other content is gone; FreeCom executes the batch,
  finds nothing to do, and returns to the prompt.

FAT chain for both modified clusters is set to EOC (0xFFF) in both FAT copies.
Image size is verified to be exactly 1,474,560 bytes.  Script is idempotent
(always rebuilds from the source image).

---

## Smoke test result

```
cargo test --release -p izarravm sp1_freedos_boots_to_prompt -- --ignored --nocapture

running 1 test
test sp1_smoke::sp1_freedos_boots_to_prompt ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out; finished in 3.04s
```

PASS.  Screen contains `A:\>`.

---

## Verbatim 80x25 screen at cycle budget (500,000,000 cycles)

```
................................................................................
FreeDOS kernel 2043 (build 2043 OEM:0xfd) [compiled May 13 2021]
Kernel compatibility 7.10 - WATCOMC - FAT32 support

(C) Copyright 1995-2012 Pasquale J. Villani and The FreeDOS Project.
All Rights Reserved. This is free software and comes with ABSOLUTELY NO
WARRANTY; you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation;
either version 2, or (at your option) any later version.
 - InitDiskno hard disks detected

FreeCom version 0.86 - WATCOMC - XMS_Swap [Dec 30 2024 22:10:51]
A:\>
```

Stop reason: `CycleLimit { requested: 500000000 }` — budget exhausted after
the prompt was already visible.  CS:IP = `F000:48E2` (BIOS idle HLT loop).

---

## Secondary criterion (typed input) — RESOLVED

The secondary smoke test `sp1_freedos_runs_injected_ver` injects `ver\r` at the
prompt and asserts the command echoes (`a:\>ver`). It now **passes** and is
`#[ignore]`d only because it requires the spike image
(`IZARRAVM_FREEDOS_SPIKE_IMG`).

### Confirmed root cause

**INT 16h AH=00h and AH=10h in `izbios-kbd.inc` clobbered CX**, violating the
IBM AT BIOS contract that INT 16h must preserve every register except AX/FLAGS.

The handlers' second DS-load (after the empty-ring spin loop) used `mov cx,
BDA_SEG / mov ds, cx` as a scratch sequence.  CX was left holding `0x0040`
after IRET.  FreeDOS's `CONRead` subroutine in the CON driver uses CX as its
character read-loop counter (e.g., CX=1 for single-char reads).  With CX
clobbered to 64, `CONRead` looped 64 times calling INT 16h, consuming 64 keys
from the ring.  On the first keystroke only one key was present, so it blocked
on the 64th INT 16h call waiting for 63 keys that never came — appearing as a
hard hang with no echo and no command dispatch.

### Fix applied

Both handlers in `izbios-kbd.inc` (`.read` / AH=00h, `.read16` / AH=10h):
changed the DS-load scratch from CX to BX.  BX is unconditionally reloaded on
the very next instruction (`mov bx, [KB_BUF_HEAD]`), so this is zero-cost and
idiomatically correct.  An explanatory comment references the IBM AT contract
and FreeDOS's use of CX.

### Sibling audit

**`izbios-kbd.inc` other subfunctions** — audit clean.  The `.peek`,
`.peek16`, `.flags`, `.flags12`, `.bufwrite`, `.typematic`, `.funcs`, `.kbid`
handlers all either use AX/BX as scratch (already clobbered), push/pop what
they use, or return the register as part of their output.  No additional
violations found.

**`kbd-bios-core.inc`** — the **same CX-clobber bug was present** in both
`.read` (AH=00h) and `.read16` (AH=10h) of the second INT 16h ROM.  Fixed
identically: changed `mov cx, 0x0040 / mov ds, cx` to `mov bx, 0x0040 / mov
ds, bx` in both handlers, with matching explanatory comments.  `kbd-bios.bin`
and `kbd-resident.bin` rebuilt from the corrected source.

## Note for SP-2

The shipped image should be built from vendored FreeDOS source with a proper
rebranded minimal config; `prep-freedos-spike.py` is the spike stand-in only.
Key decisions carried forward:

- `SWITCHES=/N` in FDCONFIG.SYS (harmless; may suppress other prompts in
  future builds).
- `SHELL=\FREEDOS\BIN\COMMAND.COM \FREEDOS\BIN /E:2048 /P=\AUTOEXEC.BAT`
  with a minimal or absent AUTOEXEC.BAT.
- No hard disk attached (`InitDiskno hard disks detected`) — floppy-only boot
  stays on drive A:.
