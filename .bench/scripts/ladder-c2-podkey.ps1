# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
GATE ladder for PR #742 (C2: PodKeyBuildHasher on the four block-linking maps).

.DESCRIPTION
Two PINNED binaries, no knob. The arms differ by BINARY ONLY, because C2 ships as a
plain type change on four `BlockCache` fields and a runtime-selectable hasher is not
expressible without putting a branch on the hash path (design section 1.2).

    BASE = main b9d77e81   sha256 2126461db0da423133bfa12e2293473e4eb6bcdec654017689951538b61ca7b8
    ARM  =      7c20fd57   sha256 4ec6bdb5dfa232f2f7ddb6dff85bce9bdc66ffd057482a6e1e324124bd72ccc0

Both hashes are verified before ANY leg runs and a mismatch refuses the whole run.
That check is the enforcement of review MAJOR-4: the two commits are built
sequentially into ONE `target/` and copied out to distinct pinned paths, so the only
thing standing between "the copy step happened" and "the ladder measured one binary
twice" is this hash.

ROWS (design section 6.3 as adjudicated by review MAJOR-3):

  PRIMARY   wolf3d-586    n = 8 ABBA pairs   SipHash13 measured at 5.16% of wall here
  SECONDARY duke3d-586    n = 4 ABBA pairs   link volume, and the SMC regime
  SECONDARY tombraid-586  n = 4 ABBA pairs   the audit's second link-volume row
  CONTROL   quake-586     n = 4 ABBA pairs   the precedent row for the `entries` swap

Every row runs its own IDENTITY pair FIRST (one BASE leg, one ARM leg). This is a
pure host-side data-structure change, so an identity drift is a DEFECT and not a
tolerance (design section 6.1): section 5's iteration-order verdict would be wrong.
An identity failure STOPS THAT ROW LOUDLY and the script continues with the other
rows, recording STOPPED for it in the summary. wolf3d-586 additionally pins its BASE
frame hash to the value the D-elision-B ladder ran against; a BASE hash that differs
from the pin is a real finding about the tree, not a tolerance, and it stops the row.

Only the PRIMARY row's wall verdict can fail this script. duke3d-586 is SMC-fragile
(`sb16-dsp-merge-duke-regression`: small port-read deltas flip it +/-5%), so a duke
regression outside the noise floor is reported as REVIEW, never STOP. The same
applies to tombraid-586 and to the quake-586 control: their labels are advisory and
land in SUMMARY.json for a human to adjudicate. That resolves the control-row/gate
tension review MAJOR-3 asked to have settled BEFORE the ladder runs.

ESTIMATORS (review MAJOR-2: a mean plus a sign test is weaker than the board's own
practice; PROTOCOL reports four estimators and merges on their agreement). Per row,
over the PAIR-MATCHED per-pair ratios r_i = (pair BASE min wall) / (pair ARM min
wall), pairing declared before the run as index-matched-within-arm in pair order:

  sign        count of pairs with r_i > 1 (an exact tie counts as a BASE win, which
              is the strict reading of "sign >= 6/8" and "sign < 4/8"; with real
              wall floats a tie does not occur)
  min_ratio   min(all BASE walls) / min(all ARM walls)   -- the min-wall cross-check
  mean_ratio  arithmetic mean of the r_i
  geomean     exp(mean(ln r_i))
  median      median of the r_i
  lower95     one-sided 95% lower bound, t-approximation on ln(r_i):
              exp( mean(ln r) - t(0.95, n-1) * sd(ln r) / sqrt(n) )
              Student t rather than a bootstrap because n is 4 or 8 and a bootstrap
              of 4 points resamples the same four numbers; the t table is inlined
              below. sd = 0 (identical walls) degenerates to the geomean.

`mean_wall_ratio` (mean of BASE walls over mean of ARM walls) is ALSO recorded as a
cross-check against the model script's estimator, but the labels below read
`mean_ratio`, the mean of the per-pair ratios.

MERGE LABELS, primary row, declared in advance:

  WALL_REAL        lower95 > 1.0  AND  sign >= 6/8  AND  min_ratio >= 1.005
  WALL_REGRESSION  sign < 4/8  OR  mean_ratio < 0.995        -- STOP
  WALL_NEUTRAL     anything else

The 0.995 DEADBAND is deliberate and is the campaign trap it is named for: mean < 1.0
with no deadband is NOT a regression when sign and min-wall hold, because the rig's
noise floor is +/- 2% and free (`inert-controls-measure-the-noise-floor`). Regression
requires sign < 4/8 OR mean_ratio < 0.995, and nothing else. Secondary rows use the
same shape with the thresholds scaled to their n (REAL needs ceil(0.75n) sign wins,
regression needs sign*2 < n).

CONTAMINATION: the re-run bar is cpu_before/after > 25 (this host's idle sits 14-23%,
per `run-fixture-scoreboard.ps1`'s calibration note). A contaminated leg is DISCARDED
to its own json and re-run. Legs are pinned to one logical processor.

ARTIFACTS, all under the mandatory -OutDir:
  <row>/<label>.json      the emulator's own profile json, one per leg
  <row>/<label>.ppm/.out/.err
  <row>/legs.json         the distilled leg records for that row
  <row>/GATE.json         that row's identity result, pairs and estimators
  SUMMARY.json            one object, every row, the verdict
Everything written here is built as a plain hashtable / pscustomobject and piped
straight to ConvertTo-Json. Do NOT interpose Format-List or Format-Table: a prior
SUMMARY.json was ruined that way and shipped `ClassId2e4f51ef21dd47e99d3c952918aff9cd`
rows instead of data.

EXPECTED RUNTIME. Per-leg walls are taken from the newest scoreboard artifacts under
.bench/results/ (`scoreboard.json`, field `wall_seconds`), rounded up:
  wolf3d-586    66 s  (68.979 arm0-g2 / 65.017 arm1-g2, 2026-08-26; 65.288 / 64.568 on 2026-08-25)
  duke3d-586   205 s  (201.075 arm0-g5 / 249.732 arm1-g5, 2026-08-26; 202.357 / 197.472 on 2026-08-25
                       -- the 249.7 leg is the outlier this ladder's contamination bar exists to reject)
  duke3d-586-short 96 s (96.567 / 95.562, 2026-08-26)   -- with -DukeShort
  tombraid-586 128 s  (128.734 arm0-g3 / 118.528 arm1-g3, 2026-08-26; 129.932 / 124.994 on 2026-08-25)
  quake-586     21 s  (21.897, scoreboard-20260826-081213-armon-inline-verify; review NOTE 5)
plus a per-leg fixture copy charged from the tree sizes (wolf3d 2.3 MB, quake 18.3 MB,
tombraid 15.0 MB, duke3d 46.1 MB). The estimate is printed at start and is a LOWER
bound: discarded contaminated legs are not in it.

See dev_docs/specs/2026-08-27-c2-podkey-links-design.md section 6 and
dev_docs/specs/2026-08-27-c2-podkey-links-review.md MAJOR-2 through MAJOR-4.
Modelled on .bench/scripts/ladder-d-elision-b.ps1 (review MINOR-9: that script is
untracked, so this is a copy under its own name with its own lock path, not a
reference to it).
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` (the shape
# an outer PowerShell produces from `-Fixtures @('a','b')`) ran ONE row of a
# two-row sweep and EXITED 0. With positional binding off, the stray token is a
# binder error before one line of this script runs. The safe multi-row spelling
# is the COMMA string: `-Rows wolf3d-586,quake-586`. -Rows carries no
# ValidateSet because that fires on the comma string as ONE value;
# Resolve-RowSelection splits it and validates every name instead.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$BaseExecutable,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$ArmExecutable,
    [Parameter(Mandatory, ParameterSetName = "Run")][string]$OutDir,
    [string[]]$Rows = @("wolf3d-586", "duke3d-586", "tombraid-586", "quake-586"),
    # Resolve -Rows, print the selection, exit 0. Exists so the self-test's
    # green control can prove a well-formed invocation binds without running a
    # leg. Run-set arguments still have to be supplied; dummies are fine.
    [switch]$BindCheck,
    [Parameter(Mandatory, ParameterSetName = "SelfTest")][switch]$SelfTest,
    [int]$PrimaryPairs = 8,
    [int]$SecondaryPairs = 4,
    [int]$ProcessorIndex = 8,
    [switch]$DukeShort,
    [switch]$IdentityOnly,
    [string]$BaseSha256 = "2126461db0da423133bfa12e2293473e4eb6bcdec654017689951538b61ca7b8",
    [string]$ArmSha256 = "4ec6bdb5dfa232f2f7ddb6dff85bce9bdc66ffd057482a6e1e324124bd72ccc0"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))
$bench = Join-Path $repo ".bench"
$lockPath = Join-Path $bench "locks\c2-podkey-ladder.lock"

# The two pins. A binary that does not hash to one of these is not the binary the
# VERDICT will name, and the run refuses rather than measuring an unknown build.
$baseSha = $BaseSha256.ToLowerInvariant()
$armSha = $ArmSha256.ToLowerInvariant()

# wolf3d-586's BASE frame hash, carried over from the D-elision-B ladder. Same
# fixture, same persona, same budget, same injection, same env.
$wolf3dFrameHash = "e33418bbd34c13ad9c99e23c3d1cddb68df08beecec8676378557a4cc102f963"

# The PUBLIC row names, the list the removed ValidateSet carried. Keep it equal
# to Get-RowTable's names; duke3d-586-short is selected via -DukeShort, never
# by name, exactly as before.
$knownRows = @("wolf3d-586", "duke3d-586", "tombraid-586", "quake-586")

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
    $split = Resolve-RowSelection @("wolf3d-586,quake-586") $knownRows
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "wolf3d-586" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "quake-586" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" wolf3d-586 , quake-586") $knownRows
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("wolf3d-586,no-such-row") $knownRows } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("wolf3d-586,") $knownRows } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("wolf3d-586", "wolf3d-586") $knownRows } `
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
                "-OutDir", "self-test-dummy",
                "-Rows", "wolf3d-586", "quake-586", "-BindCheck")
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
        if ($failureText -notmatch 'quake-586') {
            throw ("self-test failed: the mangled -Rows child failed, but not on the " +
                "stray token. stderr: $failureText")
        }

        # GREEN control: the comma spelling of the same selection must bind and
        # resolve, or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-BaseExecutable", "self-test-dummy", "-ArmExecutable", "self-test-dummy",
            "-OutDir", "self-test-dummy",
            "-Rows", "wolf3d-586,quake-586", "-BindCheck")
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
        if ($listing -notmatch 'quake-586') {
            throw "self-test failed: the -BindCheck control did not echo the selection"
        }
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-c2-podkey self-test passed"
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

# ---------------------------------------------------------------------------
# The rows. Fixture folder, arguments, cycles, injection and cd image are copied
# VERBATIM from scripts/run-fixture-scoreboard.ps1's Get-FixtureTable (wolf3d-586
# :1089-1100, duke3d-586 :1115-1131, duke3d-586-short :1132-1153, tombraid-586
# :1195-1221, quake-586 :1052-1061). Do not paraphrase them: the recorded
# invariants were measured under exactly these arguments.
# ---------------------------------------------------------------------------

function Get-RowTable {
    $duke = if ($DukeShort) {
        [pscustomobject]@{
            name = "duke3d-586-short"; folder = "duke3d_short_c"; role = "secondary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "33200000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = "DUKEMARK.TXT"
            expectedFrameHash = $null
            legSeconds = 96; copySeconds = 10
            why = "SECONDARY (cheap substitute for duke3d-586; PROTOCOL sanctions it for laddering, re-run the LONG row before merge)"
        }
    }
    else {
        [pscustomobject]@{
            name = "duke3d-586"; folder = "duke3d_c"; role = "secondary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "79680000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $false; dukemarkFile = "DUKEMARK.TXT"
            expectedFrameHash = $null
            legSeconds = 205; copySeconds = 10
            why = "SECONDARY (link volume + the SMC regime; SMC-FRAGILE, a regression here is REVIEW not STOP)"
        }
    }

    @(
        [pscustomobject]@{
            name = "wolf3d-586"; folder = "wolf3d_c"; role = "primary"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "12000000000"
            # One Enter at the signon. Without it every wolf3d number measures an
            # out-of-memory crash loop (HARNESS.md, 2026-08-08 correction).
            injection = @("--inject-keys", "2000000000:`n")
            resultPpm = $true; cdImage = $null
            qconsole = $false; dukemarkFile = $null
            expectedFrameHash = $wolf3dFrameHash
            legSeconds = 66; copySeconds = 3
            why = "PRIMARY (SipHash13 5.16% of wall; jit_direct_linked_transfers 3.63e9)"
        }
        $duke
        [pscustomobject]@{
            name = "tombraid-586"; folder = "tombraid_c"; role = "secondary"
            # NO --video: the scoreboard row omits it and the invariants were
            # measured that way.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = "28000000000"
            injection = @(); resultPpm = $true
            # The disc is REQUIRED and is mounted read-only from the shared tree,
            # never copied per run.
            cdImage = "tombraid_cd\tombeng.cue"
            qconsole = $false; dukemarkFile = $null
            # The end-of-budget frame lands MID-DEMO and the harness does NOT grade
            # its hash (it records it as final_frame_sha256 and grades bands
            # instead). So there is no pin to carry here: the frame hash is compared
            # CROSS-ARM only, which is the right check for a change that must not
            # move guest state at all.
            expectedFrameHash = $null
            legSeconds = 128; copySeconds = 5
            why = "SECONDARY (the audit's second link-volume row; graded cross-arm, its end frame carries no pin)"
        }
        [pscustomobject]@{
            name = "quake-586"; folder = "quake_c"; role = "control"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = "6200000000"
            injection = @(); resultPpm = $false; cdImage = $null
            qconsole = $true; dukemarkFile = $null
            expectedFrameHash = $null
            legSeconds = 21; copySeconds = 5
            why = 'CONTROL (the precedent row: the 3.1% reading that bought the entries map its POD hasher came from quake)'
        }
    ) | Where-Object { $Rows -contains $_.name -or ($DukeShort -and $_.name -eq "duke3d-586-short" -and $Rows -contains "duke3d-586") }
}

# ---------------------------------------------------------------------------
# Identity fields. Design section 6.1's list, plus direct_stalls.smc_lane_trials
# (review NOTE 6: free to add, and it catches MAJOR-1's mechanism moving if the
# scope decision ever changes). Dotted paths, walked by Get-Nested.
# ---------------------------------------------------------------------------

$identityFields = @(
    "executed_cpu_core_clocks"
    "raw_bus_clocks"
    "scaled_bus_clocks"
    "master_ticks"
    "perf.instructions"
    "perf.jit_direct_insns"
    "perf.jit_direct_blocks_installed"
    "perf.jit_direct_linked_transfers"
    "perf.jit_direct_links_created"
    "perf.jit_direct_links_cleared"
    "perf.jit_direct_unresolved_dynamic_miss_or_unbound"
    "perf.code_invalidations"
    "direct_stalls.far_link_refused_cs"
    "direct_stalls.smc_lane_trials"
    "stop.kind"
    "stop.requested"
)

# jit_direct_entries is watched but is NOT an identity STOP on its own: the model
# ladder records it as occupancy. Kept separate for the same reason.
$occupancyFields = @("perf.jit_direct_entries")

# ---------------------------------------------------------------------------
# Binary pins. Before anything else.
# ---------------------------------------------------------------------------

if (-not (Test-Path -LiteralPath $BaseExecutable)) { throw "Missing BASE executable: $BaseExecutable" }
if (-not (Test-Path -LiteralPath $ArmExecutable)) { throw "Missing ARM executable: $ArmExecutable" }

$baseActual = (Get-FileHash -LiteralPath $BaseExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
$armActual = (Get-FileHash -LiteralPath $ArmExecutable -Algorithm SHA256).Hash.ToLowerInvariant()

if ($baseActual -ne $baseSha) {
    throw ("BASE binary sha256 mismatch. Expected $baseSha (main b9d77e81), got $baseActual " +
        "for $BaseExecutable. REFUSING TO RUN: the copy-out step in the build procedure " +
        "did not produce the pinned binary.")
}
if ($armActual -ne $armSha) {
    throw ("ARM binary sha256 mismatch. Expected $armSha (7c20fd57), got $armActual " +
        "for $ArmExecutable. REFUSING TO RUN: the copy-out step in the build procedure " +
        "did not produce the pinned binary.")
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
"$PID c2-podkey-ladder" | Set-Content -LiteralPath $lockPath -NoNewline

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

function Set-LadderEnvironment {
    # The campaign's shipped default arm, set EXPLICITLY. Never unset a knob to
    # turn it off: an empty IZARRAVM_JIT reads as ON (env-null = empty = OFF trap
    # cuts both ways), and both `=0` spellings below are members of their knob's
    # own spelling table, which panics on a typo rather than defaulting silently.
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
    # armed, and several of them only do work when the JIT is active -- i.e. they
    # tax exactly the runs this is trying to time.
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
    [pscustomobject]@{
        pairs           = $n
        sign_arm_wins   = $sign
        mean_ratio      = $mean
        geomean_ratio   = $geomean
        median_ratio    = $median
        min_wall_ratio  = $baseMin / $armMin
        lower95         = $lower95
        min_wall_base   = $baseMin
        min_wall_arm    = $armMin
        mean_wall_base  = $baseMean
        mean_wall_arm   = $armMean
        mean_wall_ratio = $baseMean / $armMean
    }
}

# The labels, declared in advance. See the header for the deadband's reasoning.
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
        identity     = [pscustomobject]$identity
        occupancy    = [pscustomobject]$occupancy
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
        $leg | ConvertTo-Json -Depth 6 |
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

    if ($Row.expectedFrameHash) {
        if ($idBase.frame_sha256 -ne $Row.expectedFrameHash) {
            $identityNotes.Add("BASE frame hash $($idBase.frame_sha256) != PIN $($Row.expectedFrameHash) -- this is a finding about the TREE, not a tolerance")
        }
    }
    if ($Row.resultPpm -and $idArm.frame_sha256 -ne $idBase.frame_sha256) {
        $identityNotes.Add("ARM frame hash $($idArm.frame_sha256) != BASE $($idBase.frame_sha256)")
    }
    foreach ($field in $identityFields) {
        $b = Get-Field $idBase.identity $field
        $a = Get-Field $idArm.identity $field
        if ("$b" -ne "$a") { $identityNotes.Add("$field BASE=$b ARM=$a") }
    }
    if ($Row.qconsole -and $idArm.qconsole -ne $idBase.qconsole) {
        $identityNotes.Add("QCONSOLE BASE='$($idBase.qconsole)' ARM='$($idArm.qconsole)'")
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

    $identityOk = ($identityNotes.Count -eq 0)
    Write-Host ("IDENTITY {0}: BASE wall={1:N3}s ARM wall={2:N3}s" -f $Row.name, $idBase.wall_s, $idArm.wall_s)
    if ($occupancyNotes.Count -gt 0) {
        Write-Host "OCCUPANCY (not a STOP if the identity fields hold):"
        $occupancyNotes | ForEach-Object { Write-Host "  $_" }
    }

    $stats = $null
    $pairRows = [Collections.Generic.List[object]]::new()
    $wallLabel = $null

    if (-not $identityOk) {
        Write-Host ""
        Write-Host "################################################################"
        Write-Host ("## IDENTITY STOP on row {0}" -f $Row.name)
        foreach ($note in $identityNotes) { Write-Host "##   $note" }
        Write-Host "## This is a PURE host-side data-structure change. Anything moving"
        Write-Host "## here is a DEFECT, not a tolerance: the design's iteration-order"
        Write-Host "## verdict (section 5) is wrong and the slice stops."
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
        Write-Host ("{0}: {1}" -f $Row.name, $wallLabel)
        if ($wallLabel -like "WALL_REGRESSION*" -and -not $isPrimary) {
            Write-Host ("NOTE: {0} is a {1} row, not a gate row. A regression here is REVIEW, not STOP." -f `
                    $Row.name, $Row.role)
        }
    }

    $gate = [pscustomobject]@{
        row                = $Row.name
        role               = $Row.role
        why                = $Row.why
        fixture_folder     = $Row.folder
        invocation         = (@($Row.arguments) + @("--hdd-folder", "<fresh copy>", "--cycles", $Row.cycles) +
            $(if ($Row.resultPpm) { @("--result-ppm", "<path>") } else { @() }) +
            $(if ($Row.cdImage) { @("--cd-image", (Join-Path $bench $Row.cdImage)) } else { @() }) +
            @($Row.injection)) -join " "
        identity_pass      = $identityOk
        identity_notes     = @($identityNotes)
        occupancy_notes    = @($occupancyNotes)
        expected_frame_pin = $Row.expectedFrameHash
        identity_base      = $idBase
        identity_arm       = $idArm
        pairs_requested    = $Pairs
        pair_rows          = @($pairRows)
        estimators         = $stats
        wall_label         = $wallLabel
        gating             = $(if ($isPrimary) {
                "PRIMARY: WALL_REGRESSION here fails the ladder. Regression = sign < half the pairs OR mean_ratio < 0.995 (deadband)."
            }
            else {
                "NOT a gate row: identity is a per-row STOP, but a wall regression here is REVIEW only."
            })
        pairing            = "index-matched-within-arm in pair order, declared before the run; pair ratio = pair BASE min wall / pair ARM min wall"
        discards           = $discardTotal
        base_exe           = $BaseExecutable
        base_sha256        = $baseActual
        arm_exe            = $ArmExecutable
        arm_sha256         = $armActual
    }
    $gate | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $rowDir "GATE.json")
    # -InputObject with an array always emits a JSON array, one element included.
    # Do NOT pipe $legs into ConvertTo-Json: the pipeline unrolls it and a
    # one-element ladder would serialize as a bare object. And do NOT add
    # -AsArray on top of an array: it wraps it a second time.
    ConvertTo-Json -InputObject ([object[]]$legs) -Depth 8 |
        Set-Content -LiteralPath (Join-Path $rowDir "legs.json")

    return $gate
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

try {
    Write-Host "C2 PodKey link-map GATE ladder (PR #742)"
    Write-Host "  BASE $BaseExecutable"
    Write-Host "       sha256 $baseActual  (main b9d77e81) PIN OK"
    Write-Host "  ARM  $ArmExecutable"
    Write-Host "       sha256 $armActual  (7c20fd57) PIN OK"
    Write-Host "  OutDir $OutDir"
    Write-Host "  Pinned to logical processor $ProcessorIndex; contamination re-run bar cpu > 25"
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

    $verdict = if ($primary.Count -eq 0) { "NO_PRIMARY_ROW_RUN" }
    elseif (-not $primary[0].identity_pass) { "STOP (primary identity)" }
    elseif ($primary[0].wall_label -like "WALL_REGRESSION*") { "STOP (primary wall regression)" }
    elseif ($IdentityOnly) { "IDENTITY_ONLY" }
    else { $primary[0].wall_label }

    $summary = [pscustomobject]@{
        gate            = "C2 PodKeyBuildHasher on the four block-linking maps"
        pr              = 742
        started_utc     = $started.ToUniversalTime().ToString("o")
        finished_utc    = $finished.ToUniversalTime().ToString("o")
        elapsed_seconds = [math]::Round(($finished - $started).TotalSeconds, 1)
        base_exe        = $BaseExecutable
        base_commit     = "b9d77e81"
        base_sha256     = $baseActual
        arm_exe         = $ArmExecutable
        arm_commit      = "7c20fd57"
        arm_sha256      = $armActual
        knob            = "none -- the arms differ by BINARY ONLY (design section 1.2)"
        processor_index = $ProcessorIndex
        duke_short      = [bool]$DukeShort
        identity_only   = [bool]$IdentityOnly
        estimator_note  = "sign / min_wall_ratio / mean_ratio / geomean_ratio / median_ratio / lower95 (one-sided 95% t-approximation on ln of the per-pair ratios)"
        merge_bar       = "PRIMARY: WALL_REAL = lower95 > 1.0 AND sign >= ceil(0.75n) AND min_wall_ratio >= 1.005. WALL_REGRESSION (STOP) = sign*2 < n OR mean_ratio < 0.995. Otherwise WALL_NEUTRAL. Secondary and control rows are advisory: identity is a per-row STOP, a wall regression is REVIEW."
        plan            = @($plan)
        rows            = @($gates | ForEach-Object {
                [pscustomobject]@{
                    row             = $_.row
                    role            = $_.role
                    identity_pass   = $_.identity_pass
                    identity_notes  = $_.identity_notes
                    occupancy_notes = $_.occupancy_notes
                    wall_label      = $_.wall_label
                    estimators      = $_.estimators
                    discards        = $_.discards
                    invocation      = $_.invocation
                }
            })
        stopped_rows    = @($stopped | ForEach-Object { $_.row })
        review_rows     = @($review | ForEach-Object { $_.row })
        verdict         = $verdict
    }
    $summary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $OutDir "SUMMARY.json")

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
    }
    if ($stopped.Count -gt 0) {
        Write-Host ""
        Write-Host ("STOPPED ROWS: {0}" -f (($stopped | ForEach-Object { $_.row }) -join ", "))
    }
    if ($review.Count -gt 0) {
        Write-Host ("REVIEW ROWS (regression on a non-gate row): {0}" -f (($review | ForEach-Object { $_.row }) -join ", "))
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
