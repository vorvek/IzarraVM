# IzarraVM GUI Guide

IzarraVM's desktop application wraps the emulated Izarra 3000 in a control
panel: a display, a beige panel of controls below it, and a config modal for
the things you don't need to reach every session. This page covers the host
application, not the emulated machine — see the [Izarra 3000 user
manual](../izarra-3000/user-manual.md) for what happens inside the guest.

## Starting it

```powershell
cargo run -p izarravm -- --config examples/izarravm.toml
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
such as `examples/izarravm.toml`. The GUI's "Open C: folder" control just
opens your host file manager on whatever path is already configured — handy
for dropping files onto the guest's hard disk, not a way to switch drives.

## izarravm.conf

A TOML file holding GUI preferences that are meant to survive between runs,
separate from the machine-hardware config you pass with `--config`:

- Master volume
- The number of Distira/Glide render threads (1, 2, or 4)
- The CRT emulation style (below)
- Your rebound hotkeys for input release and full screen
- The last floppy image, last CD image, and last CD folder you mounted
- Whether the control panel is expanded or collapsed

Every field has a default, so an old or partial `izarravm.conf` still loads
cleanly after an upgrade.

## The config modal

Opened from the control panel, the config modal has two sections:

**Input** — rebind the "Input release" hotkey (the key combination that
gives keyboard and mouse focus back to the host) and the "Full screen"
toggle hotkey.

**Display** — the number of Voodoo/Distira render threads, and **CRT
emulation**, a three-way choice:

| On-screen label | What it does |
| --- | --- |
| **No** | No CRT effect; a plain scaled image. |
| **Subtle** | A light shadow-mask CRT effect. This is the default. |
| **Ye Olde Screene** | A heavier CRT effect for the full period look. |

Accept applies your changes and closes the modal; Cancel discards them.

## Mounting removable media

**Floppy (A:)** — accepts `.img`, `.ima`, and `.flp` disk images through a
file picker.

**CD-ROM (D:)** — accepts three sources:

- An **ISO** image, mounted as a single data track.
- A **CUE/BIN** pair, with the `.cue` parsed against its matching `.bin`.
- A **host folder**, built into an ISO9660 image on the fly. Files are read
  lazily from the folder as the guest requests sectors, rather than copied
  up front, up to about 650 MB.

A CD image and a CD folder are mutually exclusive — mounting one clears the
other. There is no equivalent host-folder option for the floppy drive; A:
only takes image files.

## Other GUI features

- A collapsible beige control panel below the display, with activity LEDs
  for the floppy and the C: drive.
- A master volume slider.
- A COM1 serial log window — a floating, resizable panel showing what the
  guest has written to the emulated serial port, useful for anything that
  logs there instead of the screen.
- An About window with license information.

There is no drag-and-drop support for mounting media, and no built-in
screenshot capture — use your host OS's own screenshot tool against the
IzarraVM window.

## Next

- [Izarra 3000 user manual](../izarra-3000/user-manual.md) — the emulated
  machine this application drives.
- [Using Toka-DOS](../toka-dos/using-toka-dos.md) — what to expect once the
  guest boots.
