<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# How to use Nuked-SC55 for Sound Canvas playback

[Nuked-SC55](https://github.com/nukeykt/Nuked-SC55) is a ROM-based emulator of
the Roland SC-55 family. It does not model the output of the instrument. It
runs the firmware from the ROM images of the machine. Its standalone build is
a MIDI device on the host. Thus it is the ROM-accurate alternative to the
[Sound Canvas VSTi](sound-canvas-vsti.md) recipe. It has the same position in
the chain, and it uses the same loopMIDI port, with a different instrument at
the end.

You must get and install the program, and also the ROM images that it runs.
IzarraVM does not supply them, and it does not download them. Nuked-SC55 has
its own license: the original MAME license, with a non-commercial condition.
That condition is not compatible with the GPL-3-only license of IzarraVM. For
this reason, Nuked-SC55 is a recipe, and not a receiver in the emulator.

## The chain

```
guest game  ->  P330 (MPU-401, 0x330)  ->  IzarraVM P330 MIDI receiver
            ->  loopMIDI port
            ->  Nuked-SC55  ->  host audio device
```

## Procedure

1. Make the loopMIDI port, and select it as the **P330 MIDI receiver**. See
   [How to use your own MIDI player](host-midi-player.md). Make sure that the
   status line shows `Ready`.
2. Get the Nuked-SC55 standalone build, and the ROM set for the model that you
   want. Put the ROMs in the location that the program expects.
3. Start Nuked-SC55, and select the loopMIDI port as its MIDI input. Use the
   same port that IzarraVM sends to, and not a second port.
4. Select the model in Nuked-SC55. The available models depend on the ROM sets
   that you have. The documentation of Nuked-SC55 lists the models of the
   build. They include the SC-55mk1, the SC-55mk2, the SC-155, the
   CM-300/SCC-1, the SC-55st, the SCB-55, and the JV-880.
5. Start the game, and select **General MIDI**, **MPU-401**, or **MIDI** at
   port `330`. If the game has **Roland Sound Canvas**, **SC-55**, or **GS**,
   select that option.

## Notes

- **The model.** Composers wrote most General MIDI game soundtracks of 1992 to
  1995 on the SC-55mk1. The mk2 is a different instrument, and it does not
  sound the same. Use the mk1 for a score of that period.
- **Accuracy against cost.** The firmware is more accurate than a model of the
  firmware. It also needs more from the host than the internal wavetable at
  `P300`. Test this path on a slow machine before you decide that the emulator
  became slow.
- **The audio belongs to the program.** As with each host receiver, the volume
  control of the machine and the ReSonique II mixer registers do not change
  this sound. Set its level in Nuked-SC55, or in the host mixer.

## Next

- [The Roland Sound Canvas VSTi](sound-canvas-vsti.md): the modelled
  alternative to this recipe.
- [How to use your own MIDI player](host-midi-player.md): the port
  connections, and what to do when the destination is not in the list.
- [MT-32 ROMs with the P330 receiver](mt32-roms.md): the other ROM-based
  instrument, which the emulator plays.
