<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# ReSonique 2 Sound Card Manual

ReSonique 2 is the Izarra 3000's audio hardware: a combo card built around a
Sound Blaster 16 compatible digital audio path and an OPL3 FM synthesizer.
This manual covers what a DOS program actually finds when it probes the
card.

## What's on the card

| Section | Compatibility | Base port | IRQ | DMA |
| --- | --- | --- | --- | --- |
| Digital audio | Sound Blaster 16 / CT1745 mixer | `0x220` | 7 | 8-bit: 1, 16-bit: 5 |
| FM synthesis | OPL3 (Yamaha YMF262) | `0x388` | n/a | n/a |
| Codec | Windows Sound System (AD1848) | `0x530` | 11 | 0 |
| Wavetable daughterboard header | MPU-401 | `0x300` | 9 | n/a |
| Rear MIDI/game port | MPU-401 | `0x330` | 9 | n/a |

Those are the **power-on defaults**, and the ones Toka-DOS advertises in
`BLASTER`. The base ports are fixed; the IRQ and DMA columns are not — see
[Changing the card's resources](#changing-the-cards-resources) for the tool that
moves them.

The digital and FM sections use their standard, fixed Sound Blaster and
AdLib addresses. ReSonique 2 assigns separate fixed ports to its internal
wavetable header and rear MIDI connection, so software can select either path
explicitly. The rear 15-pin connector also carries the usual joystick signals.

### Why the Sound Blaster sits on IRQ 7

IRQ 5 is the Sound Blaster 16 factory setting, so it is the value you might
expect here. ReSonique 2 ships on **IRQ 7** instead, because DOS software falls
into two groups and only 7 satisfies both:

- Programs that **read `BLASTER`** work on any line, since the variable always
  describes the running configuration.
- Programs that **hardwire an IRQ** almost always hardwire 7, because 7 was the
  factory setting on the Sound Blaster 1.x and 2.0 that their drivers were
  written against. Such a program on IRQ 5 never receives its interrupt: with
  most drivers that means playback starts and then stops after the first DMA
  block, giving a short click instead of sound.

Hardwiring 5 is rare, because 5 only became a default with SB16-class cards, by
which time reading `BLASTER` was standard practice. If you do hit a title that
insists on 5, run `SNDCTRL` before it — that is what the tool is for.

Because the AD1848 codec cannot share a line with the Sound Blaster, it takes
**IRQ 11** rather than the WSS standard IRQ 7. Real combo cards jumper the two
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
whatever you set. It leaves a hand-edited `AUTOEXEC.BAT` alone, though — if you
have customised yours, `SNDCTRL` still rewrites the `SET BLASTER` line in place
and leaves the rest of your file untouched.

## Changing the card's resources

Run **`SNDCTRL`** from the DOS prompt. It is the card's own setup utility, the
same thing you would have run on a real sound card of the period, and it is
installed in `C:\DOS` on every Toka-DOS disk:

```
C:\> SNDCTRL
```

![The SNDCTRL configuration screen](sndctrl.png)

Move between the values with the arrow keys or Tab, press Enter on one to
choose from the list the hardware supports, then **F10** to apply. Esc leaves
everything as it was. Values that do not apply to a device show as `*`.

The lists only offer values that work. You will not be shown a line or channel
the other device already holds, so there is no way to put the Sound Blaster and
the codec on top of each other — the same rule the emulator enforces at startup,
applied while you choose instead of after.

Applying does four things:

- **Moves the hardware immediately.** Both devices are re-pointed live, with no
  reboot: the mixer's Interrupt and DMA Setup registers for the Sound Blaster,
  the config register for the codec.
- **Saves the choice in CMOS**, so it survives a power cycle and comes back on
  the next boot.
- **Updates `BLASTER` in the current environment**, so a game started from that
  same prompt sees the new routing straight away.
- **Rewrites the `SET BLASTER` line in `C:\AUTOEXEC.BAT`**, so the next boot
  agrees.

Existing variables are updated, never created. If you deliberately removed the
`BLASTER` line, `SNDCTRL` says so and leaves it removed.

### From the command line

Every setting can be given as a switch, in which case `SNDCTRL` applies it and
exits without drawing anything — useful from a batch file:

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
both devices on one line or channel is refused outright — nothing is written.

`SNDCTRL /S` reads the mixer and the codec back rather than reporting what was
last saved, so it tells you what the card is really doing even if something else
moved it.

### Why there is nothing to set in the machine config file

There used to be. `[audio.sound_blaster] irq` and its neighbours were removed,
because a config file cannot win an argument with NVRAM: the machine boots from
CMOS, so once `SNDCTRL` had saved anything, editing those keys did nothing at
all. A setting that silently does nothing is worse than one that is not there.

Old config files still load. The retired keys are ignored, with a note in the
log naming them.

What stays in the file is what CMOS has no opinion about: whether each device is
fitted at all (`enabled`), and the codec's I/O base (`base`), which is fixed
board wiring rather than a resource anything can select.

`--sb-irq`, `--sb-dma` and `--sb-high-dma` still work on the command line, and
they are what a machine with no saved CMOS starts from — which is every headless
run, since those never load one. On a machine that *has* been configured, the
saved assignment wins, and the emulator says so:

```
WARN the saved CMOS overrode these flags; it is what the machine boots from.
Change the CPU speed with GSWMODE or the BIOS setup panel (Del), and the sound
card with SNDCTRL, both inside DOS -- or delete cmos.bin to start from the
flags again  flags=--sb-irq 5
```

`--cpu` behaves the same way, for the same reason.

## Digital audio (Sound Blaster 16 compatible)

The CT1745-compatible mixer and DSP answer at `0x220`-`0x22F`, with the
power-on IRQ and DMA defaults from the table above. Both are movable, from
inside DOS with [`SNDCTRL`](#changing-the-cards-resources), in case a program
insists on jumpering the card somewhere else. Toka-DOS's `BLASTER` line always
matches whatever the running configuration actually is.

The mixer's Interrupt and DMA Setup registers (`0x80`/`0x81`) are writable by
the guest, so a program that configures the card that way moves it too — the
same path `SNDCTRL` uses.

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
as in raw PCM playback. Nothing extra to detect or configure: a
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
