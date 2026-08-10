<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Recipes

The manuals in this set describe what the Izarra 3000 and its host application
are. These pages describe how to get particular jobs done with them: a
procedure, in order, with the reason for each step where the reason is not
obvious.

A recipe assumes the machine is already running and that you have read the
manual for the part it touches. Where a setting is named, it is named as the
program prints it.

## Sound and music

- **[Using your own MIDI player](host-midi-player.md)**: send the guest's
  MPU-401 music to a synthesiser or player running on the host, using a virtual
  MIDI cable.
- **[The Roland Sound Canvas VSTi](sound-canvas-vsti.md)**: the same route, ending
  at an SC-55/88-class instrument, the hardware most mid-1990s General MIDI
  soundtracks were written for.
- **[Nuked-SC55 for Sound Canvas playback](nuked-sc55.md)**: that instrument
  again, emulated from its own ROMs instead of modelled.
- **[MT-32 ROMs with the P330 receiver](mt32-roms.md)**: what the ROM loader
  accepts, and what each failure message means.

## Next

- [IzarraVM GUI guide](../izarravm-gui/guide.md): the host application and its
  config modal.
- [ReSonique 2 sound card manual](../resonique2/manual.md): the card behind
  P300 and P330.
- [Troubleshooting & FAQ](../troubleshooting.md): problems that are not a
  setup procedure.
