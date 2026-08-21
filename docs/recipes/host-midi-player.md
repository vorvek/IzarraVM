<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# How to use your own MIDI player

The rear MPU-401 port of the Izarra3000 is `0x330`. DOS software finds it as
`P330`. IzarraVM does not interpret the data that the guest writes there. It
sends the byte stream to the receiver that you select. One of the receivers is
a MIDI output port of the host operating system. Thus any host program with a
MIDI input port can play the music of the game. Examples are a software
synthesizer, a sequencer, a player, and a hardware module on a USB interface.

One problem remains, and it is a property of the host, not of the emulator. On
Windows, the MIDI **input** ports and the MIDI **output** ports of an
application are separate system endpoints. IzarraVM opens an output, and a
player opens an input. Windows does not connect the two. A virtual MIDI cable
makes that connection. This procedure uses
[loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) (Tobias
Erichsen), which is free for personal use.

## Procedure

1. Install loopMIDI on the host, and start it.
2. Make one port in loopMIDI. Type a name, and then add the port. Any name is
   permitted. A special name, for example `IzarraVM`, is easier to find in a
   list. The port exists while loopMIDI operates, and loopMIDI makes its saved
   ports again at each start.
3. Make the port before you open **Settings** in IzarraVM.
4. Open **Settings**. It reads the MIDI destinations of the host when it opens.
   It does not read them again while it is open. If the list does not show the
   port, close **Settings** and open it again.
5. Select **MIDI emulation...**.
6. Select the loopMIDI port in **P330 MIDI receiver**. A host destination has
   its name and then a number, for example
   `IzarraVM #1`. The number separates two interfaces with the same name.
7. Select **Apply**. The status line below the selector shows `Ready` when the
   port opens.
8. In the MIDI player, select the same loopMIDI port as its MIDI **input**.
   The player must listen before the guest starts a song. Most players open
   the input when their window opens, and not at the start of the playback.
9. Start the game.
10. In the setup program of the game, select **General MIDI**, **MPU-401**, or
    **MIDI** at port `330`. The music then goes through the loopMIDI port to
    the host application. The host application plays it through its own audio
    device.

The selection is a host preference, and IzarraVM writes it to
`izarravm.conf`. Thus it stays after a restart. The named destination is a
fixed choice, and IzarraVM does not replace it.

The port can be absent at power-on. The usual cause is that loopMIDI did not
start. The status line then reports that the selected host MIDI destination is
not available, and P330 stays silent. Select **Apply** again after the port
exists.

## Notes

- **The audio of the host.** The player makes this music. Thus the volume
  control of the machine and the ReSonique II mixer registers do not change it.
  Set its level in the player or in the host mixer. The digital audio and the
  FM continue to come from the emulated card, and the card continues to mix
  them.
- **Effects and music together.** IzarraVM sends only the `0x330` stream to
  the host. A game keeps both paths: the sound effects through the Sound
  Blaster, and the music through the MPU-401.
- **A real cable operates, but it is not necessary.** If the host has a MIDI
  interface, a DIN cable from its MIDI OUT to its MIDI IN connects the two
  endpoints, and the remainder of this procedure is the same. A real cable has
  no advantage over the virtual cable, because the music goes out and comes
  back on a 31.25 kbaud serial wire. Use a real cable when the receiver is
  external.
- **There are two other receivers.** The P330 selector also has **Off** and
  **Munt (MT-32)**. The emulator plays Munt, and the host does not. See
  [MT-32 ROMs with the P330 receiver](mt32-roms.md). The internal wavetable
  daughterboard at `P300` is a separate path, and this procedure does not
  change it.

## Next

- [The Roland Sound Canvas VSTi](sound-canvas-vsti.md): this route with one
  specified instrument at its end.
- [Nuked-SC55 for Sound Canvas playback](nuked-sc55.md): the same instrument,
  from its own ROMs.
- [IzarraVM GUI guide](../izarravm-gui/guide.md#the-settings-window): the MIDI
  emulation settings and the status messages.
- [ReSonique II sound card manual](../resonique2/manual.md#midi-and-wavetable):
  the two MPU-401 ports, as the guest finds them.
