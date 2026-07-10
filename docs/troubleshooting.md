<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Troubleshooting & FAQ

## The machine is slow / a game feels sluggish

IzarraVM is early, and CPU performance is the weakest part of it today. The
486 mode at 66 MHz is borderline for demanding software, and the full
GSW-586 speed mode is not yet usable for real-time games. If a game feels
wrong, try:

- **A slower CPU mode.** Counterintuitively, `GSWMODE 486` or even
  `GSWMODE 386` from the DOS prompt (see the [command
  reference](toka-dos/commands.md#gswmode)) can produce steadier timing than
  `586` for software that was tuned against a real 486-class machine and
  gets confused by an unexpectedly fast one. You can also set the boot-time
  default from the [Tab boot menu](izarra-3000/user-manual.md#the-tab-boot-menu)
  or the [Del setup panel](izbios/configuration-panel.md#cpu-mode).
- Check whether the game is CPU-bound versus waiting on the emulated
  hardware. See the [VGA core](vga-core/README.md#limitations) and [VEGA
  technical reference, section 9](vega/vega-technical-reference.md#9-timing-and-fidelity)
  for where the video timing model is and isn't cycle-exact.

## A game doesn't detect my sound card

Check what the game is actually probing for against the [ReSonique 2
manual](resonique2/manual.md):

- **Digital audio / Sound Blaster**: should auto-detect via the `BLASTER`
  environment variable Toka-DOS sets in `AUTOEXEC.BAT`. If a game insists on
  manual configuration, use base `220`, IRQ `5`, 8-bit DMA `1`, 16-bit DMA
  `5`.
- **FM music**: use the AdLib or OPL2/OPL3 option at port `388` if the game
  offers a choice. This is fully modeled.
- **General MIDI / wavetable**: select MPU-401 output at port `300`. Toka-DOS
  advertises it as `P300` in `BLASTER`. This represents a daughterboard fitted
  to the ReSonique 2's internal pin headers, which IzarraVM emulates through
  FluidSynth.
- **External MIDI / MT-32**: select MPU-401 output at port `330`. This is the
  Izarra 3000's rear MPU-401/gameport. In Settings, choose Munt as an emulator
  convenience or select the exact host destination connected to the receiver's
  MIDI IN side. See the [ReSonique 2 manual](resonique2/manual.md#midi-and-wavetable).

### Which ROM files does Munt need?

Choose one control ROM and the PCM ROM from the same row:

| Module | Control ROM | PCM ROM |
| --- | --- | --- |
| MT-32, old generation | `ctrl_mt32_1_04.rom`, `ctrl_mt32_1_05.rom`, `ctrl_mt32_1_06.rom`, or `ctrl_mt32_1_07.rom` | `pcm_mt32.rom` |
| MT-32, new generation | `ctrl_mt32_2_04.rom`, `ctrl_mt32_2_06.rom`, or `ctrl_mt32_2_07.rom` | `pcm_mt32.rom` |
| CM-32L | `ctrl_cm32l_1_00.rom` or `ctrl_cm32l_1_02.rom` | `pcm_cm32l.rom` |
| CM-32LN | `ctrl_cm32ln_1_00.rom` | `pcm_cm32l.rom` |

Settings accepts arbitrary control and PCM paths, so the archive filenames can
be selected directly. For automatic discovery, copy and rename an MT-32 pair
to `~/.izarravm/MT32_CONTROL.ROM` and `~/.izarravm/MT32_PCM.ROM`. Copy and
rename a CM-32L or CM-32LN pair to `~/.izarravm/CM32L_CONTROL.ROM` and
`~/.izarravm/CM32L_PCM.ROM`. Discovery is ASCII case-insensitive and is disabled
under `--portable`; paths selected in Settings still work. The source names in
the table match the `mt32pi` directory in the [Roland MT-32 ROMs
archive](https://archive.org/details/Roland-MT-32-ROMs).

## A 3D-accelerated game doesn't detect Distira

Distira supports both DOS Glide linking models. A static build contains Glide
inside the executable and needs no OVL; the original Voodoo Graphics Tomb
Raider executable is verified this way. Many dynamic builds already include a
compatible `GLIDE2X.OVL` beside the game. That game-local copy takes priority.

Make sure you are running the game's Voodoo Graphics build. For a dynamic
build without its own OVL, place a compatible Voodoo Graphics file at
`~/.izarravm/GLIDE2X.OVL`. The file-name check is ASCII case-insensitive.
IzarraVM exposes it through `C:\DOS`, after the game's current directory in the
normal DOS search order. When neither copy exists, IzarraVM does not inject a
replacement or diagnostic OVL. A
[3DBVoodoo2 driver-disc image](https://archive.org/details/3-dbvoodoo-2_202302)
is archived for reference.

IzarraVM neither downloads nor redistributes ROMs, Glide drivers, or game data.
Archive item metadata is not a license. Use these files only when you have the
lawful right to do so. See [VEGA technical reference, section
10](vega/vega-technical-reference.md#10-distira-3d) for the emulated hardware
contract.

## Where do my files actually live?

By default, everything IzarraVM writes (the C: drive contents, `cmos.bin`,
and `izarravm.conf`) lives under `~/.izarravm`, not next to the executable
or in your current directory. Pass `--portable` at launch to keep them
beside the executable instead. See the [IzarraVM GUI
guide](izarravm-gui/guide.md#where-files-live) for the full breakdown.

## My settings (keyboard layout, CPU mode) didn't stick

Only **Save and Exit** from the [Del setup panel](izbios/configuration-panel.md)
commits keyboard layout and CPU mode to CMOS. **Discard and Exit**, and
pressing Esc from the main setup menu, both throw changes away on purpose.
The [Tab boot menu](izarra-3000/user-manual.md#the-tab-boot-menu)'s Accept
also saves the CPU mode and boot device order independently. If you only
used Tab, your keyboard layout choice (which only the Del panel edits)
wasn't touched either way.

## Toka-DOS won't boot / COMMAND.COM is missing

Use **Repair Toka-DOS** from the [Del setup panel](izbios/configuration-panel.md#repair-toka-dos).
It reinstalls the Toka-DOS system files from the copy built into ROM,
backing up your `CONFIG.SYS` and `AUTOEXEC.BAT` to `.OLD` files first rather
than silently overwriting them. Anything else on your C: drive is left
alone.

## Where's the Distira / 3D programmer's guide?

Not written yet. The [VEGA programmer's guide](vega/vega-programmers-guide.md)
currently covers Margo (2D) only, and says so up front. The [technical
reference's Distira section](vega/vega-technical-reference.md#10-distira-3d)
is the current source of truth for what the 3D hardware answers.

## Next

- [Izarra 3000 user manual](izarra-3000/user-manual.md)
- [Using Toka-DOS](toka-dos/using-toka-dos.md)
- [IzarraVM GUI guide](izarravm-gui/guide.md)
