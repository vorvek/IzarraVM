<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# IZBIOS Configuration Panel Guide

This page describes each screen of the Del setup panel, the graphical
configuration tool of the Izarra-BIOS. The
[user manual](../izarra-3000/user-manual.md#the-del-setup-panel) gives a short
description of the panel. This page gives more detail.

Press **Del** during POST to open the panel. The panel uses the same Margo
linear frame buffer mode as the POST screen, and thus the same colors: a cream
field, near-black text, and red titles. Each change that you make goes into a
working copy. Nothing goes to CMOS until you select **Save and Exit**.

## Main menu

The main menu has eight rows. Up and Down move between them. A two-line help
string below the box describes the selected row.

| Row | Key(s) | Help text shown |
| --- | --- | --- |
| TIME | Enter | `Enter: set clock` / `Up/Dn move  Esc back` |
| KEYBOARD | Left/Right/Enter | `Left/Right: change keyboard` / `Enter: open` |
| CPU MODE | Left/Right/Enter | `Left/Right: change CPU speed` / `Enter: open` |
| DEBUG ON COM1 | Left/Right/Enter | `Left/Right: toggle debug output` / `Enter: toggle` |
| PERIPHERALS | Enter | `Enter: check devices` / `Up/Dn move  Esc back` |
| REPAIR TOKA-DOS | Enter | `Enter: reinstall Toka-DOS` / `Up/Dn move` |
| SAVE AND EXIT | Enter or F10 | `Enter/F10: save and reboot` / `Up/Dn move` |
| DISCARD AND EXIT | Enter or Esc | `Enter/Esc: discard and reboot` / `Up/Dn move` |

KEYBOARD, CPU MODE, and DEBUG ON COM1 are inline rows. The panel draws the
current value to the right of the label. Left and Right change the value in
position. Enter has the same effect as Right. These three rows do not open a
sub-page. The other rows with an Enter action open a full sub-screen.

## TIME

Title: **SET TIME AND DATE**. Hint: `L/R field  Up/Dn change  Esc back`.

The screen has six fields, in this order: Hour, Minute, Second, Day, Month,
Year. Left and Right move between the fields. Up and Down (or Enter) increase
or decrease the selected field by one. Each field returns to its first value
after its last value. The panel edits the clock as a binary 24-hour value,
which is the format of the machine real-time clock. A change to any field
marks the time as changed, and Save and Exit then writes it to the RTC. If you
do not use this screen, the clock does not change.

## KEYBOARD

This row selects one of the 17 keyboard layouts of the machine (US, UK,
Spanish, French, German, Italian, and others). It does this on the main menu,
without a sub-page. The selected layout becomes active immediately, so that
you can test it. Save writes it to CMOS, together with the boot code page for
the characters of that layout.

## CPU MODE

This row selects the boot-time CPU speed on the main menu, in the order `386`,
`486`, `586`, `386-slow`, and then `386` again. The value sets the speed class
for the next boot, and Save writes it to CMOS. The classes and the stored
value are the same as those of the
[Tab boot menu](../izarra-3000/user-manual.md#the-tab-boot-menu). This row is
the same control inside the setup panel.

This row does not change the speed of the running machine. To change the speed
now, use the Tab boot menu at the next boot, or run `GSWMODE` inside Toka-DOS.
See the [command reference](../toka-dos/commands.md#gswmode).

## DEBUG ON COM1

This row sets the BIOS debug output to Enabled or Disabled, on the main menu.
Enabled sends the POST messages to the COM1 serial port as text. Disabled
sends no POST messages there. The UART itself stays available to software in
both states. Save writes the value to CMOS.

The IzarraVM GUI has a COM1 log window. That window shows this output. See the
[GUI guide](../izarravm-gui/guide.md#other-gui-features).

## PERIPHERALS

Title: **PERIPHERAL CHECK**. Hint: `Esc: back to menu`.

This screen does seven of the POST hardware checks again. The checks do not
change the hardware. The screen shows PASS or FAIL for each check, in a boxed
table:

```
LOTURA      PIT TIMER    SB DSP     MARGO VGA
8042 KBD    COM1 UART    OPL FM
```

PASS is sage green and FAIL is red. This screen is a diagnostic screen only.
It has nothing to configure. It does the checks again without a full restart.

## REPAIR TOKA-DOS

This row installs the Toka-DOS system files again, from the copy in the ROM.
When you select it, the BIOS sends a request to the service port of the
emulated Lotura chipset. It then waits for a status. After a successful
repair, it shows:

```
Toka-DOS repaired
```

See [Repair Toka-DOS](../toka-dos/using-toka-dos.md#repair-toka-dos) for the
files that the repair replaces and the files that it keeps.

## SAVE AND EXIT / DISCARD AND EXIT

**Save and Exit** writes the working copy. The keyboard layout, the CPU mode,
and the debug setting go to CMOS. The clock goes to the real-time clock if you
used the Time screen. The BIOS then calculates a checksum over the saved
settings and restarts the machine.

**Discard and Exit** discards all changes and restarts the machine with the
saved settings. Esc on the main menu has the same effect.

## The settings that CMOS keeps

This panel writes three settings: the **keyboard layout** (CMOS offset
`0x10`), the **CPU mode** (CMOS offset `0x12`), and the **debug on COM1**
setting (CMOS offset `0x14`). The clock goes directly to the real-time clock
hardware, and not to CMOS, when you use the Time screen. The Tab boot menu
writes the CPU mode and the primary boot device (CMOS offset `0x11`), which is
independent of this panel. The two entry points use the same CMOS fields where
they overlap.

A checksum protects the saved block. If the checksum does not agree at boot,
the BIOS applies no setting from that image. IzarraVM keeps the default
settings, records the CMOS diagnostic indication, calculates a new checksum,
and saves the repaired image.

## Next

- [Izarra3000 user manual](../izarra-3000/user-manual.md): the machine, the
  POST screen, and the boot menu.
- [How to use Toka-DOS](../toka-dos/using-toka-dos.md): the system that boots
  after setup.
