<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Izarra3000 User Manual

The Izarra3000 is a fixed machine. This manual describes the machine as
built. It tells you what you see when you start it. It tells you what the boot
menu and the setup panel contain, and what each control does. You cannot
select different hardware, as you can on a real PC with a BIOS setup program.
IzarraVM emulates one machine, and this manual describes that machine.

## Tech specs

| Area | Izarra3000 hardware |
| --- | --- |
| CPU | GSW-586 at 166 MHz on a 66 MHz bus. This is a Pentium-class part with an x87 unit and no SIMD extension. Toka-DOS can set it to a 486DX2 at 66 MHz, a 386DX at 22 MHz, or the same 386 instruction set at 7.33 MHz. A restart is not necessary. |
| Memory | 64 MB PC100 SDRAM. Toka-DOS moves itself out of conventional memory when a DOS game needs the first 640 KB. |
| Graphics | VEGA chipset: Margo 2D with a 4 MB frame store; Distira 3D with a 2 MB frame buffer and 2 MB for each TMU; VESA VBE 2.0, VGA mode 13h, and a maximum of 1024x768 at 32-bit color. |
| Sound | ReSonique II: Sound Blaster 16 compatible digital audio (PCM and Creative ADPCM), OPL3 FM synthesis, pin headers for a wavetable daughterboard, and a rear MPU-401/gameport. |
| Storage | UDMA2 IDE hard disk on a PIIX4-compatible controller, PIO ATAPI CD-ROM with CD audio, and a 1.44 MB floppy drive. |
| Display | 15-inch CRT. The maximum mode is 1024x768 at 75 Hz. |
| Firmware | 2 MB ROM. It holds the Izarra BIOS, Toka-DOS (based on FreeDOS), and the supplied tools. |
| I/O | PS/2 keyboard and mouse, serial, parallel, VGA, rear MPU-401/gameport, line out, line in. |

The [DOS command reference](../toka-dos/commands.md) describes the software.

## The POST screen

When you start the machine, the Izarra-BIOS does its power-on self test
(POST). Most PC BIOS programs print text that scrolls up the screen. The
Izarra-BIOS draws a graphical screen instead, on the linear frame buffer of
the Margo chip. The screen shows a cream field, a red "Izarra3000" wordmark,
and a row of component icons. Each icon changes from grey to color when its
part passes the check. The header shows:

```
Izarra-BIOS v3.01 - 1997
Running diagnostics...
```

The diagnostics run in a fixed order. Each check stops for a short time, so
that you can see it. The order is: CPU identity, Margo video, the RAM count,
the Lotura chipset, the 8042 keyboard controller, the PIT timer, the COM1
UART, the Sound Blaster DSP, the OPL FM synthesizer, the floppy controller,
the hard disk, and the ATAPI optical drive. If a part fails, its icon stays
grey. The machine then does the next check. It does not stop.

Two hints stay below the header for the full POST sequence:

```
DEL ► Configuration menu
TAB ► Select boot device
```

You can press either key at any time during POST. You do not have to wait for
the end of the sequence. The first key that you press has the effect. If you
press no key, the machine boots when the diagnostics are complete.

## The Tab boot menu

Press **Tab** during POST to open the boot and speed menu. The BIOS draws one
boxed panel above the POST screen, with the title "Boot & Speed". The panel
has two sections and an Accept row.

- Up and Down move between the rows.
- Enter marks a row, or operates the Accept row.
- F10 also accepts.
- Esc cancels. The machine keeps the boot device that CMOS holds.

**Boot device** has three rows: Hard Disk, Floppy A:, and CD-ROM. The saved
value is one primary device. It is not a list of three devices that you can
put in order. If the primary device cannot boot, the BIOS tries the other
devices in the fixed order below. If all three fail, the BIOS calls INT 18h.

| Primary | Attempt sequence |
| --- | --- |
| Floppy A: | Floppy A:, Hard Disk, CD-ROM |
| Hard Disk | Hard Disk, Floppy A:, CD-ROM |
| CD-ROM | CD-ROM, Hard Disk, Floppy A: |

The floppy controller is always available. You can select the Hard Disk row
and the CD-ROM row only when the menu finds bootable media. A row that you
cannot select is grey. Enter on a device row boots that device one time, and
does not change CMOS. F10, or Enter on the Accept row, saves the marked
primary device and the CPU speed. Esc discards the menu and keeps the primary
device that CMOS holds.

**CPU speed** has four rows, from the fastest to the slowest:

| Row | CPU class | Port/CMOS code |
| --- | --- | --- |
| 586 | GSW-586 at 166 MHz | 2 |
| 486 | 486DX2 at 66 MHz | 1 |
| 386 | 386DX at 22 MHz | 0 |
| 386-slow | The same 386DX instruction set at 7.33 MHz | 3 |

When you accept the menu, the BIOS writes the selected speed to the mode port
of the Lotura chipset. The CPU changes its speed class at that moment. The
BIOS also writes the primary boot device and the CPU speed to CMOS, as the
defaults for subsequent cold boots. A second restart is not necessary. The
machine boots the selected device at the selected speed immediately.

The `GSWMODE` command selects the same four speed classes from inside
Toka-DOS, at any time. See the
[DOS command reference](../toka-dos/commands.md#gswmode).

## The Del setup panel

Press **Del** during POST to open the configuration panel. The BIOS draws it
on the Margo linear frame buffer, with the title "IZARRA3000 SETUP". The
panel edits a working copy of the settings. It writes nothing to CMOS until
you save. The panel has eight rows. Up and Down move between them. A one-line
help string below the box describes the selected row.

| Row | What it does |
| --- | --- |
| **Time** | Opens a sub-page for the clock: hour, minute, second, day, month, year. Left and Right select the field. Up and Down (or Enter) change it. Esc returns to the main menu. |
| **Keyboard** | Left and Right (or Enter) select the keyboard layout. The machine has 17 layouts (US, UK, Spanish, French, German, Italian, and others). |
| **CPU Mode** | Left and Right (or Enter) select the boot-time CPU speed. The order is 386, 486, 586, 386-slow, and then 386 again. |
| **Debug on COM1** | Left and Right (or Enter) set the row to Enabled or Disabled. Enabled sends the POST messages to the COM1 serial port as text. The UART stays available to software in both states. |
| **Peripherals** | Opens a sub-page. The sub-page does the POST hardware checks again and shows PASS or FAIL for each one. The checks do not change the hardware. The list is Lotura, 8042 KBD, PIT Timer, COM1 UART, SB DSP, OPL FM, and Margo VGA. |
| **Repair Toka-DOS** | Installs Toka-DOS on the hard disk again, from the image in the ROM. Use it when the installed copy is damaged or absent. It shows "Toka-DOS repaired" after a successful repair. See [Repair Toka-DOS](../toka-dos/using-toka-dos.md#repair-toka-dos) for the files that it changes. |
| **Save and Exit** | Saves the working copy and restarts the machine. It writes the keyboard layout, the CPU mode, and the debug setting to CMOS. It writes the clock to the real-time clock if you changed the clock. |
| **Discard and Exit** | Discards all changes and restarts the machine. Esc on the main menu has the same effect. |

### The settings that CMOS keeps

The setup panel saves the **keyboard layout**, the boot-time **CPU mode**, and
the **debug on COM1** setting. The Tab boot menu saves the **primary boot
device** and the CPU mode. A checksum protects these settings. If the saved
image is corrupt, IzarraVM applies no setting byte from it, although some
bytes can look correct. IzarraVM keeps the default settings of the machine,
sets the CMOS diagnostic indication, calculates a new checksum, and writes the
repaired image to disk.

## BIOS compatibility services

The Izarra-BIOS reports 639 KiB of conventional memory through INT 12h. The
1 KiB EBDA stays at the physical address `0x9FC00`. The E820 map reports the
same boundary.

The real-time clock (RTC) services include the asynchronous INT 15h AH=83h
wait, the synchronous AH=86h wait, and the INT 1Ah AH=06h/07h alarm
functions. The IRQ8 handler reads RTC registers B and C. It obeys a periodic
cause and an alarm cause together. It calls INT 4Ah for an alarm, and it
acknowledges both PICs.

The rear gameport decodes each port from `0x200` to `0x207` as an alias.
The connector has four 100 kOhm RC axis lines and four active-low button lines.
An OUT instruction charges each connected axis timer, and a read shows the
four timer states and four button states. A standard joystick or 4 button gamepad
connects joystick A X and Y and leaves joystick B X and Y open. A wheel and
pedals profile can connect all four axes. INT 15h AH=84h reports the four
switches and returns A X, A Y, B X, and B Y in AX, BX, CX, and DX. An absent B
axis keeps the BIOS compatibility value of zero.

The Keyboard only target leaves the gameport disconnected. Controller
directions, buttons, and triggers send AT keyboard keys or modifier combinations
instead. Trigger rows accept either analog-axis or digital-button reports from
the host input backend.

## Next

- [How to use Toka-DOS](../toka-dos/using-toka-dos.md): what occurs after POST
  gives control to the disk.
- [IZBIOS Configuration Panel guide](../izbios/configuration-panel.md): each
  setup screen in more detail.
- [The IzarraVM GUI guide](../izarravm-gui/guide.md): the host application
  around the emulated machine.
