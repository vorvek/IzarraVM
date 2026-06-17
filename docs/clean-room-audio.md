# Clean-Room Audio Policy

IzarraVM reproduces the Resonique 2 sound hardware (OPL3 FM synthesis, Sound
Blaster 16 DSP) for game compatibility. Behaviour is derived from primary
hardware documentation, not from other emulators' source code.

Permitted sources, to study and cite freely:

- Yamaha YMF262 (OPL3) datasheet.
- "Programmer's Guide to the Yamaha YMF262/OPL3" (V. Arnost, 1994) and ADLIB.DOC.
- Creative Labs Sound Blaster 16 (CT1747 DSP) programming guide / sbspec, for the
  DSP command set, reset handshake, and the 8-bit DMA playback protocol.
- Intel 8237A DMA controller datasheet, for the master/slave pair, the register
  map, and single/auto-init transfer semantics.

Restricted:

- Nuked-OPL3 and the DOSBox OPL cores are LGPL 2.1 / GPL. They may be consulted
  ONLY to check an assumption — confirm a table value or an edge-case behaviour.
  Their code must not be copied, translated, or otherwise derived into this
  repository. The constraint is clean-room provenance, not license
  compatibility: even a permissively-licensed implementation is off-limits as a
  source to translate.
- The DOSBox, QEMU, and Bochs SB16 DSP and 8237A implementations are likewise
  oracle-only: they may confirm a register value or an edge case (for example,
  the `0x41` sampling-rate byte order, or the IBM PC/AT page-register address
  wiring) but must never be translated into this code.

Lookup tables are generated from published formulas. Any value cross-checked
against a restricted source must be independently re-derived from the datasheet
or first principles before it lands here.

## SB16 DSP and 8237A notes

Derivations used by the audio slice, recorded so a future reader can re-verify
each against the datasheets cached at `dev_docs/reference/sb16-dsp/` and
`dev_docs/reference/8237a/`:

- DSP reset: write `0x01` then `0x00` to port `0x226`; after a settle the DSP
  queues `0xAA` on read-data (`0x22A`) and sets the data-available bit of the
  buffer-status port (`0x22E`).
- 8-bit PCM is unsigned; `sample_u8` centers it as `(byte - 128) * 256` for the
  signed 16-bit mix path.
- Sampling rate: time-constant command `0x40` gives
  `rate = 1_000_000 / (256 - tc)` Hz; the SB16 command `0x41` programs Hz
  directly with the high byte first, then the low byte.
- Block transfer size command `0x48` takes the low byte then the high byte and
  counts `n + 1` bytes.
- 8237A page registers use the IBM PC/AT address wiring, not channel order:
  `0x83`->channel 1, `0x81`->channel 2, `0x82`->channel 3, `0x87`->channel 0
  (and the 16-bit slave set at `0x89`/`0x8A`/`0x8B`/`0x8F`).
- Terminal count fires when the word count underflows past zero; auto-init
  reloads base address and count, single mode masks the channel.

## SB16 16-bit DMA notes

Derivations for the 16-bit DMA slice, re-derived against the same cached
datasheets (`dev_docs/reference/sb16/`, `dev_docs/reference/8237a/`). Where the
plan's commonly-cited "model" disagreed with the primary source, the primary
source won and the code plus tests were fixed together:

- **`0xBx` command byte** (Sound Blaster 16 Programming Guide, the 8/16-bit
  Single-cycle and Auto-initialize Transfer tables). The command's high nibble
  `0xB0` selects the 16-bit DMA class; the low bits are:
  `0xB0h`=16-bit D/A single-cycle, `0xB6h`=16-bit D/A auto-initialize,
  `0xB8h`=16-bit A/D single-cycle, `0xBEh`=16-bit A/D auto-initialize. Decoding
  the differing bits: **bit 3 (`0x08`) = A/D input**, **bit 2 (`0x04`) =
  auto-initialize**. (The plan's prose `auto_init = cmd & 0x01` and "output bit
  `0x10`" is the commonly-mis-cited model; the datasheet's command table shows
  `0xB0`→single and `0xB6`→auto-init, which differ by bit 2, so the dispatch
  uses `cmd & 0x04` for auto-init and `cmd & 0x08` for input.)
- **`0xBx` mode byte** (the byte after the command; same tables). The documented
  values are `0x00`=8-bit mono unsigned, `0x20`=8-bit stereo unsigned,
  `0x10`=16-bit mono signed, `0x30`=16-bit stereo signed. So **bit 5 (`0x20`) =
  stereo** and **bit 4 (`0x10`) = signed**; the 8-vs-16-bit depth is selected by
  the command (`0xBx` vs `0xCx`), not the mode byte. Default game usage is
  signed (`0x10`/`0x30`).
- **Slave 8237A word addressing** (IBM PC/AT 16-bit DMA wiring). The slave's
  16-bit address counter counts *words*, driving system address lines A1-A16
  with A0 tied low; the page register supplies A17-A23. So the driven byte
  address is `(page << 17) | (cur_addr << 1)` — e.g. page `0x01` at word addr 0
  reads from byte `0x2_0000`, not `0x1_0000`. (The master 8-bit path keeps its
  `(page << 16) | addr` byte addressing.) Verified by the channel-5 unit test.
- **Transfer count** (Programming Guide: "wLength is one less than the actual
  number of samples to be transferred"). For 16-bit the count is in **16-bit
  samples (words)**, `n` meaning `n+1` words. A stereo frame consumes two words
  (left then right), so `block_remaining` decrements by two per stereo frame
  and the half/end-buffer IRQ edges land at the same word midpoints as the 8-bit
  path. Mono decrements by one.
- **Signed 16-bit conversion** (Programming Guide: "For 16-bit PCM data, each
  sample is represented by a 16-bit signed value"). `sample_i16(word) = word as
  i16` directly — no centering, unlike 8-bit. Unsigned 16-bit (rare,
  mode-byte-selected via `bit4=0`) re-centers around `0x8000`:
  `(word.wrapping_sub(0x8000)) as i16`.
- **Interrupt acknowledgement** (Programming Guide, Introduction to DSP
  Programming). `0x22E` acknowledges 8-bit DMA-mode interrupts, `0x22F`
  acknowledges 16-bit DMA-mode interrupts. Only one DMA mode runs at a time and
  both share the single IRQ5 line, so the DSP clears its one pending IRQ on a
  read of either status port.

