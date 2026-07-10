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
| Wavetable daughterboard header | MPU-401 | `0x300` | 9 | n/a |
| Rear MIDI/game port | MPU-401 | `0x330` | 9 | n/a |

The digital and FM sections use their standard, fixed Sound Blaster and
AdLib addresses. ReSonique 2 assigns separate fixed ports to its internal
wavetable header and rear MIDI connection, so software can select either path
explicitly. The rear 15-pin connector also carries the usual joystick signals.

## The BLASTER variable

Toka-DOS sets it in `AUTOEXEC.BAT` so any Sound Blaster-aware program finds
the card without probing:

```
SET BLASTER=A220 I5 D1 H5 P300 T6
```

That's base address `0x220`, IRQ 5, 8-bit DMA channel 1, 16-bit DMA channel
5, wavetable MPU port `0x300`, and card type 6 (Sound Blaster 16). These
match the defaults above.

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

## MIDI and wavetable

The card exposes separate MPU-401 port pairs. Games configured for a wavetable
daughterboard send music to `0x300`; Toka-DOS publishes this as `P300` in
`BLASTER`. This path leads to the daughterboard pin headers inside the case.
Games configured for an external MPU-401 send music to `0x330`, which leads to
the rear MIDI/game connector. A breakout cable provides standard MIDI sockets.

Both ports support UART output and the playback side of MPU-401 intelligent
mode. Intelligent-mode software can use eight timed tracks, the conductor,
tempo and timebase changes, and start or stop playback. The two ports share
IRQ 9 for acknowledgements and data requests. Neither interface provides
recording, external clock sync, metronome input, or MPU reference filters.

IzarraVM supplies the fitted daughterboard and external receiver separately.
See the [GUI guide](../izarravm-gui/guide.md#the-config-modal) for those
host-side settings and status messages.

## Next

- [Using Toka-DOS](../toka-dos/using-toka-dos.md): where `SET BLASTER` is
  set and what else `AUTOEXEC.BAT` does.
- [Troubleshooting](../troubleshooting.md): audio setup issues and
  workarounds.
