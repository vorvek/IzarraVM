<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Izarra 3000 User Manual

The Izarra 3000 is a fixed machine. This manual describes it as built: what
you see when you power it on, what the boot menu and setup panel offer, and
what each control does. None of the hardware here is user-selectable in the
way a real PC's BIOS setup lets you swap parts. IzarraVM reproduces one
machine, and this manual reproduces its manual.

## Tech specs

| Area | Izarra 3000 hardware |
| --- | --- |
| CPU | GSW-586 at 166 MHz on a 66 MHz bus, a Pentium-class part with an x87 unit and no SIMD extension. Toka-DOS can throttle it to a 486DX2 at 66 MHz, a 386DX at 22 MHz, or the same 386 ISA at 7.33 MHz without rebooting. |
| Memory | 64 MB PC100 SDRAM, with Toka-DOS mapping itself out of conventional memory when DOS games need the first 640 KB. |
| Graphics | VEGA chipset: Margo 2D with a 4 MB frame store; Distira 3D with a 2 MB framebuffer and 2 MB per TMU; VESA VBE 2.0, VGA mode 13h, and up to 1024x768 at 32-bit color. |
| Sound | ReSonique 2: Sound Blaster 16 compatible digital audio (PCM and Creative ADPCM), OPL3 FM, pin headers for a wavetable daughterboard, and a rear MPU-401/gameport. |
| Storage | UDMA2 IDE hard disk on a PIIX4-compatible controller, PIO ATAPI CD-ROM with CD audio, and a 1.44 MB floppy drive. |
| Display | 15-inch CRT, up to 1024x768@75Hz. |
| Firmware | 2 MB ROM with the Izarra BIOS, Toka-DOS (FreeDOS-based), and bundled tools. |
| I/O | PS/2 keyboard and mouse, serial, parallel, VGA, rear MPU-401/gameport, line out, line in. |

See the [company story](company-story.md) for where the machine came from, and
the [DOS command reference](../toka-dos/commands.md) for the software side.

## Powering on: the POST screen

Turning the machine on runs the Izarra-BIOS power-on self test (POST). Unlike
the scrolling text most PC BIOSes print, the Izarra-BIOS draws a graphical
screen on the Margo chip's own linear frame buffer: a cream field, a red
"Izarra 3000" wordmark, and a row of component icons that light from grey to
color as each part passes its check. The header reads:

```
Izarra-BIOS v3.01 - 1997
Running diagnostics...
```

Diagnostics run in a fixed order, each pausing briefly so the check is
visible rather than flickering past: CPU identity, Margo video, RAM count-up,
the Lotura chipset, the 8042 keyboard controller, the PIT timer, the COM1
UART, the Sound Blaster DSP, the OPL FM synth, the floppy controller, the
hard disk, and the ATAPI optical drive. A part that fails stays grey (its
icon never lights) and the machine carries on to the next check rather than
stopping cold.

Two hints sit under the header for the whole POST sequence:

```
DEL ► Configuration menu
TAB ► Select boot device
```

Either key can be pressed at any point during POST, not just at the very
end; the first one pressed wins. Press neither, and the machine boots
normally once diagnostics finish.

## The Tab boot menu

Press **Tab** during POST to open the boot and speed menu. It draws as a
single boxed panel over the POST screen, titled "Boot & Speed," with two
stacked sections and an Accept row. Move between rows with Up/Down; Enter
marks a row or fires Accept; F10 also accepts; Esc cancels back to the saved
primary boot device without changing anything.

**Boot device**: three rows are Hard Disk, Floppy A:, and CD-ROM. The saved
choice is a primary device, not an editable three-slot order. When that device
cannot boot, the BIOS tries the fixed fallbacks below before entering INT 18h:

| Primary | Attempt sequence |
| --- | --- |
| Floppy A: | Floppy A:, Hard Disk, CD-ROM |
| Hard Disk | Hard Disk, Floppy A:, CD-ROM |
| CD-ROM | CD-ROM, Hard Disk, Floppy A: |

The floppy controller is always available. Hard disk and CD-ROM rows are
selectable only when the menu finds bootable media; an unavailable row is grey.
Enter on a device row boots it once without changing CMOS. F10, or Enter on the
Accept row, saves the marked primary and CPU speed. Esc discards the menu state
and restores the primary already stored in CMOS.

**CPU speed** has four rows, from fastest to slowest:

| Row | CPU class | Port/CMOS code |
| --- | --- | --- |
| 586 | GSW-586 at 166 MHz | 2 |
| 486 | 486DX2 at 66 MHz | 1 |
| 386 | 386DX at 22 MHz | 0 |
| 386-slow | The same 386DX ISA at 7.33 MHz | 3 |

Accepting the menu writes the chosen speed to the Lotura chipset's mode port
immediately (the CPU actually changes speed class right there) and saves
both the primary boot device and the CPU speed to CMOS as the new defaults for
future cold boots. There is no separate reboot: the machine falls straight
through to booting the chosen device at the chosen speed.

The same four speed classes are reachable from inside Toka-DOS at any time
with the `GSWMODE` command; see the
[DOS command reference](../toka-dos/commands.md#gswmode).

## The Del setup panel

Press **Del** during POST to open the configuration panel, also drawn on the
Margo linear frame buffer, titled "IZARRA 3000 SETUP." It edits a working
copy of your settings; nothing is written to CMOS until you save. The panel
has eight rows, moved between with Up/Down. A one-line help string under the
box changes to describe whatever row is highlighted.

| Row | What it does |
| --- | --- |
| **Time** | Opens a sub-page to set the clock: hour, minute, second, day, month, year. Left/Right picks the field, Up/Down (or Enter) changes it, Esc returns. |
| **Keyboard** | Cycles the active keyboard layout with Left/Right (or Enter). Seventeen layouts are available (US, UK, Spanish, French, German, Italian, and others). |
| **CPU Mode** | Cycles the boot-time CPU speed with Left/Right (or Enter): 386, 486, 586, then 386-slow, wrapping back to 386. |
| **Peripherals** | Opens a sub-page that re-runs the POST hardware checks live (they are non-destructive) and lists each as PASS or FAIL: Lotura, 8042 KBD, PIT Timer, COM1 UART, SB DSP, OPL FM, and Margo VGA. |
| **Repair Toka-DOS** | Reinstalls Toka-DOS onto the hard disk from the ROM's built-in image, for when the installed copy is damaged or missing. Reports "Toka-DOS repaired" on success. See [Repair Toka-DOS](../toka-dos/using-toka-dos.md#repair-toka-dos) for exactly what this touches. |
| **Save and Exit** | Commits the working copy (keyboard layout and CPU mode to CMOS, the clock to the real-time clock if you changed it) and reboots. |
| **Discard and Exit** | Throws away any changes and reboots. Esc on the main menu does the same thing. |

### What CMOS remembers

The setup panel persists the **keyboard layout** and boot-time **CPU mode**.
The Tab boot menu independently persists the **primary boot device** and CPU
mode. A checksum covers these settings. If a saved image is corrupt, none of its
apparently plausible setting bytes are applied: IzarraVM keeps the machine's
seeded defaults, raises the CMOS diagnostic indication, regenerates the checksum,
and writes the repaired image back to disk.

## BIOS compatibility services

The Izarra-BIOS reports 639 KiB of conventional memory through INT 12h, leaving
the 1 KiB EBDA at physical `0x9FC00`; its E820 map reports the same boundary.

RTC services include asynchronous INT 15h AH=83h waits, synchronous AH=86h
waits, and INT 1Ah AH=06h/07h alarm set/cancel. IRQ8 reads RTC Registers B and C,
handles simultaneous periodic and alarm causes, calls INT 4Ah for an alarm, and
acknowledges both PICs.

The rear gameport decodes every port from `0x200` through `0x207` as an alias.
Joystick A supplies two 250 kOhm RC axes and two active-low buttons; joystick B
is unpopulated. An OUT charges the axis timers and reads expose their live state.
INT 15h AH=84h reports the same switches and A-axis positions, with zeroes for
joystick B.

## Next

- [Using Toka-DOS](../toka-dos/using-toka-dos.md): what happens after POST hands off to the disk.
- [IZBIOS Configuration Panel guide](../izbios/configuration-panel.md): a closer look at each setup screen.
- [The IzarraVM GUI guide](../izarravm-gui/guide.md): the host application around the emulated machine.
