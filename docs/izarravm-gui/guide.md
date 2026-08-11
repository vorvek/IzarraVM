<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IzarraVM GUI Guide

IzarraVM's desktop application presents the emulated Izarra 3000 in a control
panel: a display, a beige panel of controls below it, and a config modal for
the settings not needed every session. This page covers the host
application, not the emulated machine. See the [Izarra 3000 user
manual](../izarra-3000/user-manual.md) for what happens inside the guest.

## Starting it

```powershell
cargo run -p izarravm -- --config examples/machine.toml
```

See the README's Quick Start for the full set of headless and self-test
flags (`--headless-config-check`, `--headless-test-rom`,
`--headless-boot-suite`).

## Where files live

By default, the C: drive folder, `cmos.bin`, and `izarravm.conf` all live
under the per-user `~/.izarravm` directory, so running the binary from any
working directory leaves nothing behind there. Pass **`--portable`** to keep
them beside the executable instead, in a `c_drive` folder next to it, for a
self-contained install you can carry on a USB stick.

| File | What it holds | Location |
| --- | --- | --- |
| `c_drive/` | The Izarra 3000's C: hard disk, as a real host folder. | `~/.izarravm/` (or beside the executable with `--portable`) |
| `cmos.bin` | The 64-byte RTC/NVRAM image: keyboard layout, CPU mode, and the rest of the settings the [setup panel](../izbios/configuration-panel.md) persists. | One level above `c_drive/` |
| `izarravm.conf` | Host-side GUI preferences (below). | One level above `c_drive/`, alongside `cmos.bin` |

`cmos.bin` is created fresh with defaults if it doesn't exist, and repaired
automatically if its checksum doesn't match (the same checksum the
[setup panel](../izbios/configuration-panel.md#what-persists-across-reboots)
writes). The real-time clock inside it is seeded from your host clock every
launch.

The C: drive path itself is set at startup, not from inside the GUI: via
`--c-drive`, `--dosroot`, or the `dos.c_drive` key in a `--config` TOML file
such as `examples/machine.toml`. The GUI's "Open C: folder" control opens
the host file manager on the path already configured. It is intended for
copying files onto the guest's hard disk, and does not change which folder C:
uses.

## The two config files

There are two, and they are not interchangeable:

| | `izarravm.conf` | The machine config |
| --- | --- | --- |
| What it holds | Host-side GUI preferences | The machine's hardware |
| Who writes it | The GUI, automatically | You |
| Where it lives | Next to `c_drive/`, alongside `cmos.bin` | Anywhere; you name the path |
| How it is read | Always, on startup | Only when you pass `--config <path>` |
| Example | — | `examples/machine.toml` |

Passing the GUI's own `izarravm.conf` to `--config` is refused with a message
saying so, rather than a parse error about a key you did not know was
significant.

A third location also holds machine settings, and it takes precedence over both
files: `cmos.bin`, the machine's NVRAM, which is what the machine boots from.
The CPU speed and the sound card's resources are stored there and are set from
inside the guest; see [GSWMODE](../toka-dos/commands.md#gswmode) and
[SNDCTRL](../toka-dos/commands.md#sndctrl). A `--cpu` or `--sb-irq` flag sets
the power-on value for a machine that has never been configured. After that the
saved value takes precedence, and the emulator logs a warning naming the flags
it ignored.

## izarravm.conf

A TOML file holding GUI preferences that are meant to survive between runs:

- Master volume
- The CRT emulation style (below)
- Your rebound hotkeys for input release and full screen
- The optional host-controller UUID, two axis controls and polarity, and two buttons
- The last floppy image, last CD image, and last CD folder you mounted
- Whether the control panel is expanded or collapsed
- The P330 receiver, exact host destination, P300 SoundFont, and MT-32 ROM paths

Every field has a default, so an old or partial `izarravm.conf` still loads
cleanly after an upgrade. Master volume is the only audio level kept here: it is
the host's playback level, 0.0 to 5.0 with 1.0 as unity (see "The volume knob"
below). `amp_gain`, `output_gain` and `pc_speaker_volume`
named levels inside the machine's own mixer and are retired -- a file that still
carries them loads, logs one line naming them, and drops them on the next save.
Those levels are set from DOS with `SNDMIXER`, on the card's own registers,
where the guest can read them back.

## The config modal

Opened from the control panel, the config modal has three sections:

**Input**: rebind the "Input release" hotkey (the key combination that
gives keyboard and mouse focus back to the host) and the "Full screen"
toggle hotkey. **Set joystick buttons** opens a foreground setup window and
temporarily disables the parent modal. Follow its prompts to center the stick,
move X right, recenter, move Y down, then press Button 1 and Button 2. Cancel
discards a partial capture. Completion changes only the staged modal settings;
the parent Accept button persists and activates the binding without resetting
the VM.

The first accepted controller fixes the binding to its UUID. The wizard rejects
duplicate axes or buttons and records axis polarity. Runtime input applies a
rescaled 0.15 deadzone and sends only changed 8-bit samples. If that UUID is not
connected, the gameport is detached; for identical controllers with one UUID,
the first connected match is used. Setting `[input].joystick = false` in the
machine config disables injection regardless of a saved GUI binding.

**Display**: **CRT emulation**, a three-way choice:

| On-screen label | What it does |
| --- | --- |
| **No** | No CRT effect; a plain scaled image. |
| **Subtle** | A light shadow-mask CRT effect. This is the default. |
| **Ye Olde Screene** | A heavier CRT effect for the full period look. |

**Audio**: choose which synthesiser answers each MIDI port. Levels are not set
here. The card's output stage, the PC speaker's level and the balance between
sources are ReSonique 2 mixer registers, and `SNDMIXER` sets them from DOS; the
volume knob on the machine panel is the host's playback level, the powered
speakers the machine's line-out feeds.

P300 represents a wavetable daughterboard fitted to the ReSonique 2's internal
pin headers. IzarraVM emulates that board through FluidSynth with the embedded
FluidR3Mono bank. You can select a custom SF2 or SF3 bank without changing the
P330 MIDI route.

P330 represents the Izarra 3000's rear MPU-401/gameport. Its receiver selector
contains these choices:

| Receiver | What it uses |
| --- | --- |
| **Off** | Keeps the P330 MPU active without sending its messages anywhere. |
| **Munt (MT-32)** | An emulator convenience using user-selected MT-32 control and PCM ROMs. IzarraVM does not include Roland ROMs. |
| **Host device name and ordinal** | The exact operating-system MIDI destination. These entries represent the MIDI IN side of an external receiver. If the destination disappears, IzarraVM does not choose another one. |

The host sound device is followed rather than latched. If the default output
device changes, disappears, or was not there when IzarraVM started, the machine
keeps playing into its own output queue and the stream is reopened on the
current default device as soon as one is available, without interrupting the
guest. This is unlike the P330 external destination above, which is a named
choice and is not silently replaced.

The two MT-32 ROM boxes each accept either a ROM file or the folder the ROM set
lives in, and the images are identified by content rather than by filename: a
set named `MT32_CONTROL.ROM` / `MT32_PCM.ROM`, one named for its version, and
one split into half-images all load. Which box you put which file in does not
matter. When a set cannot be loaded the status line says what was missing -- a
control image, a PCM image, or a control and PCM pair from different machines --
and the log names every file that was tried.

P300 and P330 are independent. A P330 receiver change sends all-notes-off to
the old receiver without interrupting FluidSynth. Each section has its own
status line, so a missing host destination or missing ROMs cannot hide a failed
custom SoundFont. Neither failure hides the corresponding guest MPU. Accept
retries a failed synthesiser even when nothing was changed, so a problem fixed
outside the emulator can be picked up without restarting the machine.

Startup settings are resolved one field at a time. An explicit command-line
option takes precedence, followed by an explicitly present `--config` TOML key,
the saved GUI preference, and finally the built-in default.

Accept applies your changes and closes the modal; Cancel discards them.

For step-by-step procedures built on these settings -- routing P330 to a player
on the host, or loading an MT-32 ROM set -- see the
[recipes](../recipes/index.md).

## Mounting removable media

**Floppy (A:)**: accepts `.img`, `.ima`, and `.flp` disk images through a
file picker.

**CD-ROM (D:)** accepts three sources:

- An **ISO** image, mounted as a single data track.
- A **CUE/BIN** pair, with the `.cue` parsed against its matching `.bin`.
- A **host folder**, built into an ISO9660 image on the fly. Files are read
  lazily from the folder as the guest requests sectors, rather than copied
  up front, up to about 650 MB.

A CD image and a CD folder are mutually exclusive. Mounting one clears the
other. There is no equivalent host-folder option for the floppy drive; A:
only takes image files.

## The volume knob

The slider on the control panel is the host's playback level: the powered
speakers the machine's line-out feeds. It is applied to the finished mix on its
way to the sound device, it covers the machine's own output and both MIDI
synthesisers together, and it is the only audio level the emulator itself owns.
Every level inside the machine -- the card's output stage, the PC speaker's leg,
the balance between sources -- is a ReSonique 2 mixer register that `SNDMIXER`
sets from DOS. The headless `IZARRAVM_AUDIO_WAV` capture records the machine's
output and is deliberately taken ahead of this knob, so a recording does not
change when you move the slider.

The travel runs from 0% to 500%, and the value box reads in percent. 100% is
unity: the mix reaches the sound device exactly as the machine produced it, and
the slider steps in whole percent so 100% can be set exactly. Below 100% the
knob follows a perceptual taper. At and above 100% the reading is the
multiplication factor -- 200% is twice line level, 500% is five times, +14 dB.

The ceiling is chosen from the quietest well-behaved case. A title whose own
setup program is at maximum still writes an output level of 27 to the card's
CT1745 mixer, which is 8 dB of attenuation, and the mix reserves a further 6 dB
of headroom below full scale. Its peaks therefore arrive 14 dB down, and 500%
puts them back at the rail.

Gain above 100% can drive samples past what the sound device can carry. Those
samples are held at full scale rather than allowed to wrap, so an overdriven
passage clips the way a driven amplifier clips. The knob is stored in
`izarravm.conf` as `master_volume`, where 1.0 is unity; a file written by an
earlier version, which could only save 0.0 to 1.0, loads to the level it always
meant.

## Other GUI features

- A collapsible beige control panel below the display, with activity LEDs
  for the floppy and the C: drive.
- A master volume slider (above).
- A COM1 serial log window: a floating, resizable panel showing what the
  guest has written to the emulated serial port, useful for anything that
  logs there instead of the screen.
- An About window with license information.

There is no drag-and-drop support for mounting media, and no built-in
screenshot capture. Use your host OS's own screenshot tool against the
IzarraVM window.

## Next

- [Izarra 3000 user manual](../izarra-3000/user-manual.md): the emulated
  machine this application drives.
- [Using Toka-DOS](../toka-dos/using-toka-dos.md): what to expect once the
  guest boots.
