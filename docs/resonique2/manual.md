# ReSonique 2 Sound Card Manual

ReSonique 2 is the Izarra 3000's audio hardware: a combo card built around a
Sound Blaster 16 compatible digital audio path, an OPL3 FM synthesizer, and
a Yamaha ADPCM-B streaming DAC as a second, independent playback device.
This manual covers what a DOS program actually finds when it probes the
card.

## What's on the card

| Section | Compatibility | Base port | IRQ | DMA |
| --- | --- | --- | --- | --- |
| Digital audio | Sound Blaster 16 / CT1745 mixer | `0x220` | 5 | 8-bit: 1, 16-bit: 5 |
| FM synthesis | OPL3 (Yamaha YMF262) | `0x388` | n/a | n/a |
| Streaming DAC | Yamaha ADPCM-B | `0x240` (configurable) | 10 | 3 |

The digital audio and FM sections sit at their standard, fixed Sound
Blaster addresses, the way real SB16 hardware does. Software doesn't need
to detect them beyond the usual Sound Blaster and AdLib probes. The ADPCM-B
DAC is a separate, independently configurable device window, deliberately
wired to defaults (`0x240`/IRQ 10/DMA 3) that stay clear of the SB16 and any
Windows Sound System codec sharing the same machine.

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

## Streaming DAC (Yamaha ADPCM-B)

A second, independent digital audio path: a streaming ADPCM decoder
modeled on the Yamaha ADPCM-B format (the same family used in the YM2608
and Y8950), decoding 4-bit ADPCM data streamed in over DMA or a direct data
port into 16-bit PCM, with its own half-buffer and end-of-buffer interrupts.
This is the same shape of interrupt behavior a Sound Blaster DSP gives you,
just on its own port window so it can run concurrently with the SB16 and
OPL3 sections rather than competing with them. Software that specifically
targets this path needs its own driver; it isn't detected through the
standard Sound Blaster or AdLib probes.

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
