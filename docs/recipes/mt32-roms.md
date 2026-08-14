<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# MT-32 ROMs with the P330 receiver

**Munt (MT-32)** is one of the receivers for the rear P330 MPU-401 port. The
emulator plays it internally, and a host MIDI destination is different. The
audio of Munt goes into the output of the machine. Thus you do not need a
second application, and you do not need a virtual MIDI cable.

Munt needs two ROM images from a Roland MT-32 or a Roland CM-32L: a control
ROM and a PCM ROM. **IzarraVM does not include Roland ROMs.** It does not
download them, and it does not distribute them. See
[Which ROM files does Munt need?](../troubleshooting.md#which-rom-files-does-munt-need)
for the images of each machine. This page describes what the loader accepts
after you have a set.

## What the loader accepts

The two boxes in the config modal have the labels **MT-32 control ROM** and
**MT-32 PCM ROM**. But neither box is a fixed slot. Each box accepts one ROM
file, or the folder with the ROM set. The loader identifies an image by its
content, and not by its file name. Thus:

- **The file name is not important.** `MT32_CONTROL.ROM` and `MT32_PCM.ROM`
  load. A set with firmware version names loads. A set with other names loads.
  The loader asks the synthesizer library to identify each file. It does not
  read the name.
- **The box is not important.** The set loads if you put the control image in
  the PCM box. The simplest method is to give the same folder to both boxes.
- **The loader joins split half-images.** The loader cannot identify a ROM in
  two halves one file at a time. Thus it tries the unidentified files in
  pairs, in both orders, with the files beside them. For this reason, a folder
  frequently loads when two file names do not.
- **The loader searches the folder of the set.** If your selection is not a
  complete set, the loader also scans the folders that hold your selection. It
  always tries your selection first.

## Procedure

1. Put the ROM set in its own folder, with no other file in it. The loader
   also scans a folder with other files, but that scan stops after the first
   64 files.
2. Open the config modal from the control panel, and find the Audio section.
3. Give the set to **MT-32 control ROM** and **MT-32 PCM ROM**. The folder in
   both boxes is sufficient. The two file names are also sufficient if each
   file is a whole image.
4. Select **Munt (MT-32)** as the **P330 MIDI receiver**. You can select this
   entry only after both boxes name a file or a folder that exists. Before
   that, the entry shows `Munt (MT-32) (missing ROMs)`. The two ROM boxes stay
   on the screen with each receiver.
5. Press **Accept**, and read the status line below the selector. `Ready`
   means that the synthesizer opened.
6. Start the game, and select **MT-32**, **LAPC-I**, or **MPU-401** at port
   `330`.

## Automatic discovery

If both boxes are always empty, IzarraVM looks one time at startup. It looks
in the state directory that holds `cmos.bin` and `izarravm.conf`. It takes the
first of these that it finds:

- `MT32_CONTROL.ROM` with `MT32_PCM.ROM`, or `CM32L_CONTROL.ROM` with
  `CM32L_PCM.ROM`, directly in that directory. The name check ignores the
  letter case.
- A sub-folder with the name `mt32`, `cm32l`, `roms`, or `mt32-roms`. This
  check also ignores the letter case. IzarraVM gives the full folder to the
  loader. Thus the files in it can have any name, and they can be split
  halves.

This function is for a set that you copy into that directory. It is not a
search. It does not operate under `--portable`, and it looks in no other
directory. It does not change a path that is already in a box.

**Accept** is also the retry control. You can correct a set outside the
emulator. For example, you can copy an absent file into the folder, or change
the name of the folder. Press **Accept** again to load the corrected set. A
change in the panel is not necessary, and a restart of the machine is not
necessary.

## When the set does not load

The status line gives the requirement that failed. It does not give a general
failure message. The log lists each file that the loader tried.

| Status line | What it means |
| --- | --- |
| `Select both MT-32 ROMs. P330 output is silent.` | One box or both boxes are empty. The receiver is Munt, thus the guest MPU-401 continues to answer, but nothing plays. |
| `A selected MT-32 ROM path does not exist.` | A box names a file or a folder that is not there. Look for a set that moved or that has a different name. |
| `No MT-32 control ROM was recognised. Point either box at the ROM set's folder.` | No file was a control image. Usually the set is in halves, and you named only one file. Name the folder, and the loader can join the halves. |
| `The control ROM loaded but no PCM ROM was recognised. Add the PCM image to the set.` | The set is not complete. The PCM image is not in the files that the loader searched. |
| `The control and PCM ROMs are from different machines. Use one matched set.` | The loader identified both images, and the synthesizer refused the pair. One example is an MT-32 control ROM with a CM-32L PCM ROM. Use two images from one machine. |

If the loader does not recognize one file, the message names that file. This
is more useful than a list with one entry.

## Notes

- **The P300 wavetable is independent.** A change of the P330 receiver does
  not change the internal daughterboard. Each section has its own status line,
  thus a ROM problem cannot hide a failed SoundFont. A change of the receiver
  sends all-notes-off to the previous receiver.
- **MT-32 is not General MIDI.** A game from approximately 1988 to 1992 with
  an MT-32 option uses the instrument layout of the MT-32. A General MIDI
  score through an MT-32 gives the incorrect instruments. For a General MIDI
  score, use the internal wavetable at `P300`, or a host receiver. See
  [How to use your own MIDI player](host-midi-player.md).

## Next

- [How to use your own MIDI player](host-midi-player.md): how to send P330 to
  the host.
- [IzarraVM GUI guide](../izarravm-gui/guide.md#the-config-modal): the full
  Audio section.
- [ReSonique II sound card manual](../resonique2/manual.md#midi-and-wavetable):
  the MPU-401 ports, as the guest finds them.
