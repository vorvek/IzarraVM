# IZBIOS Configuration Panel Guide

A per-screen walkthrough of the Del setup panel, the Izarra-BIOS's
graphical configuration tool. For the quick tour, see the [user
manual](../izarra-3000/user-manual.md#the-del-setup-panel); this page goes
one screen deeper.

Press **Del** during POST to open it. The panel draws on the same Margo
linear frame buffer mode the POST screen uses, so it shares its palette:
cream field, near-black text, red titles. Everything you change here is a
working copy: nothing reaches CMOS until you choose **Save and Exit**.

## Main menu

Seven rows, moved between with Up/Down. A one-line help string under the box
updates to describe whichever row is highlighted; a second, static line
below it reminds you that dead keys emit their base character on this
screen (useful context if your keyboard layout uses them).

| Row | Key(s) | Help text shown |
| --- | --- | --- |
| TIME | Enter | `Enter: set clock  Up/Dn move  Esc back` |
| KEYBOARD | Left/Right/Enter | `Left/Right: change keyboard  Enter: open` |
| CPU MODE | Left/Right/Enter | `Left/Right: change CPU speed  Enter: open` |
| PERIPHERALS | Enter | `Enter: check devices  Up/Dn move  Esc back` |
| REPAIR TOKA-DOS | Enter | `Enter: reinstall Toka-DOS  Up/Dn move` |
| SAVE AND EXIT | Enter or F10 | `Enter/F10: save and reboot  Up/Dn move` |
| DISCARD AND EXIT | Enter or Esc | `Enter/Esc: discard and reboot  Up/Dn move` |

KEYBOARD and CPU MODE are inline rows: their current value is drawn to the
right of the label, and Left/Right (or Enter, which behaves like Right)
cycles it in place without opening a sub-page. The other rows with a Enter
action open a full sub-screen.

## TIME

Title: **SET TIME AND DATE**. Hint: `L/R field  Up/Dn change  Esc back`.

Six fields in order: Hour, Minute, Second, Day, Month, Year. Left/Right
moves between fields; Up/Down (or Enter) bumps the highlighted field up or
down by one, wrapping at each field's limits. The clock is edited as a
binary 24-hour value, matching the machine's real-time clock format.
Editing any field marks the time as changed, so Save and Exit knows to write
it to the RTC; if you never touch this screen, the clock is left alone.

## KEYBOARD

Cycles through the machine's 17 selectable keyboard layouts (US, UK,
Spanish, French, German, Italian, and more) directly on the main menu, with no
sub-page. The chosen layout takes effect immediately for feedback, and is
written to CMOS on Save, together with the matching boot code page for that
layout's characters.

## CPU MODE

Cycles the boot-time CPU speed directly on the main menu, in the order
`386` → `486` → `586` → `286`, wrapping back to `386`. This sets the speed
class the machine will boot at next time, saved to CMOS on Save. It's the
same four classes and the same underlying value as the [Tab boot
menu](../izarra-3000/user-manual.md#the-tab-boot-menu), just reachable from
inside setup instead. It does not change the machine's speed live; for
that, either use the Tab boot menu at the next boot, or run `GSWMODE` from
inside Toka-DOS (see the [command reference](../toka-dos/commands.md#gswmode)).

## PERIPHERALS

Title: **PERIPHERAL CHECK**. Hint: `Esc: back to menu`.

Re-runs seven of the POST hardware probes live and non-destructively,
listing each with a PASS or FAIL status in a boxed table:

```
LOTURA      PIT TIMER    SB DSP     MARGO VGA
8042 KBD    COM1 UART    OPL FM
```

PASS is shown in sage green, FAIL in red. This is a diagnostic screen only.
There is nothing here to configure, just a live re-check without a full
reboot.

## REPAIR TOKA-DOS

Reinstalls Toka-DOS's system files from the copy built into ROM. Selecting
it fires a request to the emulated Lotura chipset's service port and waits
for a status. On success it reports:

```
Toka-DOS repaired
```

See [Repair Toka-DOS](../toka-dos/using-toka-dos.md#repair-toka-dos)
for exactly what this leaves alone versus what it replaces.

## SAVE AND EXIT / DISCARD AND EXIT

**Save and Exit** commits the working copy: keyboard layout and CPU mode go
to CMOS, the clock goes to the real-time clock if the Time screen was
edited, a checksum is recomputed over the saved settings, and the machine
reboots. **Discard and Exit** (or Esc from the main menu, which does the
same thing) throws every change away and reboots with whatever was already
saved.

## What persists across reboots

Only two settings are written by this panel: the **keyboard layout** (CMOS
offset `0x10`) and the **CPU mode** (CMOS offset `0x12`). The clock is
written to the real-time clock hardware directly, not CMOS, when the Time
screen was used. The Tab boot menu writes the CPU mode and the boot device
order (CMOS offset `0x11`) independently of this panel. The two entry
points share the same underlying CMOS fields where they overlap. A stored
checksum protects the saved block; if it doesn't match at boot, the BIOS
falls back to defaults rather than trusting corrupted settings.

## Next

- [Izarra 3000 user manual](../izarra-3000/user-manual.md): the machine
  overview, POST screen, and boot menu.
- [Using Toka-DOS](../toka-dos/using-toka-dos.md): what boots once setup is
  done.
