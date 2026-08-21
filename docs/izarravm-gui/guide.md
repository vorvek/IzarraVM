<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IzarraVM GUI Guide

The IzarraVM desktop application shows the emulated Izarra3000 in a control
panel. The window has a display and a beige panel of controls below the
display. The **Settings** window holds the settings that you do not need in
each session. It opens separate windows for the hotkeys, controller emulation,
and MIDI emulation. This page describes the host application, and not the
emulated machine. See the
[Izarra3000 user manual](../izarra-3000/user-manual.md) for the guest.

## How to start it

```powershell
cargo run -p izarravm -- --config examples/machine.toml
```

The Quick Start section of the README gives the full set of headless flags and
self-test flags (`--headless-config-check`, `--headless-test-rom`,
`--headless-boot-suite`).

## Where the files are

By default, the C: drive folder, `cmos.bin`, and `izarravm.conf` are in the
`~/.izarravm` directory of the user. Thus the program writes nothing into the
working directory. Use **`--portable`** to keep these files beside the
executable, in a `c_drive` folder. This gives an installation that you can
carry on a USB stick.

| File | What it holds | Location |
| --- | --- | --- |
| `c_drive/` | The C: hard disk of the Izarra3000, as a host folder. | `~/.izarravm/`, or beside the executable with `--portable` |
| `cmos.bin` | The 64-byte RTC/NVRAM image: the keyboard layout, the CPU mode, and the other settings that the [setup panel](../izbios/configuration-panel.md) saves. | One level above `c_drive/` |
| `izarravm.conf` | The host GUI preferences (below). | One level above `c_drive/`, with `cmos.bin` |
| `Controller Profiles/` | One TOML file for each controller profile. | In the application state directory. The normal path is `~/.izarravm/Controller Profiles/`. |
| `screenshots/` | PNG files made with the screenshot hotkey. | In the application state directory. The normal path is `~/.izarravm/screenshots/`. |

The controller profiles and screenshots do not use the C: drive location.
They stay in the application state directory when you use `--portable` or a
custom C: drive path.

If `cmos.bin` does not exist, IzarraVM makes a new one with the default
values. If its checksum does not agree, IzarraVM repairs the file. This is the
same checksum that the
[setup panel](../izbios/configuration-panel.md#the-settings-that-cmos-keeps)
writes. At each start, IzarraVM sets the real-time clock in the file from the
host clock.

You set the C: drive path at startup, and not in the GUI. Use `--c-drive`,
`--dosroot`, or the `dos.c_drive` key in a `--config` TOML file, for example
`examples/machine.toml`. The "Open C: folder" control in the GUI opens the
host file manager at the configured path. Use it to copy files to the hard
disk of the guest. It does not change the folder that C: uses.

## The two config files

There are two config files, and they are not equivalent:

| | `izarravm.conf` | The machine config |
| --- | --- | --- |
| What it holds | The host GUI preferences | The hardware of the machine |
| What writes it | The GUI, automatically | You |
| Where it is | Beside `c_drive/`, with `cmos.bin` | Any path that you select |
| When IzarraVM reads it | Always, at startup | Only with `--config <path>` |
| Example | — | `examples/machine.toml` |

IzarraVM refuses an `izarravm.conf` file that you give to `--config`. It shows
a message about the refusal. It does not show a parse error about a key that
you did not know was significant.

A third location also holds machine settings, and it has priority over both
files. It is `cmos.bin`, the NVRAM of the machine, and the machine boots from
it. It holds the CPU speed and the resources of the sound card. You set these
from inside the guest. See [GSWMODE](../toka-dos/commands.md#gswmode) and
[SNDCTRL](../toka-dos/commands.md#sndctrl). A `--cpu` flag or a `--sb-irq`
flag sets the power-on value for a machine with no configuration. After that,
the saved value has priority, and the emulator writes a warning with the names
of the flags that it ignored.

## izarravm.conf

This is a TOML file with the GUI preferences that must stay between runs:

- The master volume
- The **Start in Full Screen** setting
- The CRT emulation style (below)
- The hotkeys that you set for input release, full screen, and screenshots
- The name of the selected controller profile
- The last floppy image, the last CD image, and the last CD folder that you
  mounted
- The state of the control panel: expanded or collapsed
- The P330 receiver, the exact host destination, the P300 SoundFont, and the
  MT-32 ROM paths

New controller mappings are not stored in `izarravm.conf`. The file keeps only
the name of the selected controller profile. Each controller profile is a
separate TOML file in the `Controller Profiles` directory.

If `izarravm.conf` contains an old controller mapping, IzarraVM saves it as a
new profile. It selects the new profile. The generated name starts with **New
Profile**. IzarraVM removes the old mapping from `izarravm.conf` only after it
saves the profile.

Each field in `izarravm.conf` has a default. Thus an old or incomplete file
continues to load after an upgrade. The master volume is the only audio level
in this file. It is the playback level of the host, from 0.0 to 5.0, and 1.0
is unity. See "The volume knob" below.

`amp_gain`, `output_gain`, and `pc_speaker_volume` are removed. They named
levels in the mixer of the machine. A file that contains them loads, and
IzarraVM writes one log line with their names. The next save removes them from
the file. Set those levels from DOS with `SNDMIXER`, which writes the
registers of the card. The guest can read those registers.

## The Settings window

The control panel opens **Settings**. The **APPLICATION SETTINGS** section has
these controls:

- **Start in Full Screen**: starts IzarraVM in full screen at the next start.
  This option is off by default.
- **CRT emulation**: selects the display effect. The table below gives the
  available values.

Three buttons open separate settings windows:

- **Application Hotkeys...**
- **Controller emulation...**
- **MIDI emulation...**

In **APPLICATION HOTKEYS** and **MIDI EMULATION**, **Apply** saves the changes
and keeps the window open. **Back** returns to **Settings**. In **Settings**,
**Accept** saves the changes and closes the window. **Cancel** discards changes
that you did not apply.

### Application Hotkeys

The **APPLICATION HOTKEYS** window has three hotkeys:

| Hotkey | Default | Function |
| --- | --- | --- |
| **Input release** | Win+F2 on Windows, Super+F2 on Linux | Gives keyboard and mouse control back to the host. |
| **Full screen** | Win+F4 on Windows, Super+F4 on Linux | Changes between full screen and windowed mode. |
| **Screenshot** | Win+F12 on Windows, Super+F12 on Linux | Saves the current emulated display as a PNG file. |

The screenshot contains only the emulated display before the CRT effect. It
does not contain the application controls or the **Settings** window. IzarraVM
saves the file in the application state directory. It creates the screenshot
directory when it saves the first PNG file. The normal directory is
`~/.izarravm/screenshots`. IzarraVM uses this directory also with
`--portable` or a custom C: drive path.

The Win key is the key with the Windows logo. On Windows the window writes it
"Win". On Linux the window writes it "Super", the name that Linux desktops
give the same key. The name in `izarravm.conf` is `super` on both. Thus one
file reads correctly on either host. The left key and the right key give the
same modifier.

Click a hotkey button and then press the combination. Ctrl, Shift, Alt, and
Win are modifiers. The key that you press with them completes the hotkey.

A file that still holds the previous defaults, Ctrl+F2 and Ctrl+F11, moves to
the new defaults at the next start. IzarraVM writes one log line for each
move. Set a different combination in this window if you do not want the new
default.

While IzarraVM holds the input, and while this window waits for a hotkey, the
two Win keys do not reach the Windows shell. Thus a press of a Win key does
not open the Start menu and does not take the focus away from the guest. The
Win keys work as usual again when you release the input or when the window
loses the focus. Windows reserves Ctrl+Alt+Del and Win+L. No program can
absorb those two.

### Controller emulation

The **Controller emulation...** button opens **CONTROLLER SETUP**. This window
maps a host gamepad, joystick, or wheel to guest gameport controls, guest keys,
or both.

The device picker is at the top left. The profile controls are to its right:

- The profile picker selects a saved profile.
- **Add new profile** opens a window where you enter a profile name.
- **Delete Profile** asks you to confirm before it deletes the selected profile.

When you add a profile or confirm a deletion, IzarraVM changes the profile file
immediately. **Cancel** does not undo these file operations. **Save** writes the
mapping changes and makes that profile active. **Cancel** discards mapping and
selection changes that you did not save. If you delete the active profile,
IzarraVM disables its mapping immediately. **Cancel** does not restore it.

To make a profile:

1. Select a host device.
2. Select **Add new profile**.
3. Enter a name for the game or the mapping. Then select **Add**.
4. Change the guest target and the assignments.
5. Select **Save**.

To activate a saved profile, select it in the profile picker. Then select
**Save**.

Use a separate profile when a game needs different gamepad-to-keyboard
bindings. Each profile is a TOML file in
`~/.izarravm/Controller Profiles`. This directory is in the application state
directory and does not move with the C: drive.

A profile contains the device identity, guest target, calibrated axes,
buttons, and guest-key combinations. The device identity includes the input
backend, platform, GUID, USB vendor and product IDs when available,
operating-system name, and occurrence among identical devices. After a device
reconnects, its mapping stays inactive until the assigned controls return to
their calibrated rest positions.

The guest target can be Keyboard only, a standard two-axis joystick, a 4 button
gamepad, or wheel and pedals. Keyboard only starts with both stick directions,
the D-pad, the four face buttons, shoulders, triggers, Select, Start, and stick
presses laid out as three compact columns. Click a field and type a guest key or
a Ctrl, Shift, or Alt combination. The left stick and D-pad initially use the
arrow keys. The face buttons initially use Ctrl, Alt, Space, and Shift.

The 4 button gamepad target has four directions and four buttons. Its 4-button
mode drives the four gameport button lines. Its two-button autofire mode keeps A
and B normal, makes C autofire A, and makes D autofire B. Its handedness switch
is a host-mapping preset that reverses both direction axes. Autofire runs at 10
Hz with a 50 percent duty cycle on guest time.

Each guest axis can use all host travel, the center-to-positive half, or
the center-to-negative half. Inversion is applied after that span selection.
Deadzone and saturation are applied before it. The half spans let a centered
gamepad stick act as an accelerator, brake, or clutch. A wheel target exposes
steering, accelerator, brake, and a clutch or spare axis.

Guest-key mappings may contain one key or a combination and can be added to any
gameport target. A combination presses its modifiers first and releases them
last. Mapped keys use the same ownership tracking as the physical keyboard, so
releasing a controller button cannot release a key that is still held on the
keyboard. Focus loss releases only physical keyboard ownership. Digital mappings
from an analog axis use hysteresis: they press at 0.65 and release at 0.50. A
trigger mapping accepts either the analog trigger axis or the equivalent host
button, depending on how the operating system reports that controller.

The compact device preview uses separate face and shoulder views of a generic
gamepad. Pressed buttons and directions turn red, and the two stick caps move
with the live axes. The Input Test tab lists every raw capability reported by
the host backend. If the saved controller is absent,
the guest gameport is disconnected and its guest key sources are released.
`[input].joystick = false` disables all controller input, including guest-key
mappings, but `[input].keyboard = false` disables only the physical keyboard.

### CRT emulation

**CRT emulation** has three values:

| On-screen label | What it does |
| --- | --- |
| **No** | No CRT effect. The image is scaled only. |
| **Subtle** | A light shadow-mask CRT effect. This is the default. |
| **Ye Olde Screene** | A stronger CRT effect, for the full period appearance. |

### MIDI emulation

The **MIDI emulation...** button opens **MIDI EMULATION**. P300 always uses
FluidSynth. You can select its SoundFont. For P330, you can select no receiver,
Munt, or a host MIDI device. The window does not set the levels. The output
stage of the card, the level of the PC speaker, and the balance between the
sources are ReSonique II mixer registers. `SNDMIXER` sets them from DOS. The
volume knob on the machine panel is the playback level of the host. It is
equivalent to the powered speakers on the line-out of the machine.

P300 is a wavetable daughterboard on the internal pin headers of the ReSonique
2. IzarraVM emulates that board with FluidSynth and the embedded FluidR3Mono
bank. You can select a different SF2 or SF3 bank. This does not change the
P330 MIDI route.

P330 is the rear MPU-401/gameport of the Izarra3000. Its receiver selector has
these values:

| Receiver | What it uses |
| --- | --- |
| **Off** | Keeps the P330 MPU active. It sends the messages to no destination. |
| **Munt (MT-32)** | An emulator function that uses MT-32 control ROMs and PCM ROMs that you select. IzarraVM does not include Roland ROMs. |
| **Host device name and ordinal** | The exact MIDI destination in the operating system. Each entry is the MIDI IN side of an external receiver. If the destination is no longer available, IzarraVM does not select a different one. |

IzarraVM follows the host sound device. It does not hold one device. The
default output device can change, or become unavailable, or be unavailable at
the start. In each of those conditions, the machine continues to play into its
own output queue. IzarraVM then opens the stream on the current default device
when one is available, and the guest does not stop.

The P330 external destination above is different. It is a named choice, and
IzarraVM does not replace it.

Each of the two MT-32 ROM boxes accepts a ROM file, or the folder with the ROM
set. IzarraVM identifies an image by its content, and not by its file name. A
set with the names `MT32_CONTROL.ROM` and `MT32_PCM.ROM` loads. A set with
version names loads. A set in half-images loads. The box that you put each
file in is not important.

The ROM boxes are visible only when **Munt (MT-32)** is the P330 receiver.
They are hidden when **Off** or a host MIDI destination is selected.

If a set does not load, the status line gives the cause: an absent control
image, an absent PCM image, or a control image and a PCM image from different
machines. The log gives the name of each file that IzarraVM tried.

P300 and P330 are independent. A change of the P330 receiver sends
all-notes-off to the previous receiver, and does not interrupt FluidSynth.
Each section has its own status line. Thus an absent host destination or an
absent ROM cannot hide a failed SoundFont. A failure in one section does not
hide the related guest MPU.

**Apply** tries a failed synthesizer again, even after no change. Thus IzarraVM
can use a fix that you made outside the emulator, without a restart of the
machine.

IzarraVM resolves the startup settings one field at a time. The order of
priority is: a command-line option, then a key in the `--config` TOML file,
then the saved GUI preference, then the built-in default.

The [recipes](../recipes/index.md) give step-by-step procedures that use these
settings. Two examples are a route from P330 to a player on the host, and a
load of an MT-32 ROM set.

## How to mount removable media

**Floppy (A:)** accepts `.img`, `.ima`, and `.flp` disk images. **Load Floppy
Image** opens the file picker.

**CD-ROM (D:)** accepts three sources. **Load CD Image** opens a file picker
for the first two sources. **Load folder** opens a folder picker for the
third:

- An **ISO** image. IzarraVM mounts it as one data track.
- A **CUE** sheet, with the files that it names. A data track must be a raw
  image (`BINARY`, or `MOTOROLA` for big-endian samples). An audio track can
  be raw. It can also be Ogg Vorbis, MP3, WAV, or FLAC, with one file for each
  track. If a sheet names no file, IzarraVM uses the `.bin` file beside it.
- A **host folder**. IzarraVM makes an ISO9660 image from it. It reads a file
  from the folder when the guest requests the sectors, and does not copy the
  files first. The limit is approximately 650 MB.

IzarraVM decodes an encoded audio track during the playback. The track lengths
in the table of contents come from the audio, and not from the file sizes.
IzarraVM does not use the `FILE` type token of the sheet to identify an
encoded file. It uses the contents, because a ripper frequently writes a token
that does not agree with the file. IzarraVM converts a sample rate that is not
44.1 kHz, and it converts a mono track.

The emulator does not decode some formats, for example Opus, AAC, and Monkey's
Audio. It refuses a file in such a format at the mount, and the message gives
the file name. IzarraVM does not mount a disc with a track that would play
incorrectly. It also refuses a sheet with a layout that it cannot represent:
an encoded file with a data track, or an encoded file that more than one
`TRACK` names.

A CD image and a CD folder cannot be mounted together. A mount of one clears
the other. The floppy drive has no host-folder option. A: accepts image files
only.

## The CD front panel

The transport row in the D: bay is the front panel of the drive. It operates
on the mounted disc directly, and it sends no ATAPI packet command. Thus it
can play a disc that no DOS program opened. A guest can also control the disc.
The two controls share one transport, and the control that operated last holds
it.

The row has four controls, from left to right:

- **Play / pause.** This is one button. It shows the play triangle while the
  drive is stopped or paused, and the pause bars while the disc plays. Play
  from a stop starts at the first audio track, and continues to the end of the
  disc. Play from a pause continues at the held position. The button is
  available when the disc has one audio track or more. On a data-only disc,
  the button is grey.
- **Next track.** This plays from the start of the first audio track after the
  play head, to the end of the disc. A paused drive starts again on the new
  track. The button is grey while the drive is stopped, and on the last audio
  track of the disc.
- **Stop.** This ends the playback and clears the position. It is available
  only while the disc plays or is paused.
- **The level fader.** This is the CD line level on the ReSonique II mixer,
  which the guest can read. It is the pair of CT1745 registers that `SNDMIXER`
  writes from DOS. It is a level in the machine mixer, and not the host
  playback level in "The volume knob" below. The filled part of the track
  shows the current level. The box on the right shows the level in percent,
  and accepts a value in percent. A program in the guest that sets the CD
  level also moves the fader.

The CT1745 registers have 32 steps. Thus the fader moves to the nearest step.

## The volume knob

The slider on the control panel is the playback level of the host. It is
equivalent to the powered speakers on the line-out of the machine. IzarraVM
applies it to the finished mix, before the mix goes to the sound device. It
covers the output of the machine and both MIDI synthesizers together. It is
the only audio level that the emulator owns.

Each level in the machine is a ReSonique II mixer register, and `SNDMIXER` sets
it from DOS. Those levels are the output stage of the card, the PC-speaker
leg, and the balance between the sources. The headless `IZARRAVM_AUDIO_WAV`
capture records the output of the machine before this knob. Thus a recording
does not change when you move the slider.

The travel is from 0% to 500%, and the value box shows percent. 100% is unity:
the mix reaches the sound device as the machine made it. The slider moves in
steps of one percent, thus you can set 100% exactly. Below 100%, the knob
follows a perceptual curve. At 100% and above it, the value is the
multiplication factor. 200% is two times the line level, and 500% is five
times, or +14 dB.

The maximum comes from the quietest correct case. A game with its own setup
program at maximum writes an output level of 27 to the CT1745 mixer of the
card. That level is 8 dB of attenuation. The mix keeps a further 6 dB of
headroom below full scale. Thus the peaks of that game arrive 14 dB down, and
500% puts them at full scale.

**Warning:** gain above 100% can make a sample larger than the sound device
can carry. IzarraVM holds such a sample at full scale, and does not let it
wrap. Thus a loud passage clips as a driven amplifier clips.

`izarravm.conf` stores the knob as `master_volume`, where 1.0 is unity. An
earlier version could save only 0.0 to 1.0. A file from that version loads to
the level that it always meant.

## Other GUI features

- A beige control panel below the display, which you can collapse. It has
  activity LEDs for the floppy, the CD-ROM, and the C: drive.
- A master volume slider (above).
- A CD front panel: play/pause, next track, stop, and a level fader (above).
- A COM1 serial log window. This is a panel that floats, and that you can
  resize. It shows the data that the guest wrote to the emulated serial port.
  Use it for a program that writes its log there and not to the screen.
- An About window with the license information.

The GUI has no drag-and-drop function for a mount. Use **Screenshot** in
**APPLICATION HOTKEYS** to set the PNG screenshot hotkey.

## Next

- [Izarra3000 user manual](../izarra-3000/user-manual.md): the emulated
  machine that this application controls.
- [How to use Toka-DOS](../toka-dos/using-toka-dos.md): what occurs after the
  guest boots.
