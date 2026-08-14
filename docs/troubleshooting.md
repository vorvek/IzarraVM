<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Troubleshooting and FAQ

## The machine is slow, or a game is slow

Most games run at full speed, also in the top GSW-586 mode. Before you report
a defect, separate the two causes below.

The Izarra3000 is a 166 MHz Pentium-class machine. Software that needed more
than that in the period also needs more than that here. 3D at a high
resolution and a high frame rate is more than the machine can do. A late game
that recommended a Pentium II or a faster CPU is also more than the machine
can do.

A game can also run badly on hardware that is sufficient for it. The cause is
then usually the timing, and not the speed, because software for a slower
machine can operate incorrectly on a faster machine. Do these steps:

- **Select a slower CPU mode.** Run `GSWMODE 486` or `GSWMODE 386` at the DOS
  prompt. See the [command reference](toka-dos/commands.md#gswmode). Software
  for a real 486-class machine can have more constant timing at these speeds
  than at `586`. You can also set the boot-time default from the
  [Tab boot menu](izarra-3000/user-manual.md#the-tab-boot-menu) or the
  [Del setup panel](izbios/configuration-panel.md#cpu-mode).
- **Find the limit.** The CPU can limit the game, or the game can wait for the
  emulated hardware. See the [VGA core](vga-core/README.md#limitations) and
  [VEGA technical reference, section 9](vega/vega-technical-reference.md#9-timing-and-fidelity).
  They give the parts of the video timing model that are cycle-exact, and the
  parts that are not.

## An old program stops or operates incorrectly while it waits for a key

A program can ask for a keystroke when no keystroke is available. The IzarraVM
BIOS then halts the CPU, and does not use a loop. Almost no program detects
this, because a keypress starts the CPU immediately. But two types of program
can detect it. The first type masks the timer interrupt and the keyboard
interrupt before the wait. The second type expects the time to increase
smoothly during the wait.

Run `UNHALT` before the program to make the BIOS use a loop again. See the
[command reference](toka-dos/commands.md#unhalt). Run `UNHALT /H` to set the
default again. The idle halt of Toka-DOS is a different function.
`IDLEHALT=0` in `CONFIG.SYS` stops that one.

## A game does not detect the sound card

Compare the device that the game looks for with the
[ReSonique II manual](resonique2/manual.md):

- **Digital audio / Sound Blaster**: the game must find the card from the
  `BLASTER` environment variable that Toka-DOS sets in `AUTOEXEC.BAT`. If a
  game needs manual values, use base `220`, IRQ `7`, 8-bit DMA `1`, and 16-bit
  DMA `5`. Run `SNDCTRL /S` at the prompt to see the current values of the
  card.
- **FM music**: select the AdLib or OPL2/OPL3 option at port `388`, if the
  game gives a choice. IzarraVM emulates this hardware fully.
- **The sound is too loud or too quiet, or the music is louder than the
  effects**: run `SNDMIXER`. It has a fader for each source: music, digital
  audio, CD, MIDI, and PC speaker. It keeps the levels between sessions. If
  the sound stays quiet with MASTER at 10, the `AMP` fader adds output gain in
  steps of 6 dB. That gain uses the headroom, thus a loud game can then
  distort.
- **General MIDI / wavetable**: select MPU-401 output at port `300`. Toka-DOS
  gives this port as `P300` in `BLASTER`. This port is a daughterboard on the
  internal pin headers of the ReSonique II, and IzarraVM emulates it with
  FluidSynth.
- **External MIDI / MT-32**: select MPU-401 output at port `330`. This is the
  rear MPU-401/gameport of the Izarra3000. In Settings, select Munt, or select
  the exact host destination on the MIDI IN side of the receiver. See the
  [ReSonique II manual](resonique2/manual.md#midi-and-wavetable), and the
  recipes for [your own MIDI player](recipes/host-midi-player.md) and for
  [MT-32 ROMs](recipes/mt32-roms.md).

### The game finds the card, but it plays a short click and then silence

This is the behavior of a game with a fixed IRQ in its code, which does not
read `BLASTER`. Its interrupt handler is on a line that the card does not use.
Thus nothing sets the DSP again after the first DMA block.

The card uses IRQ 7, because almost all such games expect IRQ 7. But some of
them need IRQ 5. Change the value and try again:

```
SNDCTRL /SBIRQ:5
```

The change has an effect immediately, and the machine keeps it after a
restart. See
[How to change the card resources](resonique2/manual.md#how-to-change-the-card-resources)
for the full-screen version and the other resources.

### Which ROM files does Munt need?

Select one control ROM and the PCM ROM from the same row:

| Module | Control ROM | PCM ROM |
| --- | --- | --- |
| MT-32, old generation | `ctrl_mt32_1_04.rom`, `ctrl_mt32_1_05.rom`, `ctrl_mt32_1_06.rom`, or `ctrl_mt32_1_07.rom` | `pcm_mt32.rom` |
| MT-32, new generation | `ctrl_mt32_2_04.rom`, `ctrl_mt32_2_06.rom`, or `ctrl_mt32_2_07.rom` | `pcm_mt32.rom` |
| CM-32L | `ctrl_cm32l_1_00.rom` or `ctrl_cm32l_1_02.rom` | `pcm_cm32l.rom` |
| CM-32LN | `ctrl_cm32ln_1_00.rom` | `pcm_cm32l.rom` |

The Settings window accepts any control path and any PCM path. Thus you can
select the archive file names directly.

For automatic discovery, copy an MT-32 pair and give the copies the names
`~/.izarravm/MT32_CONTROL.ROM` and `~/.izarravm/MT32_PCM.ROM`. For a CM-32L or
a CM-32LN pair, use the names `~/.izarravm/CM32L_CONTROL.ROM` and
`~/.izarravm/CM32L_PCM.ROM`. The discovery ignores the letter case of ASCII
characters. `--portable` disables the discovery, but a path in the Settings
window continues to operate. The names in the table are the names in the
`mt32pi` directory of the
[Roland MT-32 ROMs archive](https://archive.org/details/Roland-MT-32-ROMs).

## A 3D game does not detect Distira

Distira supports both DOS Glide link models. A static build holds Glide in the
executable and needs no OVL file. The original Voodoo Graphics executable of
Tomb Raider is verified in this way. Many dynamic builds include a compatible
`GLIDE2X.OVL` beside the game, and that local copy has priority.

First, make sure that you run the Voodoo Graphics build of the game. For a
dynamic build with no OVL file, put a compatible Voodoo Graphics file at
`~/.izarravm/GLIDE2X.OVL`. The file-name check ignores the letter case of
ASCII characters. IzarraVM makes the file available through `C:\DOS`, after
the current directory of the game in the normal DOS search order. If neither
copy exists, IzarraVM supplies no replacement OVL and no diagnostic OVL. A
[3DBVoodoo2 driver-disc image](https://archive.org/details/3-dbvoodoo-2_202302)
is in an archive, for reference.

IzarraVM does not download ROMs, Glide drivers, or game data, and it does not
distribute them. The metadata of an archive item is not a license. Use these
files only if you have the legal right to use them. See
[VEGA technical reference, section 10](vega/vega-technical-reference.md#10-distira-3d)
for the contract of the emulated hardware.

## Where does IzarraVM keep the files?

By default, IzarraVM writes all of its files in `~/.izarravm`. These files are
the contents of the C: drive, `cmos.bin`, and `izarravm.conf`. It does not
write them beside the executable or in the current directory. Use
`--portable` at the start to keep them beside the executable. See the
[IzarraVM GUI guide](izarravm-gui/guide.md#where-the-files-are) for the full
list.

## The settings (keyboard layout, CPU mode) were not saved

Only **Save and Exit** in the
[Del setup panel](izbios/configuration-panel.md) writes the keyboard layout
and the CPU mode to CMOS. **Discard and Exit** discards the changes, and Esc
on the main setup menu does the same. The Accept row of the
[Tab boot menu](izarra-3000/user-manual.md#the-tab-boot-menu) saves the CPU
mode and the primary boot device, independently of the panel. If you used only
the Tab menu, the keyboard layout did not change, because only the Del panel
edits the layout.

## Toka-DOS does not boot, or COMMAND.COM is absent

Use **Repair Toka-DOS** in the
[Del setup panel](izbios/configuration-panel.md#repair-toka-dos). It installs
the Toka-DOS system files again, from the copy in the ROM. First it changes
the names of your `CONFIG.SYS` and `AUTOEXEC.BAT` to `.OLD` files, and thus it
does not write over them. It does not change the other files on your C: drive.

You can also start the machine with neither startup file. Hold **F5** while
this message is on the screen, for approximately two seconds:

```
Press F8 to trace or F5 to skip CONFIG.SYS/AUTOEXEC.BAT
```

This is the fastest method when a line that you added to `CONFIG.SYS` stops
the boot. **F8** processes `CONFIG.SYS` one line at a time, and asks about
each line.

F5 also skips `AUTOEXEC.BAT`, thus `PATH` is not set. Give the full path of a
DOS tool: `C:\DOS\EDIT.COM CONFIG.SYS`, and not `EDIT CONFIG.SYS`.

## Where is the Distira 3D programmer's guide?

It does not exist yet. The
[VEGA programmer's guide](vega/vega-programmers-guide.md) covers Margo (2D)
only, and its first section says so. The
[Distira section of the technical reference](vega/vega-technical-reference.md#10-distira-3d)
is the authority on the answers of the 3D hardware.

## How to trace the `INT n` calls of a guest (development builds)

This is a developer diagnostic. Most players do not need it. The default build
does not include it, as it does not include the `IZARRAVM_WATCH_WRITE` store
watchpoint. Thus a normal `cargo build` and a release binary have no cost from
it. Build with the `int-trace` feature. Then set `IZARRAVM_INT_TRACE` to the
vectors that you want, in hexadecimal, separated by commas:

```
cargo build --release -j8 -p izarravm --features int-trace
IZARRAVM_INT_TRACE=67,21 ./target/release/izarravm --hdd-folder <path> --cpu gsw586 2>trace.log
```

Each traced `INT n` prints its arguments immediately. The arguments are the
register file at the moment of the call. When execution returns to the
instruction after the `INT`, the trace prints a `  -> ` line with the answer
of the handler. Read both halves. The arguments show only that a program
called a driver. They do not show a correct answer.

This method found the TOKAEMM shared-pool defect. The arguments into `INT 67h`
were correct. But the answer of the handler had `AH=88h`, and a reference EMM
manager returns success.

Read these two notes before you read a log. First, the trace goes to stderr,
and it can write thousands of lines each second on a busy vector. Send it to a
file, as above, and do not watch it on a console. Second, the address after
`ret=` is the return address. It is the instruction after the `INT`, and not
the `INT` itself. The answer half of the trace uses that address. To find the
call in a disassembly, subtract the length of the instruction.

The trace holds one call at a time. If a handler makes its own traced `INT`,
that call takes the slot, and the `  -> ` line of the first call does not
appear. For example, `INT 21h` calls `INT 13h` when it opens a file, and the
example above traces vectors that nest in this way. Thus a traced `INT` with
no answer below it means one of two things: the call nested, or the handler
did not return. It does not mean that the handler gave an empty answer.

## Next

- [Izarra3000 user manual](izarra-3000/user-manual.md)
- [How to use Toka-DOS](toka-dos/using-toka-dos.md)
- [IzarraVM GUI guide](izarravm-gui/guide.md)
