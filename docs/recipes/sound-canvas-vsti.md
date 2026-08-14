<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# The Roland Sound Canvas VSTi

Composers wrote and mixed many General MIDI soundtracks of the middle 1990s on
the Sound Canvas. The MIDI music of a game is a score, and not a recording.
The instrument that plays the score controls the sound. The sound agrees with
the sound that the composer heard only on the same instrument. The Roland
Sound Canvas VA plug-in is that instrument. You can put it at the end of the
P330 MIDI route, for SC-55 and SC-88 class playback from an unmodified game.

This page is the general procedure in
[How to use your own MIDI player](host-midi-player.md), with one part
specified. The plug-in is not an application. It is a VSTi, and it needs a
host program to load it. That host program takes the MIDI input and makes the
audio output. Read the general recipe first for the port connections. This
page does not repeat them.

## The chain

```
guest game  ->  P330 (MPU-401, 0x330)  ->  IzarraVM P330 MIDI receiver
            ->  loopMIDI port
            ->  VST host  ->  Sound Canvas VSTi  ->  host audio device
```

## Procedure

1. Make the loopMIDI port, and select it as the **P330 MIDI receiver**. See
   [How to use your own MIDI player](host-midi-player.md). Make sure that the
   status line shows `Ready`.
2. Install a VST host. The host must accept MIDI input from a system port, and
   it must load a VSTi.
   [Falcosoft MIDI Player 6](https://falcosoft.hu/softwares.html) is one such
   host, and this recipe uses it. Other unrelated programs have the name "MIDI
   Player", thus take this one from its own site. A DAW or a small plug-in
   rack is also sufficient, because the procedure uses two functions only.
3. Install the Sound Canvas VSTi. Let the host scan its plug-in folder for it.
4. In the host, set the MIDI input to the loopMIDI port. Use the same port
   that IzarraVM sends to, and not a second port.
5. Load the Sound Canvas VSTi as the instrument for that input.
6. Set the audio output of the host to your sound device. Then increase the
   level of the plug-in.
7. Start the game, and select **General MIDI**, **MPU-401**, or **MIDI** at
   port `330`. If the game has **Roland Sound Canvas**, **SC-55**, or **GS**,
   select that option instead. Those settings send the GS system-exclusive
   messages that the plug-in understands, and the port is the same.

## Notes

- **The map.** The Sound Canvas VA can operate as an SC-55, an SC-88, or a
  later model. A composer almost certainly heard a game from 1994 or 1995 on
  an SC-55. A later game can have an SC-88 mix. If an instrument in a
  soundtrack is incorrect, try the earlier map before you examine the route.
- **Latency.** The buffer of the plug-in host controls the moment of the
  sound. The delay is in the host, and not in the emulator. Decrease the
  buffer size of the host if the music is late against the picture. The delay
  does not change the digital audio and the FM from the emulated card. Thus a
  large buffer makes the music late against the sound effects.
- **Two audio streams.** The output of the machine and the output of the
  plug-in host are two streams into the same device. Set the balance between
  them in the host mixer. The volume control of the machine and `SNDMIXER` do
  not change it.

## Next

- [How to use your own MIDI player](host-midi-player.md): the port
  connections, and what to do when the destination is not in the list.
- [Nuked-SC55 for Sound Canvas playback](nuked-sc55.md): the same instrument,
  from its own ROMs and not modelled.
- [MT-32 ROMs with the P330 receiver](mt32-roms.md): the other standard
  instrument of this period, which the emulator plays.
- [ReSonique II sound card manual](../resonique2/manual.md#midi-and-wavetable):
  the hardware that the guest controls.
