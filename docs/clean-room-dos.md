# Clean-Room DOS Policy

IzarraVM implements DOS-compatible behavior for game compatibility. FreeDOS and MS-DOS behavior may be studied as references, but their source code must not be copied, translated, or derived into this repository. The constraint is clean-room provenance, not license compatibility: no implementation is a source to translate, regardless of its license.

The first DOS service target is a host-backed `C:` drive and enough INT/service scaffolding to support early game bring-up later.

## DOS environment block and BLASTER seeding

Most DOS games auto-detect the Sound Blaster by reading the `BLASTER` environment variable rather than probing the card, so the SB16 is only discoverable once the loader seeds the environment. The behavior here is derived from the documented DOS/IBM PSP and EXEC conventions plus Creative's published `SET BLASTER` definition.

**The environment pointer (`PSP:0x2C`).** A program's Program Segment Prefix carries, at offset `0x2C`, the segment of its environment block. INT 21h AH=4Bh (EXEC) populates it when loading a program (Ralf Brown's Interrupt List, "PSP" and "INT 21/AH=4Bh"). A game reaches the environment by reading `PSP:0x2C` (it has `DS = PSP` from the loader) and scanning the segment in memory; no "get environment" syscall is involved. The loader writes this word via `DosKernel::install_environment`.

**The block format.** The environment is a contiguous run of ASCIIZ `KEY=VALUE` strings, terminated by an extra NUL — an empty string marking the end of the list. A scanner walks entries until it reads a zero-length string (the last entry's NUL followed by the terminator NUL). `build_env_block` emits each entry as `KEY=VALUE\0` then a single terminating `\0`; with no entries the block is just that terminator (a valid empty environment).

**The argv0 trailer (omitted).** Real DOS appends, after the terminator, a `0x0001` word (a count) and an ASCIIZ copy of the program's full path (the argv0). It follows the double-NUL, so it is invisible to environment scanners; no in-scope game reads it. It is omitted here because the loader does not currently track the guest program's path. Adding it is a follow-on that depends on the loader recording that path.

**Allocation.** The segment is sized in whole paragraphs (16 bytes, rounded up) and allocated as the first block above the program via the arena, so it sits where real DOS places it and a guest `AH=49h`/`AH=4Ah` around it behaves as on real hardware. Real DOS sizes the program's memory block *after* reserving the environment; the loader mirrors this by carving the env out of the top of a program whose `e_maxalloc` would otherwise claim all of conventional memory, reducing `PSP:0x02` accordingly. A `.COM` (whose block is a fixed 64 KiB) already has room above the program, so no carve-out happens there.

**The BLASTER value.** Creative's `SET BLASTER` variable carries space-separated `LETTER value` fields: `A` is the hex I/O base (`A220` — the Resonique 2 fixes the SB16 base at `0x220`), `I` the IRQ line, `D` the 8-bit DMA channel, `H` the 16-bit DMA channel, `T` the card type (`T6` = Sound Blaster 16), and `P` the MPU-401 base (omitted — MIDI is not modeled yet). The value is derived from `SoundBlasterConfig` (`A220 I{irq} D{dma} H{high_dma} T6`), so it always matches the IRQ/DMA the CT1745 mixer answers; the `SETSOUND` alias carries the identical value for SETSOUND-aware detection. When the card is disabled no `BLASTER` is emitted, so a machine with no SB16 advertises none.

**Clean-room.** The env-block layout and the `PSP:0x2C` pointer are the documented DOS/IBM PSP and EXEC conventions (Ralf Brown's Interrupt List). The BLASTER field meanings are Creative's published `SET BLASTER` definition. No DOSBox, QEMU, or Bochs environment-handling source was translated; those implementations are oracle-only.
