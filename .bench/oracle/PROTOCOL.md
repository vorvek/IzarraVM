<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->
# DOSBox-X oracle protocol

The usage protocol for `.bench/oracle/Invoke-DosboxOracle.ps1`, the traps it
absorbs, and what has and has not been measured. Carry forward verbatim into
future oracle sessions. Full findings and the raw measurements behind every
TESTED claim below live in `dev_docs/2026-08-31-dosbox-oracle-harness.md`.

## 1. Purpose

DOSBox-X is an ORACLE for IzarraVM campaigns, not a dependency. It answers
"what does another emulator do with this program, at a comparable CPU speed?"
in seconds instead of an afternoon — and, because it cuts different corners
than IzarraVM (port storms it cannot see, wait states it charges on a
different model), it is also useful for spotting where IzarraVM's own
behaviour is the outlier.

**Prior art only.** Nothing DOSBox-X produces is a merge gate, and none of its
code enters IzarraVM. This is the same posture as `dev_docs/reference/` for
hardware documentation: read it, never copy from it into a source file that
ships.

## 2. Quick start

Exact tested invocation:

```powershell
.\.bench\oracle\Invoke-DosboxOracle.ps1 `
    -Dir      D:\ctd\corpus-scratch\tyrian `   # host dir mounted as C:
    -Command  'TYRIAN.EXE' `                   # appended to [autoexec] after C:
    -Persona  486 `                            # 386-slow | 386 | 486 | 586
    -TimeLimit 90 `                            # EMULATED seconds
    -WallLimit 180 `                           # REAL seconds, the watchdog that works
    -LogTypes io,pit,pic `                     # [log] categories raised to debug
    -Set 'dosbox:machine=svga_s3','cpu:core=normal'
```

Persona -> DOSBox-X `cycles` (owner-supplied, from the
[DOSBox-X cycles guide](https://dosbox-x.com/wiki/Guide%3ACPU-settings-in-DOSBox%E2%80%90X#_cycles)):

| Persona | cycles | cputype the harness pairs with |
|---|---|---|
| `386-slow` | `fixed 2000` | 386 |
| `386` | `fixed 4300` | 386 |
| `486` | `fixed 23880` | 486 |
| `586` | `fixed 95000` | pentium |

The cputype column is the harness's own choice, not part of the owner's
mapping; override it with `-Set 'cpu:cputype=...'`.

A run writes a generated conf, a log, and a `run.json` record under
`D:\ctd\oracle-scratch\<name>-<timestamp>\` (override the root with
`IZARRAVM_ORACLE_SCRATCH`). **Nothing is ever written inside the repo.** The
returned PSCustomObject (also `run.json`) carries `RunName / Exe / Persona /
Cycles / Turbo / MountDir / Command / ExitCode / WallSeconds / Completed /
KilledByWallLimit / Sentinel / RunDir / ConfPath / LogPath / LogBytes /
Arguments`.

Cycles are a throttle, not a model: DOSBox-X retires `cycles` instructions per
emulated millisecond regardless of what those instructions cost on real
silicon. Any DOSBox-X-vs-IzarraVM timing comparison is ballpark evidence,
never a verdict.

## 3. Headless and the speed limiter — TESTED

- **`-silent` is headless.** It `putenv`s both `SDL_VIDEODRIVER=dummy` and
  `SDL_AUDIODRIVER=dummy` and implies `-exit -nomenu -fastlaunch`. TESTED: a
  mount-plus-`DIR` run under `-silent -fastlaunch -nogui -nomenu
  -nopromptfolder -defaultmapper -exit -log-con -time-limit 120 -conf <conf>`
  opened no window, grabbed no audio device, and exited on its own in
  0.25-0.5 s wall. `-nogui` alone is NOT headless — it only hides the menu.
- **A `[sdl] videodriver` conf key silently un-headlesses the run.** DOSBox-X
  applies that key by `putenv` *after* `-silent` has already set the dummy
  driver, so it wins. The harness never emits that key itself and refuses to
  let `-Set` do it quietly: passing `-Set 'sdl:videodriver=...'` prints a
  `Write-Warning` naming the trap.
- **`cycles=max` does NOT lift the wall-clock limiter.** DOSBox-X still paces
  the emulated clock against the host clock; only the per-millisecond
  instruction budget is uncapped. TESTED, `-time-limit 20`, idle DOS prompt:
  persona `586` (`fixed 95000`) read 20.79 s wall -> 0.96x realtime, and
  `586` + `cycles=max` read 20.52 s -> 0.97x — the same number, within noise.
- **`[cpu] turbo=true` is the actual lever.** TESTED on the same idle prompt:
  `586` + `-Turbo` read 1.03 s wall -> 19.4x realtime, and under x87 load
  (`whetstone.exe` from `.bench`) 1.31 s -> 15.3x. Adding `cycles=max` on top
  of turbo moved nothing measurable (19.4 vs 19.7x, 15.3 vs 15.4x). The
  ceiling is host throughput — roughly **15-19x realtime on this host**,
  lower under x87 load. Never quote a number measured under `-Turbo` as
  timing evidence: it accelerates the emulated clock the guest times itself
  against.

## 4. Two exit-behaviour traps the harness absorbs

### DOSBox-X always exits 0

`sdlmain.cpp` returns `saved_opt_test && testerr ? 1 : 0` — outside its own
gtest suite the exit code is always 0, whether the run finished, timed out, or
crashed. TESTED: same persona, same directory, one run that completed and one
that did not, both `ExitCode=0`:

```
CALL T2.BAT   wall=0.494s  Completed=True   ExitCode=0
T2.BAT        wall=0.379s  Completed=False  ExitCode=0
```

The harness appends a unique `echo IZARRAVM_ORACLE_DONE_<hex>` sentinel to
`[autoexec]` and derives `Completed` by grepping the captured CON output for
it. **Read `Completed`; never read `ExitCode`.**

### `-time-limit` counts emulated seconds, not host seconds

DOSBox-X checks `-time-limit` against `PIC_FullIndex`, the guest clock, so it
does not bound host time and never fires if the emulated clock stops
advancing. TESTED and not hypothetical: a `:X` / `goto X` loop in `[autoexec]`
runs in DOSBox-X's own native batch interpreter, not the emulated CPU — it
pegs a host core at 100% while the guest clock crawls. With `-time-limit 20`
that run was still alive **five and a half minutes later** (316 s of host
CPU) and had to be killed by hand. The harness's own `-WallLimit` (default
`TimeLimit*3 + 30`, real host seconds) is the watchdog that actually works;
TESTED on that spin loop: `-TimeLimit 10 -WallLimit 25` -> `wall=25.17s
KilledByWallLimit=True`.

The same chaining behaviour bites `[autoexec]` commands directly: a bare
`FOO.BAT` **chains and never returns**, so anything after it — including the
sentinel and `exit` — is dead. Use `CALL FOO.BAT` for anything that must run
afterward.

## 5. Debugger

### SKU inventory — TESTED, by symbol probe of the shipped binaries

All nine installed executables carry `C_DEBUG`. Only the four Visual Studio
SKUs carry `C_HEAVY_DEBUG` as well:

| Binary in `D:\DOSBox-X\` | debugger | heavy |
|---|---|---|
| `dosbox-x.exe` (default; = `x64_SDL1`) | yes | **yes** |
| `dosbox-x_x64_SDL1/SDL2`, `dosbox-x_x86_SDL1/SDL2` | yes | **yes** |
| `dosbox-x_MinGWx64_SDL1/SDL2`, `dosbox-x_MinGWx86_SDL1/SDL2` | yes | no |

There is no separate `-debug.exe`; the debugger lives in the normal
executable, and the default `dosbox-x.exe` is already a full heavy-debug
build. TESTED that it opens: `-break-start` produced a Win32 console window
titled `DOSBox-X Debugger` within 6 s.

Reach it three ways: `-break-start` (before the first instruction), the
`DEBUGBOX` command inside the guest, or the `mapper_debugger` shortcut
(Alt+Pause).

### Commands worth knowing

Breakpoints — `BP seg:off`, `BPINT n [ah [al]]`, `BPM seg:off`, `BPPM
sel:off`, `BPLM linear` (the memory-change breakpoints are heavy-debug only).
Memory — dump and search via `MEMDUMP`/`MEMDUMPBIN`/`MEMFIND`/`MEMS`. Ports —
`INP`/`INW`/`IND` and `OUTP`/`OUTW`/`OUTD` read/write a port by hand.
Execution — `VRT` breaks at the next vertical retrace, `TIMERIRQ` steps the
timer interrupt. Instruction logging (heavy-debug only) —
`LOGC`/`LOGS`/`LOG`/`LOGL n`, escalating detail (CS:IP only -> short disasm
-> add analysis and flags -> add a `PIC_FullIndex()` timestamp and raw bytes),
all writing `LOGCPU.TXT`; `HEAVYLOG` toggles a ring-buffer dump to
`LOGCPU_INT_CD.TXT` at exit.

**The interface is interactive-console only.** There is no GDB stub and no
serial debug protocol anywhere in the tree — nothing the harness, or any
script, can drive. An instruction trace from the debugger means someone types
into that console by hand.

## 6. Port-storm visibility

**The stock build is blind to exactly the storms IzarraVM campaigns care
about.** `IO_ReadDefault`/`IO_WriteDefault` log ONE line for an unhandled port
and then silently overwrite their own handler slot with
`IO_ReadBlocked`/`IO_WriteBlocked` — a program hammering an unmapped port a
million times produces exactly one log line, ever. TESTED: ten emulated
seconds of a running DOS program with `io=debug` produced three `IO:` lines,
all startup announcements (the three I/O-delay-ns lines), nothing else.
Upstream's `log_io()` also filters out VGA palette ports, joystick 0x201 and —
the case that matters — **0x3DA reads, commented in the source as "a real
spammer."** 0x3DA polling is precisely the port storm IzarraVM campaigns keep
landing on, so the stock build cannot see it happen at all.

**The portlog build fixes this — BUILT AND WORKING.** The source change is
`dev_docs/2026-08-31-dosbox-x-portlog.patch`, whose header carries the
copy/apply/build recipe (robocopy the reference clone to a scratch tree
outside the repo, `git apply`, then MSBuild with `PlatformToolset=v143`
retargeted on the command line — the project pins v142/VS2019, not installed
here). **The patch is LOCAL ONLY: it is a GPL-2.0 derivative of DOSBox-X and
must never enter repo history.** `dev_docs/` is gitignored and that placement
is deliberate.

The built exe sits at `D:\ctd\dosbox-x-portlog\bin\x64\Release\dosbox-x.exe`
— outside the repo, in scratch, so it will not survive a scratch wipe (the
patch survives; a wipe costs a 63-second rebuild, not a lost investigation).
Drive it with `-DosboxExe D:\ctd\dosbox-x-portlog\bin\x64\Release\dosbox-x.exe
-LogTypes io`; no other script change is needed, since the patch routes the
trace through `LOG(LOG_IO,LOG_DEBUG)` so the ordinary `[log] io = debug|never`
conf key (which `-LogTypes io` sets) turns it on and off.

TESTED end to end: a mount-plus-`DIR`-plus-`VER` run at the 486 persona,
1.35 s wall, produced a 408 KB log holding 7091 port accesses, each line
carrying direction, width, port, value and the guest CS:IP that issued it.

**Caveat: unfiltered traces from a real game frame reach hundreds of MB.**
7000 accesses came from a trivial `DIR`; a game frame will produce far more.
There is no `-PortFilter` option to narrow to a port list before writing —
it does not exist yet — so scope the run with `-TimeLimit` and post-process.
Also: this is a *modified* DOSBox-X, so use it for tracing only and quote the
stock release binary for any behavioural claim.

## 7. Known gaps

- **No framebuffer capture.** IzarraVM campaigns grade on `end-frame.ppm`;
  this harness captures text and logs only, so it cannot be used to grade a
  frame. DOSBox-X can screenshot and record AVI via the mapper, but not from
  the command line.
- **No keyboard injection beyond the `[autoexec]` `-c` lines**, of which
  there are at most 11. A title waiting on a keypress looks hung here for the
  same reason it does on the IzarraVM side (see
  `dos-3d-title-waiting-looks-hung.md`): inject the keys the autoexec needs,
  or expect the run to sit at the prompt until `-WallLimit` kills it.
- **No scriptable instruction trace.** `LOGS`/`HEAVYLOG` need interactive
  debugger commands typed by hand; there is no CLI switch and no GDB stub to
  drive them programmatically.

## 8. Timing-comparison notes for wait-state work

DOSBox-X charges I/O access cost as a fraction of the millisecond cycle
budget:

```c
Bits delaycyc = (CPU_CycleMax * io_delay_ns[szidx]) / 1000000;
CPU_Cycles -= delaycyc;  CPU_IODelayRemoved += delaycyc;
// writes cost 3/4 of a read: (CPU_CycleMax * ns * 3) / (1000000 * 4)
```

Properties that matter when comparing against IzarraVM:

- The charge is a fraction of the per-millisecond cycle budget, scaled by
  `CPU_CycleMax`. The cycle cost of an `IN`/`OUT` therefore *changes when you
  change `cycles=`* — it is not a fixed per-instruction cost, and it is not
  what real hardware does.
- It is **zero inside emulator callbacks** (`last_callback != 0`), so
  DOSBox-X's own BIOS and DOS emulation pays nothing for I/O.
- **V86 faked I/O** (`CPU_ForceV86FakeIO_In/Out`) bypasses the charge
  entirely.
- One global delay per access width; no per-device granularity.
- `CPU_Cycles` is not clamped and may go negative.

Knobs, all confirmed present in `dosbox-x.reference.full.conf` and settable
through `-Set`: `[dosbox] iodelay` / `iodelay16` / `iodelay32` (ns, `-1` =
auto from the ISA clock, `0` = off), `[dosbox] irq delay ns`, `[dosbox] isa
bus clock` (`std8.3|std8|std6|std4.77|oc10|oc12|oc15|oc16`, or raw Hz),
`[dosbox] pci bus clock`, `[video] vmemdelay` and `lfb vmemdelay` (VGA memory
wait states, `0` = off).

Defaults observed at runtime: 8-bit **1020 ns**, 16-bit **660 ns**, 32-bit
**1320 ns**, matching the ISA derivation (8.5 / 5.5 / 11 BCLK at 8.333 MHz). A
source comment on the VGA side concedes the default there is "very
optimistic… not enough to significantly bring DOS games to a crawl" — a
written admission of the corner this section exists to document.
