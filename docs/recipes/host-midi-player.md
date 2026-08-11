<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Using your own MIDI player

The Izarra 3000's rear MPU-401 port, `0x330`, is published to DOS software as
`P330`. IzarraVM does not interpret what the guest writes there: it hands the
byte stream to whatever receiver you name, and one of the choices is a MIDI
output port belonging to the host operating system. Anything on the host that
can be reached through a MIDI input port -- a software synthesiser, a sequencer,
a player, a hardware module on a USB interface -- can therefore play the game's
music.

What stands between the two is a property of the host, not of the emulator. On
Windows, an application's MIDI **input** ports and its MIDI **output** ports are
separate system endpoints. IzarraVM opens an output; a player opens an input;
nothing in Windows joins one to the other. A virtual MIDI cable is the standard
way to supply that connection, and this procedure uses
[loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) (Tobias
Erichsen), which is free for personal use.

## Procedure

1. Install loopMIDI on the host and start it.
2. Create one port in loopMIDI: enter a name and add it. Any name will do; a
   distinct one, such as `IzarraVM`, is easier to pick out of a list later. The
   port exists for as long as loopMIDI is running, and loopMIDI recreates its
   saved ports on each start.
3. Create the port before opening IzarraVM's config modal. The panel lists the
   host's MIDI destinations when it opens and does not rescan while it is
   open. If the port is already there but the list is not, close the panel and
   open it again.
4. In IzarraVM, open the config modal from the control panel and find **P330
   MIDI receiver** in the Audio section. Select the loopMIDI port by name. Host
   destinations are listed as their name followed by an ordinal, `IzarraVM #1`,
   which distinguishes two interfaces reporting the same name.
5. Press **Accept**. The status line under the selector reads `Ready` when the
   port opened.
6. In the MIDI player, select the same loopMIDI port as its MIDI **input**. The
   player has to be listening before the guest starts a song; most players open
   the input when their window opens, not when playback is pressed.
7. Start the game and select **General MIDI**, **MPU-401**, or **MIDI** at port
   `330` in its own setup program. Music now leaves the guest, crosses the
   loopMIDI port, and is played by the host application, through the host
   application's own audio device.

The selection is host-side preference and is written to `izarravm.conf`, so it
survives a restart. The named destination is a fixed choice and is never
silently replaced: if the port is missing at power-on -- loopMIDI not started
yet, most commonly -- the status line reads that the selected host MIDI
destination is not available, and P330 stays silent until you press **Accept**
again with the port present.

## Notes

- **The host's own audio.** Music routed this way is produced by the player, so
  the machine's volume control and the ReSonique 2 mixer registers do not act on
  it. Set its level in the player or in the host mixer. Digital audio and FM
  still come from the emulated card and are still mixed by it.
- **Effects and music at once.** Nothing is diverted but the `0x330` stream. A
  game playing sound effects through the Sound Blaster and music through the
  MPU-401 keeps both.
- **A real cable works, and is not necessary.** If the host has a MIDI
  interface, a DIN lead from its MIDI OUT to its MIDI IN joins the two endpoints
  in hardware and this procedure otherwise stands. It has no advantage over the
  virtual cable: the music leaves the machine and comes back over a 31.25 kbaud
  serial wire. Use it when the receiver is genuinely external.
- **Two other receivers exist.** The P330 selector also offers **Off**, and
  **Munt (MT-32)**, which is played inside the emulator rather than by the host.
  See [MT-32 ROMs with the P330 receiver](mt32-roms.md). The internal wavetable
  daughterboard at `P300` is a separate path and is not affected by any of this.

## Next

- [The Roland Sound Canvas VSTi](sound-canvas-vsti.md): this route with a
  specific instrument at the end of it.
- [Nuked-SC55 for Sound Canvas playback](nuked-sc55.md): the same instrument,
  run from its own ROMs.
- [IzarraVM GUI guide](../izarravm-gui/guide.md#the-config-modal): every field
  in the Audio section, and the status messages.
- [ReSonique 2 sound card manual](../resonique2/manual.md#midi-and-wavetable):
  the two MPU-401 ports as the guest sees them.
