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
- **MIDI / General MIDI / wavetable**: not emulated as a port-level device
  today. A game's MPU-401 or wavetable option will not find hardware to
  talk to. Pick FM/AdLib music instead, or route MIDI through the host as
  described in the [ReSonique 2 manual](resonique2/manual.md#what-resonique-2-does-not-have-yet).

## A 3D-accelerated game doesn't detect Distira

This is expected for now. The Distira 3D chip itself is emulated to a real
Voodoo Graphics-class register and memory contract (see [VEGA technical
reference, section 10](vega/vega-technical-reference.md#10-distira-3d)), but
no guest-side Glide driver exists yet for a game to load. Fall back to
software rendering or a VESA/VGA mode if the game offers one.

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
