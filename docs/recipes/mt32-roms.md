<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# MT-32 ROMs with the P330 receiver

**Munt (MT-32)** is one of the receivers the P330 rear MPU-401 can be pointed
at. Unlike a host MIDI destination, it plays inside the emulator: its audio
joins the machine's own output, so no second application and no virtual MIDI
cable are involved.

It requires two ROM images from a Roland MT-32 or CM-32L -- a control ROM and a
PCM ROM. **IzarraVM does not include Roland ROMs** and neither downloads nor
redistributes them; see [Which ROM files does Munt
need?](../troubleshooting.md#which-rom-files-does-munt-need) for which images
belong to which machine. This page covers what the loader accepts once you have
a set.

## What the loader accepts

The two boxes in the config modal are labelled **MT-32 control ROM** and
**MT-32 PCM ROM**, but neither is a fixed slot. Each box takes either a single
ROM file or the folder the ROM set lives in, and the images are identified by
content, not by filename. In practice:

- **Naming does not matter.** `MT32_CONTROL.ROM` and `MT32_PCM.ROM`, a set named
  for its firmware version, and a set named in some other scheme entirely all
  load. The loader asks the synthesiser library what each file is; it does not
  read the name.
- **Which box you use does not matter.** Put the control image in the PCM box
  and the set still loads. Pointing both boxes at the same folder is the
  simplest way to load any set.
- **Split half-images are merged.** A ROM published as two halves cannot be
  recognised a file at a time, so files that were not identified alone are then
  tried in pairs, in both orders, against the files sitting alongside them.
  This is why a folder often succeeds where naming two files does not.
- **The set's own folder is searched.** If what you named is not enough for a
  complete set, the folders those choices sit in are scanned as well. What you
  named is always tried first.

## Procedure

1. Put the ROM set in a folder of its own, with nothing else in it. A folder of
   unrelated files is scanned too, and the scan stops after the first 64 files.
2. Open the config modal from the control panel and find the Audio section.
3. Point **MT-32 control ROM** and **MT-32 PCM ROM** at the set. Naming the
   folder in both boxes is enough; naming the two files works when they are
   whole images.
4. Select **Munt (MT-32)** as the **P330 MIDI receiver**. The entry is
   selectable only once both boxes name something that exists; until then it
   reads `Munt (MT-32) (missing ROMs)` and the two ROM boxes stay on screen
   whichever receiver is selected.
5. Press **Accept** and read the status line under the selector. `Ready` means
   the synthesiser opened.
6. Start the game and select **MT-32**, **LAPC-I**, or **MPU-401** at port
   `330`.

## Without naming anything

If neither box has ever been filled in, IzarraVM looks once at startup in the
state directory that holds `cmos.bin` and `izarravm.conf`, and takes the first
of these it finds:

- `MT32_CONTROL.ROM` with `MT32_PCM.ROM`, or `CM32L_CONTROL.ROM` with
  `CM32L_PCM.ROM`, sitting loose in that directory. The name check ignores
  case.
- A sub-folder named `mt32`, `cm32l`, `roms`, or `mt32-roms`, again ignoring
  case. The folder is handed to the loader whole, so the files inside it can be
  named anything and may be split halves.

This is a convenience for a set dropped into place, not a search: it does not
run under `--portable`, it does not look anywhere else, and a path already in
either box is left alone rather than overridden.

**Accept** is also the retry. A set fixed outside the emulator -- a missing file
copied in, a folder renamed -- is picked up by pressing **Accept** again, with
or without a change in the panel, and without restarting the machine.

## When it does not load

The status line names the requirement that failed rather than reporting a
general failure, and the log lists every file that was tried.

| Status line | What it means |
| --- | --- |
| `Select both MT-32 ROMs. P330 output is silent.` | One or both boxes are empty. The receiver is Munt, so the guest's MPU-401 still answers, but nothing plays. |
| `A selected MT-32 ROM path does not exist.` | A box names a file or folder that is not there. Check for a moved or renamed set. |
| `No MT-32 control ROM was recognised. Point either box at the ROM set's folder.` | Nothing offered was a control image. Most often the set is split into halves and only one file was named; naming the folder lets the halves be paired. |
| `The control ROM loaded but no PCM ROM was recognised. Add the PCM image to the set.` | Half a set. The PCM image is genuinely absent from what was searched. |
| `The control and PCM ROMs are from different machines. Use one matched set.` | Both images were identified, and the synthesiser refused the pair -- an MT-32 control ROM with a CM-32L PCM ROM, or the reverse. Use both images from one machine. |

A single file that is recognised as nothing is reported as that file, since
naming it is more use than a list of one.

## Notes

- **The P300 wavetable is independent.** Changing the P330 receiver does not
  disturb the internal daughterboard, and each section carries its own status
  line, so a ROM problem cannot hide a failed SoundFont. Changing the receiver
  sends all-notes-off to the one being left.
- **MT-32 is not General MIDI.** Games from roughly 1988 to 1992 that offer an
  MT-32 option were written for its instrument layout. A General MIDI score
  played through it lands on the wrong instruments. For those, use the internal
  wavetable at `P300` or a host receiver; see [Using your own MIDI
  player](host-midi-player.md).

## Next

- [Using your own MIDI player](host-midi-player.md): sending P330 to the host
  instead.
- [IzarraVM GUI guide](../izarravm-gui/guide.md#the-config-modal): the Audio
  section in full.
- [ReSonique 2 sound card manual](../resonique2/manual.md#midi-and-wavetable):
  the MPU-401 ports as the guest sees them.
