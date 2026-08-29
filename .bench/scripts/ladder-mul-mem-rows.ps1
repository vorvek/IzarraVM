# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The MUL-memory-row ladder: gp2-586 plus controls, both arms from ONE binary.

.DESCRIPTION
`run-fixture-scoreboard.ps1` REMOVES every IZARRAVM_* variable from the child
(see its Get-RowEnvironment), so it can only ever run a knob's DEFAULT arm. Arm
work therefore needs a direct invocation, and this is it.

WHAT IS BEING MEASURED. `0xF7 /4` MUL r/m32 in its MEMORY form had no lowering,
while the signed `/5` sibling has had one since the rejected-row campaign. The
2026-08-29 evening census measured that row at 13.1 M interpreted hits on
gp2-586 -- the second head of that fixture's rejected class, which is 101.4 M
exits and 65% of its 156.4 M unbound. A rejected-class hit is a block
TERMINATION, so what the admission buys is the EXTENSION past the multiply, not
the multiply. Design: `dev_docs/gp2-mul-mem-slice-design-2026-08-29.md`.

WHY ONE BINARY. Layout variance between two builds of this workspace has been
measured at 3.7% on this box -- larger than most levers. A two-binary comparison
cannot carry a claim this size, so the knob exists and both arms come out of the
same executable.

Arms, both reachable in one binary:
  IZARRAVM_MUL_MEM_ROWS=0  the OFF arm, main's refusal, the A/B base and the
                           shipped default
  IZARRAVM_MUL_MEM_ROWS=1  the ON arm, `DirectKind::MulMemAcc`

THE IDENTITY GATE, and why this slice predicts identity where a lowering
normally would not. The whole group-3 interpreter arm returns `clocks(2)` for
every sub-opcode and both operand forms, which IS the `DirectKind::raw_clocks`
default the emitted form charges; the memory read is declared through
`dword_reads`, so the bus charge matches too. gp2 is deterministic and the
budget is a fixed cycle count, so the two arms must execute the SAME guest
instruction stream and charge the SAME clocks. `guest_s`, `insns`, `bus` and
`ticks` are therefore checked as a HARD gate.

`decode_probes` is NOT in the gate and must not be added to it. Block formation
is exactly what this slice changes, so that column is EXPECTED to move; it is
reported for information, and a probe count that did NOT move would mean the
arm never took.

PROVING THE OFF ARM IS OFF. Not from this script, and not from a counter that
reads zero -- a zero counter is equally consistent with "the knob is off" and
with "the instrument is not wired up". Take a barrier-census leg per arm
(`IZARRAVM_DIRECT_BARRIER_CENSUS=1`, plain release build) and read the
`0xF7 /4` memory row: it must be present at roughly 13 M hits in the OFF leg and
ABSENT in the ON leg. That instrument answers differently under each hypothesis,
which a zero counter does not.

Pre-registered bars, written before the first graded leg:
  gp2-586           min-wall ratio >= 1.02 AND pairs above 1 in at least 3 of 4
                    rounds. This is the magnitude claim and the only one.
  duke3d-586-short  CONTROL. Must stay inside +/-2%.
  nascar-586        CONTROL. Must stay inside +/-2%. Its own census re-ranked
                    RCL and LOOP above this row after PR #766, so a large move
                    here would need explaining, not celebrating.
A ratio below the bar is a PARK, not a fail. The most likely null is
RELOCATION: the exits move onto the next rejected-class head (LOOP at 20.9 M,
`0xC1 /4` SHL memory at 2.1 M) instead of disappearing, so the block still
terminates and the slice bought a shorter interpreter visit rather than an
extension. That is what the #764 ladder saw. Read the post-ladder census before
calling a null a mystery.
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` ran ONE row
# of a two-row sweep and EXITED 0. With positional binding off, the stray token
# is a binder error before one line of this script runs. The safe multi-row
# spelling is the COMMA string: `-Rows gp2-586,nascar-586`.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory = $true, ParameterSetName = "Run")][string]$Executable,
    [string]$OutDir = "",
    [int]$Rounds = 4,
    [string[]]$Rows = @("gp2-586"),
    # A/A NULL CONTROL. Both arms run with IZARRAVM_MUL_MEM_ROWS=0 while keeping
    # their 0/1 LABELS, so every leg executes byte-identical code and the ratio
    # this script reports is the box's NOISE FLOOR for this fixture, this leg
    # count and this estimator.
    #
    # It exists because the 2026-08-29 control rows measured that floor by
    # accident and it was larger than the effect: duke3d-586-short read 0.9705
    # over two rounds with the arms provably identical. A ladder result must be
    # compared against a floor measured the SAME WAY, not against a remembered
    # +/-2%.
    #
    # The non-vacuity gate INVERTS under this switch: identical decode_probes
    # becomes REQUIRED, and probes that moved mean the switch failed to pin the
    # arm.
    [switch]$NullControl,
    # Resolve -Rows, print the selection, exit 0. Exists so the self-test's
    # green control can prove a well-formed invocation binds without running a
    # leg. Run-set arguments still have to be supplied; dummies are fine.
    [switch]$BindCheck,
    [Parameter(Mandatory = $true, ParameterSetName = "SelfTest")][switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))

# The benchmark workspace. See scripts/run-fixture-scoreboard.ps1's
# Resolve-BenchRoot for the full rule: IZARRAVM_BENCH_ROOT overrides
# <repo>/.bench, unset and empty both mean the default, and a set-but-missing
# directory is a hard error rather than a silent fallback to the wrong fixtures.
function Resolve-BenchRoot([string]$RepositoryRoot) {
    $configured = $env:IZARRAVM_BENCH_ROOT
    if ([string]::IsNullOrEmpty($configured)) {
        return [IO.Path]::GetFullPath((Join-Path $RepositoryRoot ".bench"))
    }
    $resolved = [IO.Path]::GetFullPath($configured)
    if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
        throw ("IZARRAVM_BENCH_ROOT is set to '$configured' (resolved '$resolved') but " +
            "that directory does not exist. Point it at the benchmark workspace, or " +
            "unset it to use the default <repo>/.bench.")
    }
    return $resolved
}

# The fixture table sits ABOVE the -SelfTest / -BindCheck dispatch because the
# row resolver validates against its keys, and the dispatch has to exit before
# the OutDir side effects below so a self-test child with a dummy -OutDir never
# creates a stray directory. Arguments, cycles and the gp2 injection are copied
# from scripts/run-fixture-scoreboard.ps1's fixture table. nascar-586 has no
# --video there and must not gain one here, because its recorded invariants were
# measured without it.
$fixtures = @{
    # THE MAGNITUDE ROW. The mouse schedule is not optional decoration: GP2's
    # menus are mouse only, four Enter presses leave the framebuffer
    # bit-identical to no input at all, and without the three clicks the run
    # never reaches the race this row exists to measure. GP2 sets its own INT
    # 33h ratio at 1 pixel per mickey on BOTH axes, which is NOT the TOKAMOUS
    # default; a schedule built on the driver default overshoots vertically by
    # 2x and clicks nothing.
    "gp2-586"          = @{
        folder    = "gp2_c"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        cycles    = "13280000000"
        injection = @("--inject-mouse", ("3320000000:home;3652000000:move:320,386;" +
                "3984000000:click;4648000000:move:0,-115;5146000000:click;" +
                "5976000000:move:-273,181;6474000000:click"))
    }
    "nascar-586"       = @{
        folder    = "nascar1_c"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        cycles    = "4980000000"
        injection = @()
    }
    "duke3d-586-short" = @{
        folder    = "duke3d_short_c"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        cycles    = "33200000000"
        injection = @()
    }
}

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
    return , $selected
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
    $known = @($fixtures.Keys)

    $split = Resolve-RowSelection @("gp2-586,nascar-586") $known
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "gp2-586" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "nascar-586" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" gp2-586 , nascar-586") $known
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586,no-such-row") $known } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586,") $known } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586", "gp2-586") $known } `
        "more than once" "a row named twice"

    $pwshExecutable = (Get-Process -Id $PID).Path
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-mulmem-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
                "-Rows", "gp2-586", "nascar-586", "-BindCheck")
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
        if ($failureText -notmatch 'nascar-586') {
            throw ("self-test failed: the mangled -Rows child failed, but not on the " +
                "stray token. stderr: $failureText")
        }

        # GREEN control: the comma spelling of the same selection must bind and
        # resolve, or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
            "-Rows", "gp2-586,nascar-586", "-BindCheck")
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
        if ($listing -notmatch 'nascar-586') {
            throw "self-test failed: the -BindCheck control did not echo the selection"
        }
    }
    finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-mul-mem-rows self-test passed"
}

if ($SelfTest) {
    Invoke-BinderGuardSelfTest
    exit 0
}

$Rows = Resolve-RowSelection $Rows @($fixtures.Keys)
if ($BindCheck) {
    Write-Host ("bind-check ok: rows " + ($Rows -join ", "))
    exit 0
}

$benchRoot = Resolve-BenchRoot $repositoryRoot

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    throw ("OutDir must be pinned explicitly. A results script that defaults its " +
        "OutDir has already overwritten another campaign's profile once.")
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# Every knob the board sets explicitly, so a stray parent-shell value cannot
# turn an observation into a different arm. IZARRAVM_MUL_MEM_ROWS is set per leg
# below and is the ONLY variable that differs between arms.
function Set-BaseEnvironment {
    $env:IZARRAVM_JIT = "1"
    $env:IZARRAVM_JIT16 = "1"
    $env:IZARRAVM_JIT16_486 = "1"
    $env:IZARRAVM_ONE_LOOKUP_STORE = "1"
    $env:IZARRAVM_ONE_LOOKUP_LOAD = "1"
    $env:IZARRAVM_DIRECT_BARRIER_CENSUS = "0"
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_CPU_PROFILE_ADDRS",
            "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        # REMOVAL, not the empty string: an empty value leaves the variable SET,
        # and several readers arm on var_os()/is_some(). The knob under test is
        # one of them in the other direction -- for it, empty means OFF, which
        # AGREES with its default -- which is why it is always set to an explicit
        # "0" or "1" below and never cleared.
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

# The confounder this campaign actually suffers from, MEASURED per leg rather
# than assumed absent. The 2026-08-24 board was taken with an agent running and
# read four rows 8-16% slow; the emulator is single-threaded but the workload is
# L3- and memory-bandwidth sensitive, so a concurrent `cargo -j8` moves it even
# on a 32-core host. A leg whose window contained a builder is marked and can be
# re-run rather than silently averaged in. Copied from
# ladder-extended-ram-screen.ps1, which is where it earned its place: it let a
# peer session re-run one round instead of distrusting sixteen.
function Get-BuilderCount {
    $builders = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -match '^(cargo|rustc|link|lld-link)$' }
    if ($null -eq $builders) { return 0 }
    return @($builders).Count
}

# KNOWN GAP, measured 2026-08-29 rather than supposed. This detector watches FOUR PROCESS
# NAMES, and two wall excursions that evening went unattributed by it: nascar-586 round 2
# (both OFF legs ~64.5 s against 54-58 s everywhere else) and the gp2 A/A null control's
# round 2 (three legs at 114 / 109 / 104 s against ~85 s). Neither had a builder running,
# and a peer session confirmed by direct process scan that it owned nothing on the box.
#
# A NAME LIST CANNOT SEE antivirus, a search indexer, a Windows Update worker, a thermal or
# turbo excursion, or any child process with a different image name. So it answers the SAME
# WAY whether such a thing is present or absent, which is the definition of a non-instrument
# ([[asymmetric-probes-are-not-evidence]]).
#
# TO ATTRIBUTE a fifth excursion rather than merely notice it, sample TOTAL SYSTEM CPU across
# the leg -- e.g. the `\Processor(_Total)\% Processor Time` counter, or a CPU-delta sample of
# every process either side of the leg -- and record it on the row beside the builder count.
# Deliberately NOT added mid-campaign: it changes what every row carries, and the legs already
# taken would not have it. Add it in its own commit, with an A/A to show the sampling itself
# costs nothing.

function Invoke-Leg([string]$Row, [string]$Arm, [int]$Round) {
    $fixture = $fixtures[$Row]
    $stamp = "$Row-arm$Arm-r$Round"
    $copy = Join-Path $OutDir "work-$stamp"
    if (Test-Path $copy) { Remove-Item -Recurse -Force $copy }
    # Copy fresh per run: several fixtures mutate their own tree, and a leg that
    # started from a previous leg's mutated tree is a different workload.
    Copy-Item -Recurse (Join-Path $benchRoot $fixture.folder) $copy
    $profilePath = Join-Path $OutDir "$stamp.json"

    Set-BaseEnvironment
    # Under -NullControl both arms get "0" while keeping their labels, so the
    # reported ratio is the noise floor rather than an effect.
    $env:IZARRAVM_MUL_MEM_ROWS = if ($NullControl) { "0" } else { $Arm }

    $arguments = @()
    $arguments += $fixture.arguments
    $arguments += @("--hdd-folder", $copy)
    $arguments += @("--cycles", $fixture.cycles)
    $arguments += @("--profile-json", $profilePath)
    $arguments += $fixture.injection

    $buildersBefore = Get-BuilderCount
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $Executable @arguments | Out-Null
    $watch.Stop()
    $buildersAfter = Get-BuilderCount
    if ($LASTEXITCODE -ne 0) { throw "$stamp exited $LASTEXITCODE" }

    # duke3d writes its own scorecard into the working copy and then ends the VM
    # with EXITVM. Keep it before the copy goes: its sample count and Info String
    # are invariants of the run, so a leg whose scorecard differs is a leg that
    # did not run the same demo, whatever the wall says.
    $scorecard = Join-Path $copy "DUKEMARK.TXT"
    if (Test-Path -LiteralPath $scorecard) {
        Copy-Item -LiteralPath $scorecard (Join-Path $OutDir "$stamp.dukemark.txt")
    }
    Remove-Item -Recurse -Force $copy
    $report = Get-Content $profilePath -Raw | ConvertFrom-Json
    [pscustomobject]@{
        row      = $Row
        arm      = $Arm
        round    = $Round
        dirty    = ($buildersBefore -gt 0 -or $buildersAfter -gt 0)
        wall_s   = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s  = $report.guest_seconds
        insns    = $report.perf.instructions
        bus      = $report.raw_bus_clocks
        ticks    = $report.master_ticks
        probes   = $report.perf.decode_probes
        rt       = $report.real_time_factor
    }
}

$legs = [Collections.Generic.List[object]]::new()
foreach ($row in $Rows) {
    for ($round = 1; $round -le $Rounds; $round++) {
        # A/B/B/A: the inner pair is order-reversed so a monotone drift across
        # the round cancels rather than accumulating onto one arm.
        $order = if ($round % 2 -eq 1) { @("0", "1", "1", "0") } else { @("1", "0", "0", "1") }
        foreach ($arm in $order) {
            $leg = Invoke-Leg $row $arm $round
            $legs.Add($leg)
            $flag = if ($leg.dirty) { "  <-- BUILDER ACTIVE, leg suspect" } else { "" }
            "{0,-16} arm {1}  r{2}  wall {3,8:N3}s  guest {4,8:N3}  rt {5,6:N3}{6}" -f `
                $leg.row, $leg.arm, $leg.round, $leg.wall_s, $leg.guest_s, $leg.rt, $flag | Write-Host
        }
    }
}

$legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")

Write-Host ""
Write-Host "=== IDENTITY GATE (this slice predicts identity: see the header) ==="
$identityFailed = $false
foreach ($row in $Rows) {
    foreach ($field in @("guest_s", "insns", "bus", "ticks")) {
        # @(...) forces an array: Sort-Object -Unique returns a SCALAR when the
        # values are all equal, which is the passing case, and a bare .Count on
        # a scalar throws. The gate must not crash on success.
        $values = @($legs | Where-Object row -eq $row | ForEach-Object { $_.$field } | Sort-Object -Unique)
        $verdict = if ($values.Count -eq 1) { "OK" } else { "DIVERGED" }
        if ($values.Count -ne 1) { $identityFailed = $true }
        "{0,-16} {1,-8} {2,-9} {3}" -f $row, $field, $verdict, ($values -join " / ") | Write-Host
    }
}

Write-Host ""
if ($NullControl) {
    Write-Host "=== NON-VACUITY, INVERTED (-NullControl: probes MUST be identical) ==="
}
else {
    Write-Host "=== NON-VACUITY (decode_probes moving proves the arm reached block formation) ==="
}
$gateFailed = $false
foreach ($row in $Rows) {
    $offProbes = @($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" } | ForEach-Object probes | Sort-Object -Unique)
    $onProbes = @($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" } | ForEach-Object probes | Sort-Object -Unique)
    $moved = -not (($offProbes -join ",") -eq ($onProbes -join ","))
    # TWO HYPOTHESES, and the 2026-08-29 control rows are why this no longer
    # asserts the first one. Identical probes means EITHER the knob never
    # reached the child, OR the fixture has no population for the row to admit.
    # On the row under test the first is a harness bug; on a CONTROL row the
    # second is the expected and desired answer. The script cannot tell them
    # apart from wall data, so it REPORTS both and refuses to pick -- a barrier
    # census on the row settles it, because the census either shows the row or
    # does not.
    if ($NullControl) {
        if ($moved) { $gateFailed = $true }
        $verdict = if ($moved) { "MOVED -- -NullControl failed to pin the arm" } else { "identical, as required" }
    }
    else {
        $verdict = if ($moved) {
            "moved -- the arm reached block formation"
        }
        else {
            "IDENTICAL -- either the knob never took, OR this fixture has no population. NOT DECIDABLE HERE: take a census."
        }
    }
    "{0,-16} probes OFF {1}  ON {2}  {3}" -f $row, ($offProbes -join "/"), ($onProbes -join "/"),
    $verdict | Write-Host
}

Write-Host ""
Write-Host "=== WALL (OFF = main's refusal, ON = MulMemAcc; ratio > 1 means ON is faster) ==="
foreach ($row in $Rows) {
    $off = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" } | Measure-Object wall_s -Minimum).Minimum
    $on = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" } | Measure-Object wall_s -Minimum).Minimum
    $pairsAbove = 0
    for ($round = 1; $round -le $Rounds; $round++) {
        $o = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
        $n = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
        if ($o -gt $n) { $pairsAbove++ }
    }
    $ratio = if ($on -gt 0) { $off / $on } else { [double]::NaN }
    "{0,-16} min-wall OFF {1,8:N3}s  ON {2,8:N3}s  ratio {3,6:N4}  pairs above 1: {4}/{5}" -f `
        $row, $off, $on, $ratio, $pairsAbove, $Rounds | Write-Host
}

$dirtyLegs = @($legs | Where-Object dirty)
if ($dirtyLegs.Count -gt 0) {
    Write-Host ""
    "CONTAMINATION: {0} of {1} legs ran with a builder active. Those legs are SUSPECT." -f `
        $dirtyLegs.Count, $legs.Count | Write-Host
    $dirtyLegs | ForEach-Object { "  {0} arm {1} r{2}" -f $_.row, $_.arm, $_.round } | Write-Host
    Write-Host "Re-run the affected rounds on a quiet machine before quoting any wall number."
}
else {
    Write-Host ""
    Write-Host "No builder was active during any leg."
}

if ($gateFailed) {
    Write-Host ""
    Write-Host "-NullControl FAILED: decode_probes moved, so the switch did not pin both arms"
    Write-Host "to IZARRAVM_MUL_MEM_ROWS=0. The ratio above is not a noise floor."
    exit 1
}

if ($identityFailed) {
    Write-Host ""
    Write-Host "IDENTITY GATE FAILED. The admission moved guest time or the guest instruction"
    Write-Host "stream. The design predicts it cannot: group 3 charges clocks(2) in both the"
    Write-Host "interpreter and the emitted form, and the dword read is declared. Explain the"
    Write-Host "divergence before quoting any wall number -- and re-check the gp2 frame pin,"
    Write-Host "which is a cutoff-phase sample and moves with ppm timing shifts."
    exit 1
}
