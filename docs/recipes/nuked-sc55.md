<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Using Nuked-SC55 for Sound Canvas playback

[Nuked-SC55](https://github.com/nukeykt/Nuked-SC55) is a ROM-based emulator of
the Roland SC-55 family. Rather than
model the instrument's output, it runs the firmware from the machine's own ROM
images, and its standalone build presents itself as a MIDI device on the host.
That makes it the ROM-accurate alternative to the [Sound Canvas
VSTi](sound-canvas-vsti.md) recipe: the same place in the chain, reached by the
same loopMIDI port, with a different instrument at the end of it.

The program is obtained and installed by you, and so are the ROM images it
runs. IzarraVM neither supplies nor fetches either one. Nuked-SC55 is
distributed separately under its own terms -- the original MAME license, with a
non-commercial clause that is incompatible with IzarraVM's GPL-3-only licence --
which is why this is a recipe rather than a built-in receiver.

## The chain

```
guest game  ->  P330 (MPU-401, 0x330)  ->  IzarraVM P330 MIDI receiver
            ->  loopMIDI port
            ->  Nuked-SC55  ->  host audio device
```

## Procedure

1. Set up the loopMIDI port and select it as the **P330 MIDI receiver**, as in
   [Using your own MIDI player](host-midi-player.md). Confirm the status line
   reads `Ready`.
2. Obtain the Nuked-SC55 standalone build and the ROM set for the model you
   intend to run, and place the ROMs where that program expects them.
3. Start Nuked-SC55 and select the loopMIDI port as its MIDI input -- the same
   port IzarraVM sends to, not a second one.
4. Select the model in Nuked-SC55. Which models are available depends on the
   ROM sets you hold; its own documentation lists what the build supports,
   among them the SC-55mk1 and mk2, the SC-155, the CM-300/SCC-1, the SC-55st,
   the SCB-55, and the JV-880.
5. Start the game and select **General MIDI**, **MPU-401**, or **MIDI** at port
   `330`. If the game offers **Roland Sound Canvas**, **SC-55**, or **GS**
   explicitly, choose that.

## Notes

- **Which model.** The SC-55mk1 is the instrument most General MIDI game
  soundtracks of 1992 to 1995 were written on. A mk2 is not the same instrument
  and does not sound identical, so prefer the mk1 when matching a score of that
  period.
- **Accuracy against cost.** Running the firmware is more faithful than
  modelling it and also more expensive: this path asks noticeably more of the
  host than the internal wavetable at `P300` does. Judge it on a slow machine
  before concluding the emulator has become sluggish.
- **The audio is the program's.** As with any host receiver, the machine's
  volume control and the ReSonique 2 mixer registers do not act on this sound.
  Set its level in Nuked-SC55 or the host mixer.

## Next

- [The Roland Sound Canvas VSTi](sound-canvas-vsti.md): the modelled
  alternative to this one.
- [Using your own MIDI player](host-midi-player.md): the port plumbing, and
  what to do when the destination is not listed.
- [MT-32 ROMs with the P330 receiver](mt32-roms.md): the other ROM-based
  instrument, played inside the emulator.
