<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Troubleshooting & FAQ

## The machine is slow / a game feels sluggish

Most games run at full speed, including in the top GSW-586 mode. Two causes
should be separated before assuming a defect. The Izarra 3000 is a 166 MHz
Pentium-class machine, so software that required more than that in period still
requires more than that here. Demanding 3D at high resolutions and frame rates,
and late titles that recommended a Pentium II or higher, are outside the
machine's capability. If a game instead runs poorly on hardware sufficient for
it, the cause is usually timing rather than throughput, since software written
for a slower machine can misbehave on a faster one. Try the following:

- **A slower CPU mode.** `GSWMODE 486` or `GSWMODE 386` from the DOS prompt
  (see the [command
  reference](toka-dos/commands.md#gswmode)) can produce steadier timing than
  `586` for software that was tuned against a real 486-class machine and
  misbehaves on a faster one. You can also set the boot-time
  default from the [Tab boot menu](izarra-3000/user-manual.md#the-tab-boot-menu)
  or the [Del setup panel](izbios/configuration-panel.md#cpu-mode).
- Check whether the game is CPU-bound versus waiting on the emulated
  hardware. See the [VGA core](vga-core/README.md#limitations) and [VEGA
  technical reference, section 9](vega/vega-technical-reference.md#9-timing-and-fidelity)
  for where the video timing model is and isn't cycle-exact.

## An old program hangs or behaves oddly while waiting for a key

IzarraVM's BIOS halts the CPU when a program asks for a keystroke and none is
ready, rather than spinning. That is invisible to almost everything, since a
keypress wakes it immediately, but a program that masks the timer and keyboard
interrupts before waiting, or that expects time to pass smoothly during the
wait, can notice.

Run `UNHALT` before the program (see the [command
reference](toka-dos/commands.md#unhalt)) to put the BIOS back to spinning, and
`UNHALT /H` to restore the default. Toka-DOS's own idle halt is separate:
`IDLEHALT=0` in `CONFIG.SYS` turns that one off.

## A game doesn't detect my sound card

Check what the game is probing for against the [ReSonique 2
manual](resonique2/manual.md):

- **Digital audio / Sound Blaster**: should auto-detect via the `BLASTER`
  environment variable Toka-DOS sets in `AUTOEXEC.BAT`. If a game insists on
  manual configuration, use base `220`, IRQ `7`, 8-bit DMA `1`, 16-bit DMA
  `5`. Run `SNDCTRL /S` at the prompt to confirm the card's current setting.
- **FM music**: use the AdLib or OPL2/OPL3 option at port `388` if the game
  offers a choice. This is fully modeled.
- **Too loud, too quiet, or the music drowning the effects**: run `SNDMIXER`.
  It provides a fader per source (music, digital audio, CD, MIDI, PC speaker),
  and the levels are saved between sessions. If everything is quiet with MASTER
  already at 10, the `AMP` fader adds output gain in 6 dB positions — at the
  cost of headroom, so a loud game may then distort.
- **General MIDI / wavetable**: select MPU-401 output at port `300`. Toka-DOS
  advertises it as `P300` in `BLASTER`. This represents a daughterboard fitted
  to the ReSonique 2's internal pin headers, which IzarraVM emulates through
  FluidSynth.
- **External MIDI / MT-32**: select MPU-401 output at port `330`. This is the
  Izarra 3000's rear MPU-401/gameport. In Settings, choose Munt as an emulator
  convenience or select the exact host destination connected to the receiver's
  MIDI IN side. See the [ReSonique 2 manual](resonique2/manual.md#midi-and-wavetable).

### The game finds the card but plays a short click, then silence

This is the behaviour of a game that hardwires an IRQ rather than reading
`BLASTER`. Its interrupt handler is on a line the card is not using, so nothing
re-arms the DSP after the first DMA block finishes.

The card ships on IRQ 7 because that is what such games nearly always assume,
but a few require IRQ 5. Change the assignment and try again:

```
SNDCTRL /SBIRQ:5
```

The change takes effect immediately and is remembered across reboots. See
[Changing the card's resources](resonique2/manual.md#changing-the-cards-resources)
for the full-screen version and the other resources.

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

## Where are my files stored?

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
also saves the CPU mode and primary boot device independently. If you only
used Tab, your keyboard layout choice (which only the Del panel edits)
wasn't touched either way.

## Toka-DOS won't boot / COMMAND.COM is missing

Use **Repair Toka-DOS** from the [Del setup panel](izbios/configuration-panel.md#repair-toka-dos).
It reinstalls the Toka-DOS system files from the copy built into ROM,
backing up your `CONFIG.SYS` and `AUTOEXEC.BAT` to `.OLD` files first rather
than silently overwriting them. Anything else on your C: drive is left
alone.

You can also hold **F5** while the message

```
Press F8 to trace or F5 to skip CONFIG.SYS/AUTOEXEC.BAT
```

is on screen, for about two seconds, to boot with neither file processed. That
is the quickest way back in when a line you added to `CONFIG.SYS` stops the
machine booting. **F8** steps through `CONFIG.SYS` one line at a time instead,
asking about each.

Because F5 skips `AUTOEXEC.BAT` too, `PATH` is not set, so DOS tools need their
full path: `C:\DOS\EDIT.COM CONFIG.SYS` rather than `EDIT CONFIG.SYS`.

## Where's the Distira / 3D programmer's guide?

Not written yet. The [VEGA programmer's guide](vega/vega-programmers-guide.md)
currently covers Margo (2D) only, and says so up front. The [technical
reference's Distira section](vega/vega-technical-reference.md#10-distira-3d)
is the current source of truth for what the 3D hardware answers.

## Tracing a guest's `INT n` calls to a driver (development builds)

This is a developer diagnostic, not something most players need. Like the
`IZARRAVM_WATCH_WRITE` store watchpoint, it is compiled out of the default
build, so a normal `cargo build` or release binary pays nothing for it.
Build with the `int-trace` feature and set `IZARRAVM_INT_TRACE` to the
vectors you want, in hex, comma separated:

```
cargo build --release -j8 -p izarravm --features int-trace
IZARRAVM_INT_TRACE=67,21 ./target/release/izarravm --hdd-folder <path> --cpu gsw586 2>trace.log
```

Every traced `INT n` prints its arguments -- the register file at the moment
of the call -- as soon as it fires, then a `  -> ` line with the handler's
answer once execution returns to the instruction after it. Read both halves:
the arguments alone show only that a driver was called, not whether it
answered correctly. This is how the TOKAEMM
shared-pool defect was found. The arguments into `INT 67h` looked fine; the
handler's answer carried `AH=88h`, where a reference EMM manager returns
success.

Two things to know before reading a log. The trace goes to stderr and runs
to thousands of lines a second on a busy vector, so redirect it to a file as
above rather than watching a console. And the address after `ret=` is the
*return* site -- the instruction following the `INT`, not the `INT` itself --
because that is what the answer half matches on, so subtract the
instruction's length to find the call in a disassembly.

Only one call is outstanding at a time. A handler that issues its own traced
`INT` takes over the pending slot, and the outer call's `  -> ` line never
appears: `INT 21h` opening a file calls `INT 13h`, and the example above
traces vectors that nest like that in real guests. A traced `INT` with no
answer under it means the call nested or the handler never returned, not
that the handler answered with nothing.

## Next

- [Izarra 3000 user manual](izarra-3000/user-manual.md)
- [Using Toka-DOS](toka-dos/using-toka-dos.md)
- [IzarraVM GUI guide](izarravm-gui/guide.md)
