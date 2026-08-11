<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# The Roland Sound Canvas VSTi

The Sound Canvas is the instrument a great many mid-1990s General MIDI
soundtracks were written on and mixed against. A game's MIDI music is a score,
not a recording: what reaches the ear depends on the instrument that plays the
score, and it matches what the composer heard only on the same instrument the
score was written against. Roland's Sound Canvas
VA plug-in is that instrument, and it can be placed at the end of the P330 MIDI
route: SC-55 and SC-88 class playback, from an unmodified game.

This is the general procedure in [Using your own MIDI
player](host-midi-player.md) with one part named. The plug-in is not an
application; it is a VSTi and needs a host program to load it, and that host
program is what takes the MIDI input and produces the audio output. Read the
general recipe first for the port plumbing, which is not repeated here.

## The chain

```
guest game  ->  P330 (MPU-401, 0x330)  ->  IzarraVM P330 MIDI receiver
            ->  loopMIDI port
            ->  VST host  ->  Sound Canvas VSTi  ->  host audio device
```

## Procedure

1. Set up the loopMIDI port and select it as the **P330 MIDI receiver**, as in
   [Using your own MIDI player](host-midi-player.md). Confirm the status line
   reads `Ready`.
2. Install a VST host that accepts MIDI input from a system port and can load a
   VSTi. [Falcosoft MIDI Player
   6](https://falcosoft.hu/softwares.html) is one such host and is what this
   recipe was written against; several unrelated programs are called "MIDI
   Player", so take that one from its own site. A DAW or a small plug-in rack
   works equally well, since only two of the host's abilities are used.
3. Install the Sound Canvas VSTi and let the host scan for it, in whichever
   plug-in folder that host is configured to search.
4. In the host, set the MIDI input to the loopMIDI port -- the same port
   IzarraVM sends to, not a second one.
5. Load the Sound Canvas VSTi as the instrument that input feeds.
6. Set the host's audio output to the sound device you are listening on, and
   raise the plug-in's level.
7. Start the game and select **General MIDI**, **MPU-401**, or **MIDI** at port
   `330`. If the game offers **Roland Sound Canvas**, **SC-55**, or **GS**
   explicitly, choose that: those settings send the GS system-exclusive messages
   the plug-in understands, and the port is the same one.

## Notes

- **Which map.** The Sound Canvas VA can present itself as an SC-55, an SC-88,
  or later. A game from 1994 or 1995 was almost certainly heard on an SC-55; a
  later one may have been mixed on an SC-88. If a soundtrack has an instrument
  in the wrong place, try the earlier map before suspecting the route.
- **Latency.** Sound produced by a plug-in host arrives when that host's buffer
  says so, and the delay is the host's, not the emulator's. Reduce the host's
  buffer size if music lags visible action. Digital audio and FM from the
  emulated card are unaffected, so a large buffer shows up as music drifting
  behind sound effects.
- **Two audio streams.** The machine's output and the plug-in host's output are
  separate streams into the same device, and the balance between them is set in
  the host mixer, not by the machine's volume control or by `SNDMIXER`.

## Next

- [Using your own MIDI player](host-midi-player.md): the port plumbing, and
  what to do when the destination is not listed.
- [Nuked-SC55 for Sound Canvas playback](nuked-sc55.md): the same instrument
  run from its own ROMs rather than modelled.
- [MT-32 ROMs with the P330 receiver](mt32-roms.md): the other canonical
  instrument for this era, played inside the emulator.
- [ReSonique 2 sound card manual](../resonique2/manual.md#midi-and-wavetable):
  what the guest is actually driving.
