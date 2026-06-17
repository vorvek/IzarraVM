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

