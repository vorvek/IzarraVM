<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Command-line reference

This page lists the command-line switches of the `izarravm` program. Run
`izarravm --help` to see the list in your own build, and `izarravm --version`
to see the version. For environment variables, see the
[environment variable reference](environment-variables.md).

With no switch, `izarravm` opens the GUI. See the
[IzarraVM GUI guide](../izarravm-gui/guide.md). A switch overrides the same
setting in the config file.

Each switch has the form `--name value`. A switch with no value in the tables
is a flag. You set it by writing its name.

## Machine and media

| Switch | Value | Description |
| --- | --- | --- |
| `--config` | A file path | Reads the machine description from this file. See `examples/machine.toml`. This file is not the preferences file that the GUI writes. |
| `--cpu` | `386slow`, `386`, `486`, or `586` | Sets the CPU mode. The `gsw` forms of the names, such as `gsw586`, also work. |
| `--interpreter` | Flag | Runs the portable CPU interpreter and turns off native block admission. |
| `--memory-mib` | A number | Sets the memory size in MiB. The allowed range is 2 to 64. |
| `--video` | `vega` | Selects the video card. `vega` is the only card. |
| `--c-drive` | A directory path | Uses this host folder as the C: drive. |
| `--dosroot` | A directory path | Gives the fallback folder for the C: drive. `--c-drive` has priority. You can also set it with `IZARRAVM_DOSROOT`. An empty value counts as not set. |
| `--portable` | Flag | Keeps the C: drive, `cmos.bin`, and `izarravm.conf` beside the executable, and not in the per-user `.izarravm` folder. |
| `--bios` | A file path | Loads this BIOS ROM in place of the built-in Izarra BIOS. With `--headless-test-rom`, it also gives the ROM to run. |
| `--margo-test-pattern` | Flag | Draws the Margo test pattern when the GUI session starts. |

## Sound

| Switch | Value | Description |
| --- | --- | --- |
| `--soundfont` | A file path | Gives a custom SF2 or SF3 bank for the FluidSynth wavetable at port 300. |
| `--midi-backend` | `off`, `external`, or `munt` | Sets the receiver at port 330. |
| `--midi-port` | A port name | Gives the host MIDI destination for the receiver at port 330. |
| `--midi-port-ordinal` | A number | Selects one of several host ports that have the same name. The first port is `0`. It needs `--midi-port`. |
| `--mt32-control-rom` | A file path | Gives the MT-32 control ROM for Munt. |
| `--mt32-pcm-rom` | A file path | Gives the MT-32 PCM ROM for Munt. |
| `--sb-irq` | `2`, `5`, `7`, or `10` | Sets the interrupt line of the Sound Blaster. |
| `--sb-dma` | `0`, `1`, or `3` | Sets the 8-bit DMA channel of the Sound Blaster. |
| `--sb-high-dma` | `5`, `6`, or `7` | Sets the 16-bit DMA channel of the Sound Blaster. |

## Headless checks

A headless run opens no window. Use only one run-mode switch at a time. The
run-mode switches are the ones that start a headless run. They are in this
group, in the next group, and in the profiling group.

| Switch | Value | Description |
| --- | --- | --- |
| `--headless-config-check` | Flag | Reads and checks the config and the switches, then exits. |
| `--headless-test-rom` | Flag | Runs the test ROM, or the ROM that `--bios` names, and prints the screen, the POST code, and the stop reason. |
| `--headless-boot-suite` | Flag | Runs the boot suite and prints its result block. |
| `--headless-izarra-bios` | Flag | Boots the Izarra3000 BIOS, runs POST until it halts, and prints the VDTS records. |
| `--headless-keyboard` | Flag | Boots the keyboard test ROM, types the `--stdin-text` text into it, and prints the screen. |
| `--stdin-text` | Text | Gives the text for `--headless-keyboard`. |
| `--headless-bandwidth` | Flag | Measures memory bandwidth in each CPU mode and prints a table. |
| `--cycles` | A number | Sets the cycle budget of a headless run. The defaults are 200000000 for `--headless-test-rom`, 50000000 for `--headless-boot-floppy`, and 500000000 for `--headless-boot-hdd`. With `--headless-test-rom` and no `--bios`, a budget that is too small for the ROM to halt fails the run. |

## Headless boot and capture

| Switch | Value | Description |
| --- | --- | --- |
| `--headless-boot-floppy` | A file path | Mounts this floppy image and boots from it with the Izarra BIOS. |
| `--headless-boot-hdd` | A file path | Mounts this hard disk image and boots from it with the Izarra BIOS. |
| `--hdd-folder` | A directory path | Boots the Katea host-folder disk: mounts the folder as C: through the FreeDOS system files, runs the BIOS, and prints the boot diagnostics. The top-level files of the folder appear read-only beside the system. |
| `--katea-run` | A file path | Boots FreeDOS from a temporary Katea disk, runs this one DOS program, and exits with its DOS exit code. |
| `--cd-image` | A file path | With `--hdd-folder`, mounts a CD image (ISO or CUE/BIN) before the boot. |
| `--inject-keys` | `cycles:text` steps, separated by `;` | With `--hdd-folder`, types keys at fixed guest-cycle offsets. The offsets must increase. Write `\r` for Enter. |
| `--inject-mouse` | `cycles:action` steps, separated by `;` | With `--hdd-folder`, moves the mouse at fixed guest-cycle offsets. The actions are `home`, `move:<dx>,<dy>`, `down`, `up`, and `click`. The deltas are mickeys. You can use it with `--inject-keys`. |
| `--dump-result` | Flag | With `--hdd-folder`, prints a result block after the stop: the stop reason, CS:IP, the registers, and the 80x25 text page. |
| `--result-ppm` | A file path | With `--hdd-folder`, writes the final framebuffer to this PPM (P6) file. The frame is drawn again at the register state of the stop. |
| `--presented-ppm` | A file path | With `--hdd-folder`, writes the last completed frame to this PPM (P6) file. It is the frame that a user sees. It writes nothing if no frame is complete. |
| `--mode-census` | A file path | With `--hdd-folder`, writes the video modes that the guest programmed, with their counts, to this JSON file. |
| `--expect-test-exit` | Flag | With `--hdd-folder`, fails the run unless the guest reaches test exit code 0. Without it, the process exits 0 for any guest exit code. |
| `--screen-dump-dir` | A directory path | With `--hdd-folder`, saves the screen at intervals into this directory, with an index file named `screens.jsonl`. This slices the run, so do not use it for a benchmark. |
| `--screen-dump-interval-ms` | A number | Sets the guest milliseconds between screen samples. The default is `5000`. It needs `--screen-dump-dir`. |

## Profiling

| Switch | Value | Description |
| --- | --- | --- |
| `--headless-bench` | Flag | Runs the built-in calibration probes and the Dhrystone and Whetstone programs at `.bench/dhrystone.exe` and `.bench/whetstone.exe`. |
| `--headless-bench-exe` | A file path | Runs one DOS EXE through the raw-program bench harness in the 586 mode. |
| `--headless-profile-exe` | A file path | Runs one DOS EXE twice in the 586 mode: first the baseline, then with the profiling buckets. |
| `--profile-json` | A file path | Writes the profile output as JSON. It works with `--headless-profile-exe`, `--hdd-folder`, and `--headless-boot-profile`. The parent directory must exist. |
| `--profile-sample-stride` | A number | Samples every Nth instruction for `--headless-profile-exe`. The default is `1024`. |
| `--headless-boot-profile` | Flag | Boots the C: drive as the GUI does, and shows the wall time of each boot phase: POST, boot, prompt idle, command execution, and disk load. |
| `--load-file` | A guest path | With `--headless-boot-profile`, sets the file that the disk-load phase reads. The default is the largest root-level file. |
| `--idle-seconds` | A number | With `--headless-boot-profile`, sets the guest seconds to wait at the DOS prompt. The default is `10`. |
