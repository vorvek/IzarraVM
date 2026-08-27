# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
GATE ladder for C1 (the invalidation presence filter: per-key coverage carried
inline in the per-page scan vectors).

.DESCRIPTION
Two PINNED binaries, no knob. The arms differ by BINARY ONLY: C1 ships
default-ON and unconditional, because a knob would mean carrying both the
`Vec<BlockKey>` and the SoA layout plus a branch selecting between them inside
the hot loop -- the "default-off instruments tax the hot path" failure the
campaign has already recorded (design section 2.4).

Pass the two pins with -BaseSha256 / -ArmSha256. Both are verified before ANY
leg runs and a mismatch refuses the whole run. That check is the enforcement of
the copy-out step: the two commits are built sequentially into ONE `target/`,
and a same-target build overwrites the first binary, so the only thing standing
between "the copy step happened" and "the ladder measured one binary twice" is
this hash (design section 6.4).

ROWS (design section 6.5's MECHANISM and INERT CONTROL table, reduced to the
five this ladder carries):

  PRIMARY   nascar-586   n = 8   242.6 M keys scanned; the ONLY row with a
                                 line-level price (3.030% of wall across four
                                 lines, .bench/results/rip-census-20260826/
                                 nascar-rip.txt inline-exclusive)
  SECONDARY duke3d-586   n = 4   1.909 e9 keys, the heaviest row on the board.
                                 SMC-FRAGILE: small write-side deltas flip it
                                 +/-5% (`sb16-dsp-merge-duke-regression`), so a
                                 duke regression is REVIEW, never STOP.
  SECONDARY doom-486     n = 4   73.1 M keys at 3.51 M keys/wall-second --
                                 nascar's RATE on a third of nascar's wall.
                                 Carries the campaign's strongest identity pin.
  SECONDARY doom-586     n = 4   69.7 M keys at 4.01 M keys/wall-second.
  CONTROL   quake-586    n = 4   30 scan calls in the whole run. The mechanism
                                 provably does not engage, so this row's
                                 arm-to-arm spread IS the rig's noise plus this
                                 binary pair's layout confound.

wolf3d and tombraid are NOT rows here: wolf3d-586 makes 5,177 scan calls and
tombraid-586 makes 126 (design section 1.2), so neither is a mechanism row, and
this ladder does not need two more inert controls at 69 s and 128 s a leg.

DEVIATION FROM THE DESIGN, STATED RATHER THAN BURIED. Section 6.5 names FOUR
inert controls (quake-586 30 calls, prince-486 16, gp2-586 115, tombraid-586
126) and defines the wall bar `F` as the largest absolute min-wall delta across
all four, measured in the same session. This ladder carries ONE of them. `F` is
therefore computed from quake-586 alone and is recorded as
`measured_floor_F_one_row` -- a one-row estimate of the floor, NOT the design's
four-row bar. Widen it with prince-486, gp2-586 and tombraid-586 legs on the
same binary pair before quoting `F` as the design's bar. The last measured set
spanned -1.20% to +1.93%, 3.13 points, which is why "+/-1.5%" was once a bar
tighter than the rig could measure.

Also NOT replicated here: nascar's frame CONTRACT. The scoreboard grades that
row with a second emulator invocation at the 0.445e9 anchor (pin
383cfebd4a68a669ab4908764205232b4e02c8d0976ab2cc575f032184d529d5, plus coverage
and colour bands at the budget) because its end-of-budget frame lands
mid-attract with the camera in flight and its hash samples the demo's PHASE.
That machinery exists to survive a legitimate cadence change ACROSS commits. It
is not what an A/B needs: this ladder requires the two arms' end-of-budget
frames to be BIT-IDENTICAL to each other, which is strictly stronger than the
bands, and cheaper than an extra anchor run per leg. Run the scoreboard for the
contract; run this for the A/B.

IDENTITY, and it is a STOP. Every row runs its own identity pair FIRST (one BASE
leg, one ARM leg). C1 is a pure host-side data-structure change -- the function's
return value, its counters and its side effects are bit-identical by
construction (design section 3.5) -- so COUNTER MOVEMENT IS A DEFECT, not a
tolerance or a tuning question. An identity failure STOPS THAT ROW LOUDLY and
the script continues with the other rows, recording STOPPED in the summary.

ONE identity field has a SPECIFIC diagnosis and the script prints it:
`perf.smc_scan_keys`. Today an `entries`-miss row takes `continue` WITHOUT being
written back as a survivor, so it falls in the drain range and is removed -- the
scan SELF-HEALS stale rows. Under the pre-filter a stale row whose `lens` says
"no overlap" is skipped BEFORE the probe and is compacted as a survivor, so it
stays on the page and every later scan of that page counts it. Therefore
`smc_scan_keys` moving means `entries_get_misses > 0` means the bijection has a
hole means the self-heal was load-bearing: STOP and re-derive section 3.1. These
are ONE predicate, not two independent confirmations (design section 6.2).

MECHANISM, and it is the ACCEPT -- not the wall (design section 6.5 item 3).
The identity legs' profile JSONs are mined for the smc-census units at
`smc_census.phases[<whole>].units` and every closure the design names is
evaluated and recorded per row:

  probes_elided(ON) + keys_surviving(ON) == keys_surviving(OFF)   [THE accept]
  probes_elided(ON) <= keys_surviving(OFF)                        [the ceiling]
  probes_elided + entries_get_calls == keys_scanned               [ON arm]
  entries_get_calls == keys_killed + keys_surviving
                       + lane_accept_keys + entries_get_misses    [ON arm]
  survivors_moved == keys_surviving + lane_accept_keys
                       + probes_elided                            [ON arm]
  probe_divergences == 0                                          [ON arm]
  entries_get_misses == 0                                         [both arms]
  retire_calls == retire_calls_effective                          [both arms]
  keys_scanned(ON) == keys_scanned(OFF)                           [like with like]

An equality, not a ratio, and it runs in one direction only: a skipped row is a
survivor that never reached the probe, so (INV) makes the skipped set a SUBSET
of the surviving set. `probes_elided / keys_scanned` is REPORTED, never a bar.

NONE of this aborts the script. `probes_elided`, `entries_get_calls` and
`probe_divergences` do not exist on a plain release build -- they are
`#[cfg(feature = "smc-census")]` fields (design section 6.3), and
`entries_get_calls` does not exist in the tree at all yet. When a field is
absent the closure records `not_evaluated` with the missing field named. The
orchestrator adjudicates; this script reports.

ESTIMATORS, per row, over the PAIR-MATCHED per-pair ratios
r_i = (pair BASE min wall) / (pair ARM min wall), pairing declared before the
run as index-matched-within-arm in pair order:

  sign        count of pairs with r_i > 1 (an exact tie counts as a BASE win,
              which is the strict reading of the bar; with real wall floats a
              tie does not occur)
  min_ratio   min(all BASE walls) / min(all ARM walls)   -- the min-wall cross-check
  mean_ratio  arithmetic mean of the r_i
  geomean     exp(mean(ln r_i))
  median      median of the r_i
  lower95     one-sided 95% lower bound, t-approximation on ln(r_i):
              exp( mean(ln r) - t(0.95, n-1) * sd(ln r) / sqrt(n) )
              Student t rather than a bootstrap because n is 4 or 8 and a
              bootstrap of 4 points resamples the same four numbers; the t table
              is inlined below. sd = 0 degenerates to the geomean.

Design section 6.5 item 5 asks for the PROTOCOL agreement set and calls point
estimators disagreeing by more than 0.005 a contaminated ladder. That spread is
computed per row as `estimator_spread` (max minus min over geomean, median,
min-wall and lower95) and flagged; it is a re-run signal for the orchestrator,
not an abort.

MERGE LABELS, primary row, declared in advance:

  WALL_REAL        lower95 > 1.0  AND  sign >= 6/8  AND  min_ratio >= 1.005
  WALL_REGRESSION  sign < 4/8  OR  mean_ratio < 0.995        -- STOP
  WALL_NEUTRAL     anything else

The 0.995 DEADBAND is deliberate: mean < 1.0 with no deadband is NOT a
regression when sign and min-wall hold, because the rig's noise floor is +/- 2%
and free (`inert-controls-measure-the-noise-floor`). Secondary rows use the same
shape with thresholds scaled to their n, and their labels are ADVISORY: only the
primary row's wall verdict can fail this script.

CONTAMINATION: the re-run bar is cpu_before/after > 25 (this host's idle sits
14-23%). A contaminated leg is DISCARDED to its own json and re-run. Legs are
pinned to one logical processor.

ARTIFACTS, all under the mandatory -OutDir:
  <row>/<label>.json      the emulator's own profile json, one per leg
  <row>/<label>.ppm/.out/.err
  <row>/legs.json         the distilled leg records for that row
  <row>/GATE.json         identity, census closures, pairs and estimators
  SUMMARY.json            one object, every row, the verdict
Everything is built as a plain hashtable / pscustomobject and piped straight to
ConvertTo-Json. Do NOT interpose Format-List or Format-Table: a prior
SUMMARY.json was ruined that way.

EXPECTED RUNTIME. Per-leg walls are taken from the newest scoreboard artifacts
under .bench/results/ (`scoreboard.json`, field `wall_seconds`), rounded up:
  nascar-586    65 s  (64.737 arm0-g2 / 61.510 arm1-g2, 2026-08-26)
  duke3d-586   205 s  (201.075 arm0-g5 / 249.732 arm1-g5, 2026-08-26; 202.357 /
                       197.472 on 2026-08-25 -- the 249.7 leg is the outlier
                       this ladder's contamination bar exists to reject)
  doom-486      21 s  (20.775 arm0-g1 / 19.662 arm1-g1, 2026-08-26)
  doom-586      18 s  (17.436 arm0-g1 / 16.562 arm1-g1, 2026-08-26)
  quake-586     21 s  (21.897, scoreboard-20260826-081213-armon-inline-verify)
plus a per-leg fixture copy charged from the tree sizes (nascar1_c 27.2 MB over
364 files, jemmex_doom_c 4.7 MB, quake_c 18.3 MB, duke3d_c 46.1 MB). The
estimate is printed at start and is a LOWER bound: discarded contaminated legs
are not in it.

See dev_docs/specs/2026-08-27-c1-presence-filter-design.md sections 6.1-6.5.
Adapted from .bench/scripts/ladder-c2-podkey.ps1 (PR #742), which is itself a
copy of the untracked .bench/scripts/ladder-d-elision-b.ps1 under its own name
and its own lock path.
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` (the shape
# an outer PowerShell produces from `-Fixtures @('a','b')`) ran ONE row of a
# two-row sweep and EXITED 0. With positional binding off, the stray token is a
# binder error before one line of this script runs. The safe multi-row spelling
# is the COMMA string: `-Rows doom-486,doom-586`. -Rows carries no ValidateSet
# because that fires on the comma string as ONE value; Resolve-RowSelection
# splits it and validates every name instead.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$BaseExecutable,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$ArmExecutable,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$OutDir,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$BaseSha256,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$ArmSha256,
    [string[]]$Rows = @("nascar-586", "duke3d-586", "doom-486", "doom-586", "quake-586"),
    # Resolve -Rows, print the selection, exit 0. Exists so the self-test's
    # green control can prove a well-formed invocation binds without running a
    # leg. Run-set arguments still have to be supplied; dummies are fine.
    [switch]$BindCheck,
    [Parameter(Mandatory, ParameterSetName = "SelfTest")][switch]$SelfTest,
    [int]$PrimaryPairs = 8,
    [int]$SecondaryPairs = 4,
    [int]$ProcessorIndex = 8,
    [switch]$DukeShort,
    [switch]$IdentityOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))
$bench = Join-Path $repo ".bench"
$lockPath = Join-Path $bench "locks\c1-scan-spans-ladder.lock"

# The sha pins are normalised BELOW the -SelfTest / -BindCheck dispatch: the two
# parameters are mandatory only in the Run set, so they are null under -SelfTest
# and .ToLowerInvariant() on null throws under StrictMode.

# doom's guest oracle. gametics is THE invariant that gates: PROTOCOL's
# 2026-08-11 correction retired the COUNTER half of the 8G/586 pin (the run is
# now ~48 guest seconds instead of ~85 after the 166 MHz/64 MB respec) and left
# gametics 2134 standing, unchanged, at both personas. realtics is NOT pinned to
# a value here: it is SESSION-LOCAL (`doom-realtics-not-cross-session`; the same
# commit has produced 813 and 769 hours apart). Within ONE ladder session on two
# binaries that must be counter-identical it must still MATCH ACROSS ARMS, and
# that is what is checked. The scoreboard's per-persona band is carried only as
# a warning, so a session that drifts out of band says so without failing a row
# on a quantity the campaign has already ruled session-local.
$doomGametics = 2134

# The PUBLIC row names, the list the removed ValidateSet carried. Keep it equal
# to Get-RowTable's names; duke3d-586-short is selected via -DukeShort, never
# by name, exactly as before.
$knownRows = @("nascar-586", "duke3d-586", "doom-486", "doom-586", "quake-586")

# Parse -Rows into a validated list of row names. It splits on the comma ITSELF
# because `pwsh -File ... -Rows a,b` binds ONE string "a,b" to the [string[]]
# parameter. The two-token shape (`-Rows a b`) never gets here: PositionalBinding
# is off for the whole script, so the binder rejects the second token first.
# Copied from scripts/run-fixture-scoreboard.ps1's Resolve-FixtureSelection.
function Resolve-RowSelection([string[]]$Specification, [string[]]$KnownNames) {
    $entries = @()
    foreach ($element in @($Specification)) {
        if ($null -eq $element) {
            throw "-Rows contains a null entry. Name each row, comma-separated."
        }
        $entries += ([string]$element).Split(',')
    }
    $selected = @()
    foreach ($entry in $entries) {
        $name = ([string]$entry).Trim()
        if ($name -eq "") {
            throw ("-Rows contains an empty entry. A stray comma would silently " +
                "shrink the sweep, so it is refused instead.")
        }
        if (@($KnownNames) -notcontains $name) {
            throw "Unknown row '$name'. Known: $(@($KnownNames) -join ', ')"
        }
        if ($selected -contains $name) {
            throw ("-Rows names '$name' more than once. The ladder runs each row " +
                "once, so a repeat would report fewer rows than the caller asked for.")
        }
        $selected += $name
    }
    return ,$selected
}

function Assert-BinderSelfTestEqual($Actual, $Expected, [string]$Message) {
    if ($Actual -ne $Expected) {
        throw "self-test failed: $Message (expected $Expected, got $Actual)"
    }
}

function Assert-BinderSelfTestThrows([scriptblock]$Action, [string]$Expected,
    [string]$Message) {
    $failure = $null
    try { $null = & $Action } catch { $failure = $_.Exception.Message }
    if ($null -eq $failure) { throw "self-test failed: $Message did not throw" }
    if (-not $failure.Contains($Expected, [StringComparison]::Ordinal)) {
        throw "self-test failed: $Message threw '$failure', expected '$Expected'"
    }
}

# Half drives Resolve-RowSelection directly. The other half spawns this very
# script under `pwsh -File` with the mangled two-token shape, because that
# failure happens in the parameter binder -- before any function here runs --
# and only a real child invocation can prove the guard fires there. The
# campaign rule applies: the guard must go RED on the broken input, and a green
# control must show the child harness works.
function Invoke-BinderGuardSelfTest {
    $split = Resolve-RowSelection @("doom-486,doom-586") $knownRows
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "doom-486" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "doom-586" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" doom-486 , doom-586") $knownRows
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486,no-such-row") $knownRows } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486,") $knownRows } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486", "doom-486") $knownRows } `
        "more than once" "a row named twice"

    $pwshExecutable = (Get-Process -Id $PID).Path
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-bindsel-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-BaseExecutable", "self-test-dummy", "-ArmExecutable", "self-test-dummy",
                "-OutDir", "self-test-dummy", "-BaseSha256", "self-test-dummy",
                "-ArmSha256", "self-test-dummy",
                "-Rows", "doom-486", "doom-586", "-BindCheck")
            RedirectStandardOutput = $outputPath
            RedirectStandardError  = $failurePath
            PassThru               = $true
            NoNewWindow            = $true
        }
        # RED: the two-token shape must be a binder error, never a one-row run.
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "self-test failed: the mangled -Rows child never exited"
        }
        if ($process.ExitCode -eq 0) {
            throw ("self-test failed: the mangled two-token -Rows invocation exited 0. " +
                "The silent-subset hazard is back: the second row bound positionally.")
        }
        $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
        if ($failureText -notmatch 'doom-586') {
            throw ("self-test failed: the mangled -Rows child failed, but not on the " +
                "stray token. stderr: $failureText")
        }

        # GREEN control: the comma spelling of the same selection must bind and
        # resolve, or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-BaseExecutable", "self-test-dummy", "-ArmExecutable", "self-test-dummy",
            "-OutDir", "self-test-dummy", "-BaseSha256", "self-test-dummy",
            "-ArmSha256", "self-test-dummy",
            "-Rows", "doom-486,doom-586", "-BindCheck")
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "self-test failed: the -BindCheck control child never exited"
        }
        if ($process.ExitCode -ne 0) {
            $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
            throw ("self-test failed: the well-formed -BindCheck control exited " +
                "$($process.ExitCode); the red row above is therefore meaningless. " +
                "stderr: $failureText")
        }
        $listing = [string](Get-Content -LiteralPath $outputPath -Raw)
        if ($listing -notmatch 'doom-586') {
            throw "self-test failed: the -BindCheck control did not echo the selection"
        }
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-c1-scan-spans self-test passed"
}

if ($SelfTest) {
    Invoke-BinderGuardSelfTest
    exit 0
}

$Rows = Resolve-RowSelection $Rows $knownRows
if ($BindCheck) {
    Write-Host ("bind-check ok: rows " + ($Rows -join ", "))
    exit 0
}

$baseSha = $BaseSha256.ToLowerInvariant()
$armSha = $ArmSha256.ToLowerInvariant()

# ---------------------------------------------------------------------------
# The rows. Fixture folder, arguments, cycles and injection are copied VERBATIM
# from scripts/run-fixture-scoreboard.ps1's Get-FixtureTable (doom-486
# :1029-1042, doom-586 :1043-1051, quake-586 :1052-1061, duke3d-586 :1115-1131,
# duke3d-586-short :1132-1153, nascar-586 :1154-1167), assembled the way
# Get-FixtureArguments does at :1663-1688. Do not paraphrase them: the recorded
# invariants were measured under exactly these arguments, so changing a persona,
# a memory size or a video card invalidates the invariant silently instead of
# failing.
# ---------------------------------------------------------------------------

function Get-RowTable {
    $duke = if ($DukeShort) {
        [pscustomobject]@{
            name = "duke3d-586-short"; folder = "duke3d_short_c"; role = "secondary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "33200000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = "DUKEMARK.TXT"; timedemo = $false
            realticsMinimum = $null; realticsMaximum = $null
            mechanism = $true
            legSeconds = 96; copySeconds = 10
            why = "SECONDARY mechanism (709.8 M keys). Cheap substitute for duke3d-586; PROTOCOL sanctions it for laddering, re-run the LONG row before merge. Its OFF arm has been measured varying 16.87% across three runs, so its min-wall is a lucky draw rather than an estimate: a mechanism row for the COUNTERS and a weak one for the wall."
        }
    }
    else {
        [pscustomobject]@{
            name = "duke3d-586"; folder = "duke3d_c"; role = "secondary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "79680000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = "DUKEMARK.TXT"; timedemo = $false
            realticsMinimum = $null; realticsMaximum = $null
            mechanism = $true
            legSeconds = 205; copySeconds = 10
            why = "SECONDARY mechanism (1.909 e9 keys, 26.66 per call, the heaviest row on the board). SMC-FRAGILE: a regression here is REVIEW, not STOP."
        }
    }

    @(
        [pscustomobject]@{
            name = "nascar-586"; folder = "nascar1_c"; role = "primary"
            # NO --video: the scoreboard row omits it and the invariants were
            # measured that way.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = "4980000000"
            injection = @(); resultPpm = $true; cdImage = $null
            qconsole = $false; dukemarkFile = $null; timedemo = $false
            realticsMinimum = $null; realticsMaximum = $null
            mechanism = $true
            legSeconds = 65; copySeconds = 6
            why = "PRIMARY mechanism (242.6 M keys; 3.030% of wall across four lines, the only row with a line-level price)"
        }
        $duke
        [pscustomobject]@{
            name = "doom-486"; folder = "jemmex_doom_c"; role = "secondary"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = "8000000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = $null; timedemo = $true
            # Band only, and only as a warning. See $doomGametics above.
            realticsMinimum = 2814; realticsMaximum = 2964
            mechanism = $true
            legSeconds = 21; copySeconds = 3
            why = "SECONDARY mechanism (73.1 M keys at 3.51 M/wall-s, nascar's RATE; carries the campaign's strongest identity pin, gametics 2134)"
        }
        [pscustomobject]@{
            name = "doom-586"; folder = "jemmex_doom_c"; role = "secondary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "6640000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = $null; timedemo = $true
            realticsMinimum = 951; realticsMaximum = 1021
            mechanism = $true
            legSeconds = 18; copySeconds = 3
            why = "SECONDARY mechanism (69.7 M keys at 4.01 M/wall-s, the highest scan rate of the two doom rows; cheapest leg on the ladder)"
        }
        [pscustomobject]@{
            name = "quake-586"; folder = "quake_c"; role = "control"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "6200000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $true; dukemarkFile = $null; timedemo = $false
            realticsMinimum = $null; realticsMaximum = $null
            mechanism = $false
            legSeconds = 21; copySeconds = 5
            why = "INERT CONTROL (30 scan calls in the whole run; invalidate_physical_range absent from quake-class rows). Its spread IS the floor."
        }
    ) | Where-Object { $Rows -contains $_.name -or ($DukeShort -and $_.name -eq "duke3d-586-short" -and $Rows -contains "duke3d-586") }
}

# ---------------------------------------------------------------------------
# Identity fields. Design section 6.2's list verbatim: the clocks, the two
# instruction counters, and the WRITE-SIDE set. Dotted paths, walked by
# Get-Nested. `decline_memo_hits` lives under direct_stalls, not perf.
# ---------------------------------------------------------------------------

$identityFields = @(
    "executed_cpu_core_clocks"
    "scaled_bus_clocks"
    "raw_bus_clocks"
    "master_ticks"
    "perf.instructions"
    "perf.jit_direct_insns"
    # The write-side set. smc_scan_keys carries the section 6.2 diagnosis.
    "perf.smc_scan_calls"
    "perf.smc_scan_keys"
    "perf.smc_lane_accepts"
    "perf.smc_lane_reject_width"
    "perf.smc_lane_reject_address"
    "perf.code_invalidations"
    "perf.smc_narrow_kills"
    "perf.smc_heat_demotions"
    "perf.jit_direct_exit_code_watch"
    "perf.jit_direct_blocks_installed"
    "direct_stalls.decline_memo_hits"
    "stop.kind"
    "stop.requested"
)

# The one field whose movement has a named diagnosis rather than a generic one.
$scanKeysField = "perf.smc_scan_keys"

# Watched, but not an identity STOP on its own: the model ladder records it as
# occupancy and C1 does not touch admission.
$occupancyFields = @("perf.jit_direct_entries")

# ---------------------------------------------------------------------------
# The smc-census units the design's closures read. All live under
# `smc_census.phases[<whole>].units` (crates/izarravm/src/main.rs:2760-2824,
# emitted from main.rs:2115 under `--features smc-census`). The last three do
# NOT exist on a plain build -- probes_elided / entries_get_calls /
# probe_divergences are design section 6.3's NEW fields, and entries_get_calls
# is not in the tree at all yet -- so every one is read defensively and a
# missing field turns its closures into `not_evaluated`.
# ---------------------------------------------------------------------------

$censusFields = @(
    "keys_scanned", "keys_surviving", "keys_killed", "entries_get_misses",
    "lane_accept_keys", "survivors_moved", "keys_surviving_in_kill_calls",
    "scan_calls_absent_page", "scan_calls", "window_len_sum",
    "retire_calls", "retire_calls_effective",
    "probes_elided", "entries_get_calls", "probe_divergences"
)

# ---------------------------------------------------------------------------
# Binary pins. Before anything else.
# ---------------------------------------------------------------------------

if (-not (Test-Path -LiteralPath $BaseExecutable)) { throw "Missing BASE executable: $BaseExecutable" }
if (-not (Test-Path -LiteralPath $ArmExecutable)) { throw "Missing ARM executable: $ArmExecutable" }

$baseActual = (Get-FileHash -LiteralPath $BaseExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
$armActual = (Get-FileHash -LiteralPath $ArmExecutable -Algorithm SHA256).Hash.ToLowerInvariant()

if ($baseActual -ne $baseSha) {
    throw ("BASE binary sha256 mismatch. Expected $baseSha, got $baseActual for $BaseExecutable. " +
        "REFUSING TO RUN: the copy-out step in the build procedure did not produce the pinned binary.")
}
if ($armActual -ne $armSha) {
    throw ("ARM binary sha256 mismatch. Expected $armSha, got $armActual for $ArmExecutable. " +
        "REFUSING TO RUN: the copy-out step in the build procedure did not produce the pinned binary.")
}
if ($baseActual -eq $armActual) {
    throw "BASE and ARM are the same binary. There is nothing to ladder."
}

$rowTable = @(Get-RowTable)
if ($rowTable.Count -eq 0) { throw "No rows selected." }

foreach ($row in $rowTable) {
    $folder = Join-Path $bench $row.folder
    if (-not (Test-Path -LiteralPath $folder)) { throw "Missing fixture: $folder" }
    if ($row.cdImage) {
        $cd = Join-Path $bench $row.cdImage
        if (-not (Test-Path -LiteralPath $cd)) { throw "Missing CD image for $($row.name): $cd" }
    }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $bench "locks") | Out-Null

if (Test-Path -LiteralPath $lockPath) {
    $recordedPid = 0
    if ([int]::TryParse((((Get-Content -LiteralPath $lockPath -Raw).Trim()) -split '\s+')[0], [ref]$recordedPid)) {
        $alive = $null
        try { $alive = Get-Process -Id $recordedPid -ErrorAction Stop } catch { $alive = $null }
        if ($alive) { throw "Campaign lock held by live PID $recordedPid" }
    }
}
"$PID c1-scan-spans-ladder" | Set-Content -LiteralPath $lockPath -NoNewline

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Get-HostCpuPercent {
    try {
        $sample = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -ErrorAction Stop |
            Where-Object Name -eq '_Total'
        if ($null -eq $sample) { return -1 }
        return [int]$sample.PercentProcessorTime
    }
    catch { return -1 }
}

function Get-Field($Object, [string]$Name) {
    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Get-Nested($Object, [string]$Path) {
    $node = $Object
    foreach ($part in $Path.Split('.')) {
        $node = Get-Field $node $part
        if ($null -eq $node) { return $null }
    }
    return $node
}

# The census's WHOLE-run phase. Located by label, not by index: the reporter
# emits [whole, windowed] today (main.rs:2618-2621) but a label lookup does not
# silently start reading the windowed phase if that order ever changes.
function Get-CensusUnits($Report) {
    $census = Get-Field $Report "smc_census"
    if ($null -eq $census) { return $null }
    $phases = Get-Field $census "phases"
    if ($null -eq $phases) { return $null }
    $chosen = $null
    foreach ($phase in @($phases)) {
        $label = Get-Field $phase "label"
        if ("$label" -match '^(?i)whole') { $chosen = $phase; break }
    }
    if ($null -eq $chosen) { $chosen = @($phases)[0] }
    if ($null -eq $chosen) { return $null }
    $units = Get-Field $chosen "units"
    if ($null -eq $units) { return $null }
    $out = [ordered]@{ phase_label = (Get-Field $chosen "label") }
    foreach ($field in $censusFields) { $out[$field] = Get-Field $units $field }
    return [pscustomobject]$out
}

function Set-LadderEnvironment {
    # The campaign's shipped default arm, set EXPLICITLY. Never unset a knob to
    # turn it off: an empty IZARRAVM_JIT reads as ON, and both `=0` spellings
    # below are members of their knob's own spelling table, which panics on a
    # typo rather than defaulting silently.
    $env:IZARRAVM_JIT = "1"
    $env:IZARRAVM_JIT16 = "1"
    $env:IZARRAVM_JIT16_486 = "1"
    $env:IZARRAVM_ONE_LOOKUP_STORE = "1"
    $env:IZARRAVM_ONE_LOOKUP_LOAD = "1"
    $env:IZARRAVM_DIRECT_BARRIER_CENSUS = "0"
    $env:IZARRAVM_ROTATE_ROWS = "1"
    $env:IZARRAVM_COUNT_LANES = "1"
    $env:IZARRAVM_FPU_LOOP_ROWS = "1"
    $env:IZARRAVM_V86_LOOP_ROWS = "1"
    $env:IZARRAVM_IMM8_LANES = "1"
    $env:IZARRAVM_DISP_LANES = "1"
    $env:IZARRAVM_DISP_STORE_LANES = "1"
    $env:IZARRAVM_DIRECT_RETF_V86 = "v86"
    $env:IZARRAVM_DIRECT_POLL_SKIP = "1"
    $env:IZARRAVM_DIRECT_IN_IMM8_CALLOUT = "1"
    $env:IZARRAVM_DIRECT_EAGER_FLAGS = "1"
    $env:IZARRAVM_DIRECT_HOLD_LOAD_BIAS = "0"
    $env:IZARRAVM_DIRECT_ALIGN_TEST_AL = "0"
    # Every observer OFF. A default-off instrument still taxes the hot path once
    # armed, and several only do work when the JIT is active -- i.e. they tax
    # exactly the runs this is trying to time.
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

# One-sided 95% Student t. n is 4 or 8 here; a bootstrap over four points would
# resample the same four numbers and report its own optimism as precision.
function Get-TCritical([int]$Df) {
    $table = @{
        1 = 6.314; 2 = 2.920; 3 = 2.353; 4 = 2.132; 5 = 2.015; 6 = 1.943; 7 = 1.895
        8 = 1.860; 9 = 1.833; 10 = 1.812; 11 = 1.796; 12 = 1.782; 13 = 1.771
        14 = 1.761; 15 = 1.753; 16 = 1.746; 17 = 1.740; 18 = 1.734; 19 = 1.729
        20 = 1.725; 21 = 1.721; 22 = 1.717; 23 = 1.714; 24 = 1.711; 25 = 1.708
        26 = 1.706; 27 = 1.703; 28 = 1.701; 29 = 1.699; 30 = 1.697
    }
    if ($Df -lt 1) { return [double]::NaN }
    if ($table.ContainsKey($Df)) { return [double]$table[$Df] }
    return 1.645
}

function Measure-Ladder([double[]]$Ratios, [double[]]$BaseWalls, [double[]]$ArmWalls) {
    $n = $Ratios.Count
    $sign = @($Ratios | Where-Object { $_ -gt 1.0 }).Count
    $mean = ($Ratios | Measure-Object -Average).Average
    $sorted = @($Ratios | Sort-Object)
    $median = if ($n % 2 -eq 1) { $sorted[($n - 1) / 2] }
    else { ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2.0 }
    $logs = @($Ratios | ForEach-Object { [math]::Log($_) })
    $logMean = ($logs | Measure-Object -Average).Average
    $geomean = [math]::Exp($logMean)
    $lower95 = $geomean
    if ($n -gt 1) {
        $ss = 0.0
        foreach ($l in $logs) { $ss += ($l - $logMean) * ($l - $logMean) }
        $sd = [math]::Sqrt($ss / ($n - 1))
        if ($sd -gt 0) {
            $lower95 = [math]::Exp($logMean - (Get-TCritical ($n - 1)) * $sd / [math]::Sqrt($n))
        }
    }
    $baseMin = ($BaseWalls | Measure-Object -Minimum).Minimum
    $armMin = ($ArmWalls | Measure-Object -Minimum).Minimum
    $baseMean = ($BaseWalls | Measure-Object -Average).Average
    $armMean = ($ArmWalls | Measure-Object -Average).Average
    $minRatio = $baseMin / $armMin
    # Design section 6.5 item 5: the PROTOCOL agreement set. Point estimators
    # disagreeing by more than 0.005 means the ladder is contaminated -- re-run,
    # do not pick.
    $agreement = @($geomean, $median, $minRatio, $lower95)
    $spread = ($agreement | Measure-Object -Maximum).Maximum - ($agreement | Measure-Object -Minimum).Minimum
    [pscustomobject]@{
        pairs             = $n
        sign_arm_wins     = $sign
        mean_ratio        = $mean
        geomean_ratio     = $geomean
        median_ratio      = $median
        min_wall_ratio    = $minRatio
        lower95           = $lower95
        estimator_spread  = $spread
        estimators_agree  = ($spread -le 0.005)
        min_wall_base     = $baseMin
        min_wall_arm      = $armMin
        mean_wall_base    = $baseMean
        mean_wall_arm     = $armMean
        mean_wall_ratio   = $baseMean / $armMean
    }
}

function Get-WallLabel($Stats, [bool]$IsPrimary) {
    $n = $Stats.pairs
    $sign = $Stats.sign_arm_wins
    $realSign = [int][math]::Ceiling(0.75 * $n)
    $regression = ($sign * 2 -lt $n) -or ($Stats.mean_ratio -lt 0.995)
    if ($regression) {
        return $(if ($IsPrimary) { "WALL_REGRESSION (STOP)" } else { "WALL_REGRESSION (REVIEW)" })
    }
    if ($Stats.lower95 -gt 1.0 -and $sign -ge $realSign -and $Stats.min_wall_ratio -ge 1.005) {
        return "WALL_REAL"
    }
    return "WALL_NEUTRAL"
}

# ---------------------------------------------------------------------------
# The census closures. Every one returns pass / fail / not_evaluated, and NONE
# of them aborts: the orchestrator adjudicates. See design sections 6.1(b),
# 6.2, 6.3 and 6.5 item 3.
# ---------------------------------------------------------------------------

function New-Closure([string]$Name, [string]$Statement, $Left, $Right, [string]$Comparison, [string[]]$Needed, [string]$Meaning) {
    $missing = @($Needed | Where-Object { $_ -like "MISSING:*" })
    if ($missing.Count -gt 0) {
        return [pscustomobject]@{
            closure = $Name; statement = $Statement; result = "not_evaluated"
            missing_fields = @($missing | ForEach-Object { $_ -replace '^MISSING:', '' })
            left = $null; right = $null; meaning = $Meaning
        }
    }
    $ok = switch ($Comparison) {
        "eq" { [decimal]$Left -eq [decimal]$Right }
        "le" { [decimal]$Left -le [decimal]$Right }
        default { throw "unknown comparison $Comparison" }
    }
    [pscustomobject]@{
        closure = $Name; statement = $Statement
        result = $(if ($ok) { "pass" } else { "FAIL" })
        missing_fields = @()
        left = $Left; right = $Right; meaning = $Meaning
    }
}

# Returns the value, or the marker string "MISSING:<label>.<field>" so
# New-Closure can name exactly which field was absent instead of silently
# treating a null as a zero.
function Get-CensusValue($Units, [string]$Field, [string]$ArmLabel) {
    if ($null -eq $Units) { return "MISSING:$ArmLabel.smc_census" }
    $value = Get-Field $Units $Field
    if ($null -eq $value) { return "MISSING:$ArmLabel.$Field" }
    return $value
}

function Test-CensusClosures($BaseUnits, $ArmUnits) {
    $closures = [Collections.Generic.List[object]]::new()

    $bSurv = Get-CensusValue $BaseUnits "keys_surviving" "BASE"
    $bScan = Get-CensusValue $BaseUnits "keys_scanned" "BASE"
    $bMiss = Get-CensusValue $BaseUnits "entries_get_misses" "BASE"
    $bRet = Get-CensusValue $BaseUnits "retire_calls" "BASE"
    $bRetE = Get-CensusValue $BaseUnits "retire_calls_effective" "BASE"

    $aSurv = Get-CensusValue $ArmUnits "keys_surviving" "ARM"
    $aScan = Get-CensusValue $ArmUnits "keys_scanned" "ARM"
    $aMiss = Get-CensusValue $ArmUnits "entries_get_misses" "ARM"
    $aKill = Get-CensusValue $ArmUnits "keys_killed" "ARM"
    $aLane = Get-CensusValue $ArmUnits "lane_accept_keys" "ARM"
    $aMoved = Get-CensusValue $ArmUnits "survivors_moved" "ARM"
    $aRet = Get-CensusValue $ArmUnits "retire_calls" "ARM"
    $aRetE = Get-CensusValue $ArmUnits "retire_calls_effective" "ARM"
    $aElided = Get-CensusValue $ArmUnits "probes_elided" "ARM"
    $aGets = Get-CensusValue $ArmUnits "entries_get_calls" "ARM"
    $aDiv = Get-CensusValue $ArmUnits "probe_divergences" "ARM"

    # THE ACCEPT. An equality, not a ratio, and it runs in this direction and no
    # other: a skipped row is a survivor that never reached the probe, so (INV)
    # makes the skipped set a SUBSET of the surviving set.
    $closures.Add((New-Closure "accept_equality" `
                "probes_elided(ON) + keys_surviving(ON) == keys_surviving(OFF)" `
                $(if ($aElided -is [string] -or $aSurv -is [string]) { $null } else { [decimal]$aElided + [decimal]$aSurv }) `
                $(if ($bSurv -is [string]) { $null } else { $bSurv }) "eq" @($aElided, $aSurv, $bSurv) `
                "THE mechanism accept. Elision moves rows out of keys_surviving and into probes_elided; the two arms' totals must reconcile exactly."))

    $closures.Add((New-Closure "accept_ceiling" `
                "probes_elided(ON) <= keys_surviving(OFF)" `
                $aElided $bSurv "le" @($aElided, $bSurv) `
                "The same fact as a ceiling. Catches a filter skipping a row it had no right to skip, read from counters rather than from an assertion."))

    $closures.Add((New-Closure "keys_scanned_like_for_like" `
                "keys_scanned(ON) == keys_scanned(OFF)" `
                $aScan $bScan "eq" @($aScan, $bScan) `
                "keys_scanned stays the WINDOW LENGTH (design 2.3), computed before the loop, so nothing in the loop can move it. If this fails the accept equality is not comparing like with like."))

    $closures.Add((New-Closure "probe_partition" `
                "probes_elided + entries_get_calls == keys_scanned  [ON]" `
                $(if ($aElided -is [string] -or $aGets -is [string]) { $null } else { [decimal]$aElided + [decimal]$aGets }) `
                $(if ($aScan -is [string]) { $null } else { $aScan }) "eq" @($aElided, $aGets, $aScan) `
                "Every scanned row either took the probe or was elided. A non-closing partition means a third path exists."))

    $closures.Add((New-Closure "probe_disposition" `
                "entries_get_calls == keys_killed + keys_surviving + lane_accept_keys + entries_get_misses  [ON]" `
                $aGets `
                $(if ($aKill -is [string] -or $aSurv -is [string] -or $aLane -is [string] -or $aMiss -is [string]) { $null }
                else { [decimal]$aKill + [decimal]$aSurv + [decimal]$aLane + [decimal]$aMiss }) `
                "eq" @($aGets, $aKill, $aSurv, $aLane, $aMiss) `
                "Every issued probe ends in exactly one disposition."))

    $closures.Add((New-Closure "survivors_moved" `
                "survivors_moved == keys_surviving + lane_accept_keys + probes_elided  [ON]" `
                $aMoved `
                $(if ($aSurv -is [string] -or $aLane -is [string] -or $aElided -is [string]) { $null }
                else { [decimal]$aSurv + [decimal]$aLane + [decimal]$aElided }) `
                "eq" @($aMoved, $aSurv, $aLane, $aElided) `
                "direct_test.rs:1535-1538's closure, updated for the skip path: a skipped row is compacted as a survivor without reaching the probe."))

    $closures.Add((New-Closure "probe_divergences_zero" `
                "probe_divergences == 0  [ON]" $aDiv 0 "eq" @($aDiv) `
                "A filter whose own instrument cannot go RED is systemic: prove it fires by hand-corrupting one lens element in a unit test before trusting this zero."))

    $closures.Add((New-Closure "entries_get_misses_zero_base" `
                "entries_get_misses == 0  [OFF]" $bMiss 0 "eq" @($bMiss) `
                "Design 6.2: entries_get_misses > 0 means the bijection has a hole, means the scan's self-heal was load-bearing, means the identity leg WILL fail cumulatively on smc_scan_keys. One predicate, not two."))

    $closures.Add((New-Closure "entries_get_misses_zero_arm" `
                "entries_get_misses == 0  [ON]" $aMiss 0 "eq" @($aMiss) `
                "Same predicate on the ON arm."))

    $closures.Add((New-Closure "retire_unreachable_base" `
                "retire_calls == retire_calls_effective  [OFF]" $bRet $bRetE "eq" @($bRet, $bRetE) `
                "Design 3.3 / 6.1(c): unequal means a live entries row named a retired BlockId -- a state run.rs:1569 already .expect()s away in RELEASE on a hotter path. STOP and root-cause; it is a pre-existing defect, not a tuning input."))

    $closures.Add((New-Closure "retire_unreachable_arm" `
                "retire_calls == retire_calls_effective  [ON]" $aRet $aRetE "eq" @($aRet, $aRetE) `
                "Same reading on the ON arm."))

    # REPORTED, never a bar: its own predictor is the OFF-arm census and the two
    # cannot disagree once the accept equality holds.
    $elisionRate = $null
    if ($aElided -isnot [string] -and $aScan -isnot [string] -and [decimal]$aScan -ne 0) {
        $elisionRate = [double]$aElided / [double]$aScan
    }
    $survivingRate = $null
    if ($bSurv -isnot [string] -and $bScan -isnot [string] -and [decimal]$bScan -ne 0) {
        $survivingRate = [double]$bSurv / [double]$bScan
    }

    [pscustomobject]@{
        available            = ($null -ne $BaseUnits -and $null -ne $ArmUnits)
        note                 = "probes_elided / entries_get_calls / probe_divergences are #[cfg(feature = `"smc-census`")] fields (design 6.3) and entries_get_calls does not exist in the tree yet. On a plain release build every closure reads not_evaluated, which is not a failure."
        closures             = @($closures)
        pass_count           = @($closures | Where-Object { $_.result -eq "pass" }).Count
        fail_count           = @($closures | Where-Object { $_.result -eq "FAIL" }).Count
        not_evaluated_count  = @($closures | Where-Object { $_.result -eq "not_evaluated" }).Count
        probes_elided_rate   = $elisionRate
        keys_surviving_rate_base = $survivingRate
        rate_note            = "probes_elided / keys_scanned is the magnitude that decides whether the slice was worth building. It is a REPORTED quantity, not a bar (design 6.5 item 3). keys_surviving(OFF) / keys_scanned(OFF) is S, section 5.2's direct predictor of the prize."
        base_units           = $BaseUnits
        arm_units            = $ArmUnits
    }
}

# ---------------------------------------------------------------------------
# One leg
# ---------------------------------------------------------------------------

function Invoke-Leg($Row, [string]$Arm, [string]$Label, [string]$RowDir) {
    $exe = if ($Arm -eq "arm") { $ArmExecutable } else { $BaseExecutable }
    $source = Join-Path $bench $Row.folder
    $scratch = Join-Path $RowDir "work-$Label"
    if (Test-Path -LiteralPath $scratch) { Remove-Item -Recurse -Force -LiteralPath $scratch }
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    $robo = Start-Process -FilePath robocopy.exe -ArgumentList @(
        $source, $scratch, "/MIR", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:2", "/W:1"
    ) -NoNewWindow -Wait -PassThru
    if ($robo.ExitCode -ge 8) { throw "robocopy failed ($($robo.ExitCode)) for $Label" }

    # Quake APPENDS to QCONSOLE.LOG and the oracle is its last line, so a stale
    # copy mirrored out of the source tree would be read as this run's result.
    $quakeLog = Join-Path $scratch "QUAKE\ID1\QCONSOLE.LOG"
    if (Test-Path -LiteralPath $quakeLog) { Remove-Item -LiteralPath $quakeLog -Force }
    # Same hazard for DUKEMARK's redirected report.
    if ($Row.dukemarkFile) {
        $stale = Join-Path $scratch $Row.dukemarkFile
        if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force }
    }

    $json = Join-Path $RowDir "$Label.json"
    $ppm = Join-Path $RowDir "$Label.ppm"
    $outLog = Join-Path $RowDir "$Label.out"
    $errLog = Join-Path $RowDir "$Label.err"

    $arguments = @()
    $arguments += $Row.arguments
    $arguments += @("--hdd-folder", $scratch)
    $arguments += @("--cycles", $Row.cycles)
    $arguments += @("--profile-json", $json)
    if ($Row.resultPpm) { $arguments += @("--result-ppm", $ppm) }
    if ($Row.cdImage) { $arguments += @("--cd-image", (Join-Path $bench $Row.cdImage)) }
    $arguments += $Row.injection

    Set-LadderEnvironment
    $cpuBefore = Get-HostCpuPercent
    $self = Get-Process -Id $PID
    $parentMask = $self.ProcessorAffinity.ToInt64()
    $mask = [int64]1 -shl $ProcessorIndex
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $self.ProcessorAffinity = [IntPtr]$mask
        $self.Refresh()
        $proc = Start-Process -FilePath $exe -ArgumentList $arguments -NoNewWindow -PassThru `
            -RedirectStandardOutput $outLog -RedirectStandardError $errLog
        $null = $proc.Handle
        $proc.Refresh()
        if ($proc.ProcessorAffinity.ToInt64() -ne $mask) { throw "child affinity not inherited" }
    }
    finally {
        $self.ProcessorAffinity = [IntPtr]$parentMask
        $self.Refresh()
    }
    $proc.WaitForExit()
    $watch.Stop()
    $cpuAfter = Get-HostCpuPercent
    if ($proc.ExitCode -ne 0) { throw "$Label exited $($proc.ExitCode)" }

    # Read the guest-side oracles OUT OF THE WORKING COPY before it is deleted.
    $qconsole = $null
    if ($Row.qconsole -and (Test-Path -LiteralPath $quakeLog)) {
        $lines = @(Get-Content -LiteralPath $quakeLog | Where-Object { $_ -match "\d+\s+frames" })
        if ($lines.Count -gt 0) { $qconsole = $lines[-1].Trim() }
    }
    $dukemark = $null
    if ($Row.dukemarkFile) {
        $resultPath = Join-Path $scratch $Row.dukemarkFile
        if (Test-Path -LiteralPath $resultPath) {
            $dukemark = ((Get-Content -LiteralPath $resultPath -Raw) -replace "`r", "").Trim()
        }
    }

    Remove-Item -Recurse -Force -LiteralPath $scratch -ErrorAction SilentlyContinue

    $report = Get-Content -LiteralPath $json -Raw | ConvertFrom-Json
    $hash = $null
    if ($Row.resultPpm -and (Test-Path -LiteralPath $ppm)) {
        $hash = (Get-FileHash -LiteralPath $ppm -Algorithm SHA256).Hash.ToLowerInvariant()
    }

    $identity = [ordered]@{}
    foreach ($field in $identityFields) { $identity[$field] = Get-Nested $report $field }
    $occupancy = [ordered]@{}
    foreach ($field in $occupancyFields) { $occupancy[$field] = Get-Nested $report $field }

    [pscustomobject]@{
        row          = $Row.name
        label        = $Label
        arm          = $Arm
        exe          = $exe
        cpu_before   = $cpuBefore
        cpu_after    = $cpuAfter
        flagged      = ($cpuBefore -gt 8 -or $cpuAfter -gt 8)
        contaminated = ($cpuBefore -gt 25 -or $cpuAfter -gt 25)
        wall_s       = [double](Get-Field $report "wall_seconds")
        stopwatch_s  = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s      = Get-Field $report "guest_seconds"
        rt           = Get-Field $report "real_time_factor"
        frame_sha256 = $hash
        qconsole     = $qconsole
        dukemark     = $dukemark
        gametics     = Get-Nested $report "timedemo.gametics"
        realtics     = Get-Nested $report "timedemo.realtics"
        identity     = [pscustomobject]$identity
        occupancy    = [pscustomobject]$occupancy
        census       = Get-CensusUnits $report
    }
}

function Invoke-LegUntilClean($Row, [string]$Arm, [string]$Label, [string]$RowDir) {
    $discards = 0
    while ($true) {
        $leg = Invoke-Leg $Row $Arm $Label $RowDir
        if (-not $leg.contaminated) { return @{ leg = $leg; discards = $discards } }
        $discards++
        $stamp = Get-Date -Format "HHmmss"
        Write-Host ("CONTAMINATED {0}/{1} cpu_before={2} cpu_after={3} (re-run bar 25); discard #{4}" -f `
                $Row.name, $Label, $leg.cpu_before, $leg.cpu_after, $discards)
        $leg | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath (Join-Path $RowDir "$Label-discard-$stamp.json")
        Start-Sleep -Seconds 2
    }
}

# ---------------------------------------------------------------------------
# One row
# ---------------------------------------------------------------------------

function Invoke-Row($Row, [int]$Pairs) {
    $rowDir = Join-Path $OutDir $Row.name
    New-Item -ItemType Directory -Force -Path $rowDir | Out-Null
    $isPrimary = ($Row.role -eq "primary")

    $legs = [Collections.Generic.List[object]]::new()
    $discardTotal = 0

    Write-Host ""
    Write-Host ("=== ROW {0} [{1}] {2}" -f $Row.name, $Row.role.ToUpperInvariant(), $Row.why)
    Write-Host "IDENTITY pair (BASE then ARM)..."
    $idBaseRun = Invoke-LegUntilClean $Row "base" "identity-base" $rowDir
    $idArmRun = Invoke-LegUntilClean $Row "arm" "identity-arm" $rowDir
    $discardTotal += $idBaseRun.discards + $idArmRun.discards
    $idBase = $idBaseRun.leg
    $idArm = $idArmRun.leg
    $legs.Add($idBase)
    $legs.Add($idArm)

    $identityNotes = [Collections.Generic.List[string]]::new()
    $warnings = [Collections.Generic.List[string]]::new()
    $scanKeysMoved = $false

    # The framebuffer, where the row emits one. Cross-arm BIT-IDENTITY, which is
    # strictly stronger than the scoreboard's bands and is the right check for a
    # change that must not move guest state at all.
    if ($Row.resultPpm) {
        if ([string]::IsNullOrWhiteSpace($idBase.frame_sha256)) {
            $identityNotes.Add("BASE wrote no framebuffer PPM")
        }
        elseif ($idArm.frame_sha256 -ne $idBase.frame_sha256) {
            $identityNotes.Add("frame hash BASE=$($idBase.frame_sha256) ARM=$($idArm.frame_sha256)")
        }
    }

    foreach ($field in $identityFields) {
        $b = Get-Field $idBase.identity $field
        $a = Get-Field $idArm.identity $field
        if ("$b" -ne "$a") {
            $identityNotes.Add("$field BASE=$b ARM=$a")
            if ($field -eq $scanKeysField) { $scanKeysMoved = $true }
        }
    }

    # doom: gametics is the pin that gates; realtics matches across arms but its
    # ABSOLUTE value is session-local and only warned on.
    if ($Row.timedemo) {
        if ($null -eq $idBase.gametics) {
            $identityNotes.Add("BASE reported no timedemo block")
        }
        else {
            if ([int]$idBase.gametics -ne $doomGametics) {
                $identityNotes.Add("gametics BASE=$($idBase.gametics) != PIN $doomGametics -- the demo itself changed")
            }
            if ("$($idArm.gametics)" -ne "$($idBase.gametics)") {
                $identityNotes.Add("gametics BASE=$($idBase.gametics) ARM=$($idArm.gametics)")
            }
            if ("$($idArm.realtics)" -ne "$($idBase.realtics)") {
                $identityNotes.Add("realtics BASE=$($idBase.realtics) ARM=$($idArm.realtics)")
            }
            if ($null -ne $Row.realticsMinimum -and $null -ne $idBase.realtics) {
                $rt = [int]$idBase.realtics
                if ($rt -lt $Row.realticsMinimum -or $rt -gt $Row.realticsMaximum) {
                    $warnings.Add(("realtics $rt is outside the scoreboard band " +
                            "[$($Row.realticsMinimum), $($Row.realticsMaximum)]. NOT a row failure: " +
                            "realtics is SESSION-LOCAL (the same commit has produced 813 and 769 hours " +
                            "apart). Cross-arm equality is the gate; this band is a sanity note."))
                }
            }
        }
    }

    if ($Row.qconsole) {
        if ([string]::IsNullOrWhiteSpace($idBase.qconsole)) {
            $identityNotes.Add("BASE wrote no QCONSOLE result line")
        }
        elseif ($idArm.qconsole -ne $idBase.qconsole) {
            $identityNotes.Add("QCONSOLE BASE='$($idBase.qconsole)' ARM='$($idArm.qconsole)'")
        }
    }

    if ($Row.dukemarkFile) {
        if ([string]::IsNullOrWhiteSpace($idBase.dukemark)) {
            $identityNotes.Add("BASE wrote no $($Row.dukemarkFile): the redirection or the guest-driven exit failed")
        }
        if ($idArm.dukemark -ne $idBase.dukemark) {
            $identityNotes.Add("DUKEMARK report differs between arms")
        }
    }

    $occupancyNotes = [Collections.Generic.List[string]]::new()
    foreach ($field in $occupancyFields) {
        $b = Get-Field $idBase.occupancy $field
        $a = Get-Field $idArm.occupancy $field
        if ("$b" -ne "$a") { $occupancyNotes.Add("$field BASE=$b ARM=$a") }
    }

    # The mechanism reading, from the identity legs. Never aborts.
    $census = Test-CensusClosures $idBase.census $idArm.census

    $identityOk = ($identityNotes.Count -eq 0)
    Write-Host ("IDENTITY {0}: BASE wall={1:N3}s ARM wall={2:N3}s" -f $Row.name, $idBase.wall_s, $idArm.wall_s)
    foreach ($warning in $warnings) { Write-Host "WARNING ($($Row.name)): $warning" }
    if ($occupancyNotes.Count -gt 0) {
        Write-Host "OCCUPANCY (not a STOP if the identity fields hold):"
        $occupancyNotes | ForEach-Object { Write-Host "  $_" }
    }

    Write-Host ("MECHANISM ({0}): {1} pass, {2} FAIL, {3} not_evaluated" -f `
            $Row.name, $census.pass_count, $census.fail_count, $census.not_evaluated_count)
    foreach ($closure in $census.closures) {
        if ($closure.result -eq "FAIL") {
            Write-Host ("  FAIL {0}: {1}  (left={2} right={3})" -f `
                    $closure.closure, $closure.statement, $closure.left, $closure.right)
        }
    }
    if ($census.not_evaluated_count -gt 0 -and $census.pass_count -eq 0) {
        Write-Host "  (no smc_census block in the profile JSON -- these are plain release builds, which is expected)"
    }
    if ($null -ne $census.probes_elided_rate) {
        Write-Host ("  probes_elided / keys_scanned = {0:P2}   (REPORTED, not a bar)" -f $census.probes_elided_rate)
    }
    if ($null -ne $census.keys_surviving_rate_base) {
        Write-Host ("  S = keys_surviving(OFF) / keys_scanned(OFF) = {0:P2}" -f $census.keys_surviving_rate_base)
    }

    $stats = $null
    $pairRows = [Collections.Generic.List[object]]::new()
    $wallLabel = $null

    if (-not $identityOk) {
        Write-Host ""
        Write-Host "################################################################"
        Write-Host ("## IDENTITY STOP on row {0}" -f $Row.name)
        foreach ($note in $identityNotes) { Write-Host "##   $note" }
        Write-Host "##"
        Write-Host "## C1 is a PURE host-side data-structure change: the function's"
        Write-Host "## return value, its counters and its side effects are bit-identical"
        Write-Host "## by construction (design 3.5). COUNTER MOVEMENT IS A DEFECT, not a"
        Write-Host "## tolerance and not a tuning question."
        if ($scanKeysMoved) {
            Write-Host "##"
            Write-Host "## smc_scan_keys MOVED, and that has a SPECIFIC diagnosis (design 6.2):"
            Write-Host '##   Today an entries-MISS row takes `continue` WITHOUT being written'
            Write-Host "##   back as a survivor, so it falls in the drain range and is removed."
            Write-Host "##   The scan SELF-HEALS stale rows. Under the pre-filter a stale row"
            Write-Host "##   whose lens says 'no overlap' is skipped BEFORE the probe and is"
            Write-Host "##   compacted as a survivor, so it stays on the page and every later"
            Write-Host "##   scan of that page counts it -- the divergence is CUMULATIVE."
            Write-Host "##   smc_scan_keys moving  =>  entries_get_misses > 0"
            Write-Host "##                         =>  the bijection has a hole"
            Write-Host "##                         =>  the self-heal was load-bearing."
            Write-Host "##   STOP and re-derive design section 3.1's add/remove site table."
            Write-Host "##   Check entries_get_misses in this row's GATE.json census block."
        }
        Write-Host ("## NO WALL LADDER WILL RUN FOR {0}. Continuing with the other rows." -f $Row.name)
        Write-Host "################################################################"
        Write-Host ""
        $wallLabel = "STOPPED (identity)"
    }
    elseif ($IdentityOnly) {
        Write-Host "IDENTITY PASS ($($Row.name)); -IdentityOnly, no wall ladder"
        $wallLabel = "IDENTITY_ONLY"
    }
    else {
        Write-Host "IDENTITY PASS ($($Row.name))"
        Write-Host ("WALL ladder: {0} ABBA pairs on {1}" -f $Pairs, $Row.name)
        for ($p = 1; $p -le $Pairs; $p++) {
            $pairLegs = [Collections.Generic.List[object]]::new()
            foreach ($step in @(@("base", "base"), @("arm", "arm"), @("arm", "arm"), @("base", "base"))) {
                $arm, $role = $step
                $label = "p$p-$role-$([guid]::NewGuid().ToString('N').Substring(0, 6))"
                Write-Host "  pair $p / $role ..."
                $ran = Invoke-LegUntilClean $Row $arm $label $rowDir
                $discardTotal += $ran.discards
                $leg = $ran.leg
                $leg | Add-Member -NotePropertyName pair -NotePropertyValue $p
                $legs.Add($leg)
                $pairLegs.Add($leg)
                Write-Host ("    wall {0,9:N3}s  cpu {1}/{2}" -f $leg.wall_s, $leg.cpu_before, $leg.cpu_after)
            }
            $baseMin = (@($pairLegs | Where-Object { $_.arm -eq "base" } | ForEach-Object { $_.wall_s }) |
                    Measure-Object -Minimum).Minimum
            $armMin = (@($pairLegs | Where-Object { $_.arm -eq "arm" } | ForEach-Object { $_.wall_s }) |
                    Measure-Object -Minimum).Minimum
            $ratio = $baseMin / $armMin
            $pairRows.Add([pscustomobject]@{
                    pair     = $p
                    base_min = $baseMin
                    arm_min  = $armMin
                    ratio    = $ratio
                    arm_wins = ($armMin -lt $baseMin)
                })
            Write-Host ("  pair {0}: BASE min {1:N3} ARM min {2:N3} ratio {3:N4} {4}" -f `
                    $p, $baseMin, $armMin, $ratio, $(if ($armMin -lt $baseMin) { "ARM" } else { "BASE" }))
        }

        $ratios = [double[]]@($pairRows | ForEach-Object { $_.ratio })
        $baseWalls = [double[]]@($legs | Where-Object { $_.arm -eq "base" -and $_.label -notlike "identity-*" } |
                ForEach-Object { $_.wall_s })
        $armWalls = [double[]]@($legs | Where-Object { $_.arm -eq "arm" -and $_.label -notlike "identity-*" } |
                ForEach-Object { $_.wall_s })
        $stats = Measure-Ladder $ratios $baseWalls $armWalls
        $wallLabel = Get-WallLabel $stats $isPrimary

        Write-Host ""
        Write-Host ("{0}: sign {1}/{2}  min-wall {3:N4}  mean {4:N4}  geomean {5:N4}  median {6:N4}  lower95 {7:N4}" -f `
                $Row.name, $stats.sign_arm_wins, $stats.pairs, $stats.min_wall_ratio, `
                $stats.mean_ratio, $stats.geomean_ratio, $stats.median_ratio, $stats.lower95)
        if (-not $stats.estimators_agree) {
            Write-Host ("{0}: ESTIMATOR SPREAD {1:N4} > 0.005 -- the PROTOCOL agreement set disagrees. The ladder is contaminated: RE-RUN, do not pick an estimator." -f `
                    $Row.name, $stats.estimator_spread)
        }
        Write-Host ("{0}: {1}" -f $Row.name, $wallLabel)
        if ($wallLabel -like "WALL_REGRESSION*" -and -not $isPrimary) {
            Write-Host ("NOTE: {0} is a {1} row, not a gate row. A regression here is REVIEW, not STOP." -f `
                    $Row.name, $Row.role)
        }
    }

    $gate = [pscustomobject]@{
        row               = $Row.name
        role              = $Row.role
        mechanism_row     = $Row.mechanism
        why               = $Row.why
        fixture_folder    = $Row.folder
        invocation        = (@($Row.arguments) + @("--hdd-folder", "<fresh copy>", "--cycles", $Row.cycles) +
            $(if ($Row.resultPpm) { @("--result-ppm", "<path>") } else { @() }) +
            $(if ($Row.cdImage) { @("--cd-image", (Join-Path $bench $Row.cdImage)) } else { @() }) +
            @($Row.injection)) -join " "
        identity_pass     = $identityOk
        identity_notes    = @($identityNotes)
        identity_warnings = @($warnings)
        scan_keys_moved   = $scanKeysMoved
        occupancy_notes   = @($occupancyNotes)
        census            = $census
        identity_base     = $idBase
        identity_arm      = $idArm
        pairs_requested   = $Pairs
        pair_rows         = @($pairRows)
        estimators        = $stats
        wall_label        = $wallLabel
        gating            = $(if ($isPrimary) {
                "PRIMARY: WALL_REGRESSION here fails the ladder. Regression = sign < half the pairs OR mean_ratio < 0.995 (deadband)."
            }
            else {
                "NOT a gate row: identity is a per-row STOP, but a wall regression here is REVIEW only."
            })
        accept_note       = "The ACCEPT for C1 is MECHANISM-first (design 6.5 item 3), not the wall: identity exact, probe_divergences 0, all closures holding, and probes_elided(ON) + keys_surviving(ON) == keys_surviving(OFF). The wall is a no-regression check. A wall number that outruns its mechanism by an order of magnitude is not banked: section 5.2 predicts 2.0-3.0% on nascar, so a reading of 8% is a confound, not a win."
        pairing           = "index-matched-within-arm in pair order, declared before the run; pair ratio = pair BASE min wall / pair ARM min wall"
        discards          = $discardTotal
        base_exe          = $BaseExecutable
        base_sha256       = $baseActual
        arm_exe           = $ArmExecutable
        arm_sha256        = $armActual
    }
    $gate | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $rowDir "GATE.json")
    # -InputObject with an array always emits a JSON array, one element included.
    # Do NOT pipe $legs into ConvertTo-Json: the pipeline unrolls it and a
    # one-element ladder would serialize as a bare object. And do NOT add
    # -AsArray on top of an array: it wraps it a second time.
    ConvertTo-Json -InputObject ([object[]]$legs) -Depth 10 |
        Set-Content -LiteralPath (Join-Path $rowDir "legs.json")

    return $gate
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

try {
    Write-Host "C1 invalidation presence-filter GATE ladder"
    Write-Host "  BASE $BaseExecutable"
    Write-Host "       sha256 $baseActual  PIN OK"
    Write-Host "  ARM  $ArmExecutable"
    Write-Host "       sha256 $armActual  PIN OK"
    Write-Host "  OutDir $OutDir"
    Write-Host "  Pinned to logical processor $ProcessorIndex; contamination re-run bar cpu > 25"
    Write-Host "  ACCEPT is MECHANISM-first; the wall is a no-regression check (design 6.5)."
    Write-Host ""

    $plan = [Collections.Generic.List[object]]::new()
    $estimateSeconds = 0.0
    foreach ($row in $rowTable) {
        $pairs = if ($row.role -eq "primary") { $PrimaryPairs } else { $SecondaryPairs }
        $legCount = if ($IdentityOnly) { 2 } else { 2 + 4 * $pairs }
        $rowSeconds = $legCount * ($row.legSeconds + $row.copySeconds)
        $estimateSeconds += $rowSeconds
        $plan.Add([pscustomobject]@{
                row              = $row.name
                role             = $row.role
                mechanism_row    = $row.mechanism
                pairs            = $pairs
                legs             = $legCount
                leg_seconds_est  = $row.legSeconds
                copy_seconds_est = $row.copySeconds
                row_seconds_est  = $rowSeconds
            })
        Write-Host ("PLAN {0,-16} {1,-9} pairs {2}  legs {3,3}  ~{4,4} s/leg  ~{5}" -f `
                $row.name, $row.role, $pairs, $legCount, ($row.legSeconds + $row.copySeconds), `
                ([TimeSpan]::FromSeconds($rowSeconds).ToString("hh\:mm\:ss")))
    }
    Write-Host ("PLAN total {0} legs, expected runtime ~{1} (LOWER BOUND: discarded contaminated legs are not counted)" -f `
        (($plan | Measure-Object -Property legs -Sum).Sum),
        ([TimeSpan]::FromSeconds($estimateSeconds).ToString("hh\:mm\:ss")))
    Write-Host ""

    $started = Get-Date
    $gates = [Collections.Generic.List[object]]::new()
    foreach ($row in $rowTable) {
        $pairs = if ($row.role -eq "primary") { $PrimaryPairs } else { $SecondaryPairs }
        $gates.Add((Invoke-Row $row $pairs))
    }
    $finished = Get-Date

    $primary = @($gates | Where-Object { $_.role -eq "primary" })
    $stopped = @($gates | Where-Object { -not $_.identity_pass })
    $review = @($gates | Where-Object { $_.identity_pass -and $_.wall_label -like "WALL_REGRESSION*" -and $_.role -ne "primary" })
    $censusFailures = @($gates | Where-Object { $_.census.fail_count -gt 0 })
    $contaminated = @($gates | Where-Object { $null -ne $_.estimators -and -not $_.estimators.estimators_agree })

    # The measured floor, from whatever control rows this run carried. ONE row
    # here, against the design's four -- see the DEVIATION note in the header.
    $controls = @($gates | Where-Object { $_.role -eq "control" -and $null -ne $_.estimators })
    $floorF = $null
    if ($controls.Count -gt 0) {
        $floorF = ($controls | ForEach-Object { [math]::Abs(1.0 - $_.estimators.min_wall_ratio) } |
                Measure-Object -Maximum).Maximum
    }
    $floorRows = @()
    if ($null -ne $floorF) {
        $floorRows = @($gates | Where-Object { $_.mechanism_row -and $null -ne $_.estimators } | ForEach-Object {
                $delta = $_.estimators.min_wall_ratio - 1.0
                [pscustomobject]@{
                    row               = $_.row
                    min_wall_delta    = $delta
                    outside_floor     = ([math]::Abs($delta) -gt $floorF)
                    reading           = $(if ($delta -gt $floorF) { "WIN (beyond +F)" }
                        elseif ($delta -lt (-1.0 * $floorF)) { "REGRESSION (beyond -F)" }
                        else { "NEUTRAL (inside +/-F)" })
                }
            })
    }

    $verdict = if ($primary.Count -eq 0) { "NO_PRIMARY_ROW_RUN" }
    elseif (-not $primary[0].identity_pass) { "STOP (primary identity)" }
    elseif ($primary[0].wall_label -like "WALL_REGRESSION*") { "STOP (primary wall regression)" }
    elseif ($IdentityOnly) { "IDENTITY_ONLY" }
    else { $primary[0].wall_label }

    $summary = [pscustomobject]@{
        gate                    = "C1 invalidation presence filter (per-key coverage carried inline in the per-page scan vectors)"
        design                  = "dev_docs/specs/2026-08-27-c1-presence-filter-design.md sections 6.1-6.5"
        started_utc             = $started.ToUniversalTime().ToString("o")
        finished_utc            = $finished.ToUniversalTime().ToString("o")
        elapsed_seconds         = [math]::Round(($finished - $started).TotalSeconds, 1)
        base_exe                = $BaseExecutable
        base_sha256             = $baseActual
        arm_exe                 = $ArmExecutable
        arm_sha256              = $armActual
        knob                    = "none -- C1 ships default-ON and unconditional (design 2.4); the arms differ by BINARY ONLY"
        processor_index         = $ProcessorIndex
        duke_short              = [bool]$DukeShort
        identity_only           = [bool]$IdentityOnly
        accept_note             = "MECHANISM-first ACCEPT (design 6.5 item 3). The wall is a no-regression check, not the accept."
        estimator_note          = "sign / min_wall_ratio / mean_ratio / geomean_ratio / median_ratio / lower95 (one-sided 95% t-approximation on ln of the per-pair ratios). estimator_spread > 0.005 over {geomean, median, min-wall, lower95} means the ladder is contaminated: re-run, do not pick."
        merge_bar               = "PRIMARY: WALL_REAL = lower95 > 1.0 AND sign >= ceil(0.75n) AND min_wall_ratio >= 1.005. WALL_REGRESSION (STOP) = sign*2 < n OR mean_ratio < 0.995. Otherwise WALL_NEUTRAL. Secondary and control rows are advisory: identity is a per-row STOP, a wall regression is REVIEW."
        measured_floor_F_one_row = $floorF
        measured_floor_note     = "F is the largest absolute min-wall delta over the INERT CONTROL rows this run carried. Design 6.5 item 4 defines it over FOUR (quake-586, prince-486, gp2-586, tombraid-586); this ladder carries quake-586 alone, so F here is a ONE-ROW estimate of the floor and NOT the design's bar. Widen it with the other three on the same binary pair before quoting it."
        floor_readings          = $floorRows
        plan                    = @($plan)
        rows                    = @($gates | ForEach-Object {
                [pscustomobject]@{
                    row                 = $_.row
                    role                = $_.role
                    mechanism_row       = $_.mechanism_row
                    identity_pass       = $_.identity_pass
                    identity_notes      = $_.identity_notes
                    identity_warnings   = $_.identity_warnings
                    scan_keys_moved     = $_.scan_keys_moved
                    occupancy_notes     = $_.occupancy_notes
                    census_pass         = $_.census.pass_count
                    census_fail         = $_.census.fail_count
                    census_not_eval     = $_.census.not_evaluated_count
                    census_closures     = $_.census.closures
                    probes_elided_rate  = $_.census.probes_elided_rate
                    keys_surviving_rate_base = $_.census.keys_surviving_rate_base
                    wall_label          = $_.wall_label
                    estimators          = $_.estimators
                    discards            = $_.discards
                    invocation          = $_.invocation
                }
            })
        stopped_rows            = @($stopped | ForEach-Object { $_.row })
        review_rows             = @($review | ForEach-Object { $_.row })
        census_failure_rows     = @($censusFailures | ForEach-Object { $_.row })
        contaminated_rows       = @($contaminated | ForEach-Object { $_.row })
        verdict                 = $verdict
    }
    $summary | ConvertTo-Json -Depth 14 | Set-Content -LiteralPath (Join-Path $OutDir "SUMMARY.json")

    Write-Host ""
    Write-Host "================================ SUMMARY ================================"
    foreach ($gate in $gates) {
        if (-not $gate.identity_pass) {
            Write-Host ("{0,-16} {1,-9} STOPPED (identity): {2}" -f $gate.row, $gate.role, ($gate.identity_notes -join "; "))
        }
        elseif ($null -eq $gate.estimators) {
            Write-Host ("{0,-16} {1,-9} {2}" -f $gate.row, $gate.role, $gate.wall_label)
        }
        else {
            $s = $gate.estimators
            Write-Host ("{0,-16} {1,-9} sign {2}/{3}  min {4:N4}  mean {5:N4}  geo {6:N4}  med {7:N4}  lo95 {8:N4}  {9}" -f `
                    $gate.row, $gate.role, $s.sign_arm_wins, $s.pairs, $s.min_wall_ratio, `
                    $s.mean_ratio, $s.geomean_ratio, $s.median_ratio, $s.lower95, $gate.wall_label)
        }
        Write-Host ("{0,-16} {1,-9} mechanism: {2} pass / {3} FAIL / {4} not_evaluated" -f `
                "", "", $gate.census.pass_count, $gate.census.fail_count, $gate.census.not_evaluated_count)
    }
    if ($null -ne $floorF) {
        Write-Host ""
        Write-Host ("MEASURED FLOOR F (one control row, NOT the design's four-row bar): {0:P2}" -f $floorF)
        foreach ($reading in $floorRows) {
            Write-Host ("  {0,-16} min-wall delta {1,8:P2}  {2}" -f $reading.row, $reading.min_wall_delta, $reading.reading)
        }
    }
    if ($stopped.Count -gt 0) {
        Write-Host ""
        Write-Host ("STOPPED ROWS: {0}" -f (($stopped | ForEach-Object { $_.row }) -join ", "))
    }
    if ($review.Count -gt 0) {
        Write-Host ("REVIEW ROWS (regression on a non-gate row): {0}" -f (($review | ForEach-Object { $_.row }) -join ", "))
    }
    if ($censusFailures.Count -gt 0) {
        Write-Host ("MECHANISM CLOSURE FAILURES (the orchestrator adjudicates): {0}" -f `
            (($censusFailures | ForEach-Object { $_.row }) -join ", "))
    }
    if ($contaminated.Count -gt 0) {
        Write-Host ("ESTIMATOR DISAGREEMENT > 0.005 (re-run, do not pick): {0}" -f `
            (($contaminated | ForEach-Object { $_.row }) -join ", "))
    }
    Write-Host ("VERDICT: {0}" -f $verdict)
    Write-Host ("Artifacts: {0}" -f $OutDir)
    Write-Host "========================================================================="

    if ($verdict -like "STOP*") { throw "$verdict. See $OutDir\SUMMARY.json" }
    if ($primary.Count -eq 0) { throw "No primary row ran. See $OutDir\SUMMARY.json" }
}
finally {
    if (Test-Path -LiteralPath $lockPath) {
        $held = Get-Content -LiteralPath $lockPath -Raw
        if ($held -match "^$PID\b") { Remove-Item -LiteralPath $lockPath -Force }
    }
}
