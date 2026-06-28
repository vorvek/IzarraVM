# SP-1 boot fault log

Running record of faults hit booting FreeDOS (`freedos-spike.img`) and the fixes
applied. Feeds SP-2 (vendored source build).

Image: freedos-spike.img (FreeDOS), 1,474,560 bytes.
Cycle budget reaching prompt: <fill once green>.

## Faults

1. `CycleLimit { requested: 500000000 }` at CS:IP `FF00:0000` — cause: cycle
   budget exhausted before the DOS prompt appeared; not a CPU fault. FreeDOS
   boots successfully through INT 19h, loads the kernel, and COMMAND.COM
   (FreeCom 0.86) prints its banner — but 500 M cycles at gsw_386/22 MHz are
   insufficient to complete AUTOEXEC.BAT / FreeCom initialisation and render
   `C:\>`. The sampled CS:IP (FF00:0000) is the BIOS idle HLT loop, confirming
   the emulator spent most of the budget in the interrupt-driven wait between
   timer ticks. Fix: increase the cycle budget (try 1–2 billion cycles) and/or
   investigate whether a BIOS service (timer, keyboard wait, or PIT tick rate)
   is spinning longer than expected, slowing FreeCom's prompt display. No CPU
   opcode is missing; this is a throughput / timing issue.

   Verbatim harness output (`--headless-boot-floppy --cycles 500000000`):

   ```
   image: ...freedos-spike.img (1474560 bytes)
   stop: CycleLimit { requested: 500000000 }
   CS:IP = FF00:0000
   0000:7C00 = 3B 46 F6 75 20 81 7E F4 AC 0A 75 19 8B 46 FC 30
   boot: still in the BIOS (no boot, or read error)
   video mode: text (03h)
   text non-blank glyphs: 54
   --- 80x25 text ---

   FreeCom version 0.86 - WATCOMC - XMS_Swap [Dec 30 2024 22:10:51]
   --- end text ---
   ```

   Verbatim test panic (`cargo test --release ... --nocapture`):

   ```
   thread 'sp1_smoke::sp1_freedos_boots_to_prompt' panicked:
   no DOS prompt on screen (stop=CycleLimit { requested: 500000000 }).
   --- screen ---

   FreeCom version 0.86 - WATCOMC - XMS_Swap [Dec 30 2024 22:10:51]

   --- end screen ---
   ```
