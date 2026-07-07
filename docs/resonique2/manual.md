# ReSonique 2 Sound Card Manual

ReSonique 2 is the Izarra 3000's audio hardware: a combo card built around a
Sound Blaster 16 compatible digital audio path and an OPL3 FM synthesizer.
This manual covers what a DOS program actually finds when it probes the
card.

## What's on the card

| Section | Compatibility | Base port | IRQ | DMA |
| --- | --- | --- | --- | --- |
| Digital audio | Sound Blaster 16 / CT1745 mixer | `0x220` | 5 | 8-bit: 1, 16-bit: 5 |
| FM synthesis | OPL3 (Yamaha YMF262) | `0x388` | n/a | n/a |

Both sections sit at their standard, fixed Sound Blaster addresses, the way
real SB16 hardware does. Software doesn't need to detect them beyond the
usual Sound Blaster and AdLib probes.

## The BLASTER variable

Toka-DOS sets it in `AUTOEXEC.BAT` so any Sound Blaster-aware program finds
the card without probing:

```
SET BLASTER=A220 I5 D1 H5 T6
```

That's base address `0x220`, IRQ 5, 8-bit DMA channel 1, 16-bit DMA channel
5, and card type 6 (Sound Blaster 16): exactly the digital audio section's
defaults above.

## Digital audio (Sound Blaster 16 compatible)

The CT1745-compatible mixer and DSP answer at `0x220`-`0x22F`, with the
power-on IRQ and DMA defaults from the table above. These are configurable
per machine through the emulator's config file or `--sb-irq`/`--sb-dma`/
`--sb-high-dma` flags, in case a program insists on jumpering the card
somewhere else, but Toka-DOS's shipped `BLASTER` line always matches
whatever the running configuration actually is.

## FM synthesis (OPL3)

The FM synthesizer is at `0x388`, the standard AdLib/OPL2/OPL3 address, and
answers as a real OPL3 (Yamaha YMF262): the full four-operator instrument
set on top of OPL2's two-operator patches, detected the same way real OPL3
hardware is: by reading back its status and timer registers, the classic
AdLib detection routine.

## Creative ADPCM

The DSP decodes Creative ADPCM the way a real SB16 does, as part of the
digital audio path rather than a separate device. The 4-bit, 2.6-bit, and
2-bit playback commands (`0x74`-`0x77`, `0x16`/`0x17`, and their auto-init
variants) expand the compressed DMA stream to 8-bit samples through the
DSP's adaptive predictor, with the same half-buffer and end-of-buffer
interrupts as raw PCM playback. Nothing extra to detect or configure: a
program that issues the ADPCM DSP commands just works.

## What ReSonique 2 does not have, yet

The Izarra 3000's original spec sheet also lists MPU-401 MIDI and a
wavetable daughterboard. Neither is implemented as hardware a guest program
can program directly:

- **MIDI** is handled on the host side instead: IzarraVM can route MIDI
  output either to an external MIDI device through your operating system,
  or to an in-process FluidSynth soundfont renderer. There is no emulated
  MPU-401 UART at the usual `0x330` port for a DOS program to talk to
  directly. A game that insists on probing for one at that address won't
  find it.
- **Wavetable daughterboard** is flavor text describing where the card's
  design was headed, not a modeled device. There's no wavetable synthesis
  chip on the emulated card today.

If you're chasing down why a game's MIDI or wavetable option doesn't light
up, this is why. Check the [troubleshooting page](../troubleshooting.md)
for what to try instead (OPL3 FM is always a safe fallback for MIDI-style
music in period DOS games).

## Next

- [Using Toka-DOS](../toka-dos/using-toka-dos.md): where `SET BLASTER` is
  set and what else `AUTOEXEC.BAT` does.
- [Troubleshooting](../troubleshooting.md): audio setup issues and
  workarounds.
