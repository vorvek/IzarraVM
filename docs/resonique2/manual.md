<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# ReSonique 2 Sound Card Manual

ReSonique 2 is the Izarra 3000's audio hardware: a combo card built around a
Sound Blaster 16 compatible digital audio path and an OPL3 FM synthesizer.
This manual describes what a DOS program finds when it probes the card.

## What's on the card

| Section | Compatibility | Base port | IRQ | DMA |
| --- | --- | --- | --- | --- |
| Digital audio | Sound Blaster 16 / CT1745 mixer | `0x220` | 7 | 8-bit: 1, 16-bit: 5 |
| FM synthesis | OPL3 (Yamaha YMF262) | `0x388` | n/a | n/a |
| Codec | Windows Sound System (AD1848) | `0x530` | 11 | 0 |
| Wavetable daughterboard header | MPU-401 | `0x300` | 9 | n/a |
| Rear MIDI/game port | MPU-401 | `0x330` | 9 | n/a |

Those are the power-on defaults, and the ones Toka-DOS advertises in
`BLASTER`. The base ports are fixed. The IRQ and DMA columns are not; see
[Changing the card's resources](#changing-the-cards-resources) for the tool that
changes them.

The digital and FM sections use their standard, fixed Sound Blaster and
AdLib addresses. ReSonique 2 assigns separate fixed ports to its internal
wavetable header and rear MIDI connection, so software can select either path
explicitly. The rear 15-pin connector also carries the usual joystick signals.

### Why the Sound Blaster sits on IRQ 7

IRQ 5 is the Sound Blaster 16 factory setting, so it is the value you might
expect here. ReSonique 2 ships on IRQ 7 instead, because DOS software falls
into two groups and only 7 satisfies both:

- Programs that read `BLASTER` work on any line, since the variable always
  describes the running configuration.
- Programs that hardwire an IRQ almost always hardwire 7, because 7 was the
  factory setting on the Sound Blaster 1.x and 2.0 that their drivers were
  written against. Such a program on IRQ 5 never receives its interrupt: with
  most drivers that means playback starts and then stops after the first DMA
  block, giving a short click instead of sound.

Hardwiring 5 is rare, because 5 only became a default with SB16-class cards, by
which time reading `BLASTER` was standard practice. If you do hit a title that
insists on 5, run `SNDCTRL` before starting it.

Because the AD1848 codec cannot share a line with the Sound Blaster, it takes
IRQ 11 rather than the WSS standard IRQ 7. Real combo cards jumper the two
apart in exactly this way.

## The BLASTER variable

Toka-DOS sets it in `AUTOEXEC.BAT` so any Sound Blaster-aware program finds
the card without probing:

```
SET BLASTER=A220 I7 D1 H5 P300 T6
```

That's base address `0x220`, IRQ 7, 8-bit DMA channel 1, 16-bit DMA channel
5, wavetable MPU port `0x300`, and card type 6 (Sound Blaster 16). These
match the defaults above.

Toka-DOS regenerates this line from the machine's configuration, so it follows
whatever you set. A hand-edited `AUTOEXEC.BAT` is left alone. In a customised
file, `SNDCTRL` rewrites the `SET BLASTER` line in place and leaves the rest of
the file unchanged.

## Changing the card's resources

The `SNDCTRL` utility is used to set your ReSonique 2 Sound Card's hardware
parameters. It is the card's own setup utility, and it is installed in `C:\DOS`
on every Toka-DOS disk. Run it from the DOS prompt:

```
C:\> SNDCTRL
```

![The SNDCTRL configuration screen](sndctrl.png)

Move between the values with the arrow keys or Tab, press Enter on one to
choose from the list the hardware supports, then **F10** to apply. Esc leaves
everything as it was. Values that do not apply to a device show as `*`.

The lists offer only values that are available. A line or channel the other
device already holds is not shown, so the Sound Blaster and the codec cannot be
assigned to the same resource. The emulator enforces the same rule at startup.

Applying does four things:

- The hardware moves immediately. Both devices are re-pointed live, with no
  reboot: the mixer's Interrupt and DMA Setup registers for the Sound Blaster,
  the config register for the codec.
- The choice is saved in CMOS, so it survives a power cycle and comes back on
  the next boot.
- `BLASTER` is updated in the current environment, so a game started from that
  same prompt uses the new routing.
- The `SET BLASTER` line in `C:\AUTOEXEC.BAT` is rewritten, so the next boot
  agrees.

Existing variables are updated, never created. If you deliberately removed the
`BLASTER` line, `SNDCTRL` says so and leaves it removed.

### From the command line

Every setting can be given as a switch, in which case `SNDCTRL` applies it and
exits without drawing anything, which is useful from a batch file:

```
SNDCTRL /SBIRQ:5              Sound Blaster IRQ        2, 5, 7, 10
SNDCTRL /SBDMAL:1             Sound Blaster 8-bit DMA  0, 1, 3
SNDCTRL /SBDMAH:5             Sound Blaster 16-bit DMA 5, 6, 7
SNDCTRL /WSSIRQ:11            Codec IRQ                7, 9, 10, 11
SNDCTRL /WSSDMA:0             Codec DMA                0, 1, 3
SNDCTRL /MPU:330              MPU-401 port             300, 330
SNDCTRL /S                    show the current assignment, change nothing
SNDCTRL /?                    usage
```

Switches combine: `SNDCTRL /SBIRQ:5 /MPU:330`. A value the hardware cannot
select is refused with the list of ones it can, and a combination that would put
both devices on one line or channel is refused outright, and nothing is written.

`SNDCTRL /S` reads the mixer and the codec back rather than reporting what was
last saved, so it reports the card's current assignment even if another program
changed it.

### Why there is nothing to set in the machine config file

There used to be. `[audio.sound_blaster] irq` and its neighbours were removed.
The machine boots from CMOS, so once `SNDCTRL` had saved an assignment, editing
those keys had no effect on the running machine.

Old config files still load. The retired keys are ignored, with a note in the
log naming them.

The file retains the settings CMOS does not hold: whether each device is fitted
at all (`enabled`), and the codec's I/O base (`base`), which is fixed board
wiring rather than a selectable resource.

`--sb-irq`, `--sb-dma` and `--sb-high-dma` still work on the command line. They
set the power-on values for a machine with no saved CMOS, which includes every
headless run, since headless runs do not load one. On a machine that has been
configured, the saved assignment takes precedence, and the emulator reports it:

```
WARN the saved CMOS overrode these flags; it is what the machine boots from.
Change the CPU speed with GSWMODE or the BIOS setup panel (Del), and the sound
card with SNDCTRL, both inside DOS -- or delete cmos.bin to start from the
flags again  flags=--sb-irq 5
```

`--cpu` behaves the same way, for the same reason.

## Setting the volume

Use the `SNDMIXER` utility to set the card's volume levels. It presents seven
vertical faders over the card's own mixer: MASTER, FMSYNTH, WAVE, CD-ROM, MIDI,
the PC speaker, and AMP. Each level is applied as the fader moves, saved to a
file, and restored on the next boot from `AUTOEXEC.BAT`.

```
C:\> SNDMIXER
```

Tab and the arrow keys move between the faders and, after them, the **Accept**
and **Cancel** buttons along the bottom of the box; Enter or Space presses the
one that is selected. Accept leaves with the levels in effect, Cancel puts back
the levels the mixer opened on, and F10 saves them for the next boot.

There is no line-in or microphone fader. The machine models playback only, so
those inputs have no source for a control to adjust.

Every leg of the card powers on at 0 dB, and the mix reserves its headroom after
the mixer, so nothing clips at the default settings and the faders cover the
whole range below them. Each step is 4 dB, which spreads the ten positions
evenly rather than crowding them into the top of a 62 dB scale. The full
switch list is in [SNDMIXER](../toka-dos/commands.md#sndmixer).

Two of the seven are not plain Sound Blaster registers:

- **PC speaker** is the card's PC-SPK input (mixer register `0x3B`). That input
  is two bits wide on the real chip, so the fader has four positions, not ten.
  The beeper is mixed through the card, so MASTER affects it as well.
- **MIDI** is the wavetable synthesiser. A real SB16 has no register for one;
  its "MIDI" volume is the FM bus, which is this card's FMSYNTH fader. The
  ReSonique 2 therefore adds a pair of its own at `0x50`/`0x51`, on the same
  register file and the same 5-bit scale as everything else. That pair alone
  carries a mute bit in D0, and a level of zero on it is the quietest audible
  step rather than silence. The wavetable is the only source with no second
  control elsewhere in the machine, so a program that cleared the mixer's
  registers would otherwise silence it with no indication of the cause. Mute
  the wavetable with the mute bit.

### Output gain (AMP)

The seventh fader is not a volume control. It is the card's output amplifier,
the stage after the mixer's summing node, and it is the only control on the
card that adds level rather than taking it away.

| Register | Bits | Field | Positions |
| --- | --- | --- | --- |
| `0x41` | D7-D6 | Output gain, left | 0, 1, 2, 3 |
| `0x42` | D7-D6 | Output gain, right | 0, 1, 2, 3 |

Each position is 6 dB:

| Position | Gain | Fader step |
| --- | --- | --- |
| 0 | 0 dB | 0 |
| 1 | +6 dB | 3 |
| 2 | +12 dB | 7 |
| 3 | +18 dB | 10 |

Position 0 is the power-on setting, and it is the bottom of the fader's travel
rather than a mute: an amplifier at 0 dB is an amplifier passing its input
through, and everything above it is gain the card was not applying before. The
two registers are written together — `SNDMIXER` moves both from one fader,
because a difference between them is a balance change and not a level.

**The gain can clip.** The mix reserves 6 dB of headroom after the mixer, which
is exactly enough to absorb one leg driven to full scale. Position 1 spends that
reserve. Positions 2 and 3 are past it, and a hot source — a full-scale digital
audio leg, or several legs playing at once — will distort. The card offers the
positions and the fader offers them; neither one refuses the setting or quietly
limits it. Use them to lift a quiet recording, not to make a loud one louder.

## Digital audio (Sound Blaster 16 compatible)

The CT1745-compatible mixer and DSP answer at `0x220`-`0x22F`, with the
power-on IRQ and DMA defaults from the table above. Both are movable, from
inside DOS with [`SNDCTRL`](#changing-the-cards-resources), in case a program
insists on jumpering the card somewhere else. Toka-DOS's `BLASTER` line always
matches the running configuration.

The mixer's Interrupt and DMA Setup registers (`0x80`/`0x81`) are writable by
the guest, so a program that configures the card that way moves it as well,
by the same path `SNDCTRL` uses.

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
DSP's adaptive predictor, with one interrupt at each programmed block boundary,
as in raw PCM playback. No additional detection or configuration is required for
a program that issues the ADPCM DSP commands.

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
