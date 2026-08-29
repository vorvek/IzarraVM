# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The extended-RAM screen ladder: 32-bit rows, both arms from ONE binary.

.DESCRIPTION
`run-fixture-scoreboard.ps1` REMOVES every IZARRAVM_* variable from the child
(see its Get-RowEnvironment), so it can only ever run a knob's DEFAULT arm. Arm
work therefore needs a direct invocation, and this is it.

WHAT IS BEING MEASURED. The bus classifies an address as a device window (ROM,
VGA, Margo or Distira) to decide what wait-state to charge it, and its only fast
screen was `< 0x000A_0000` -- conventional memory. A 32-bit game runs its code
and keeps its data above 1 MB, so every instruction-fetch run and every data
access it makes missed that screen and walked the aperture gauntlet to be told
what the address range already guarantees. The screen answers it directly.

WHY ONE BINARY. The effect is a few percent, and layout variance between two
builds of this workspace has been measured at 3.7% on this box -- larger than
what is being measured. A two-binary comparison cannot carry a claim this size,
so the knob exists and both arms come out of the same executable.

The slice is HOST-ONLY. That gives a cheap, strong falsifier which this script
checks BEFORE it reports any wall number: guest_seconds, perf.instructions,
raw_bus_clocks, master_ticks and perf.decode_probes must be IDENTICAL across the
two arms. A screen that mis-classified one address would charge a different
wait-state, which moves guest time, which moves those columns. If they are not
identical, the slice is not host-only and no wall number can rescue it.

Arms, both reachable in one binary:
  IZARRAVM_EXTENDED_RAM_SCREEN=0  the OFF arm, main's gauntlet, the A/B base
  IZARRAVM_EXTENDED_RAM_SCREEN=1  the ON arm, the screen

ROW CHOICE. Both defaults are 32-bit protected-mode titles, which is the shape
the screen is for. `quake-586` earns its place twice over: its demo finishes
before the budget and the run stops in an idle tail whose length moves with the
TIMING MODEL (see the scoreboard's own note on that row), so its instruction
count is the most sensitive identity column on the board. A screen that moved a
single charged wait-state would show up there first.

Pre-registered bars, from the riprofile shares the screen targets
(`charge_classified_instruction_fetch_run` plus `Vega::owns_memory`: 2.49% of
nascar-586 wall, 4.65% of duke3d-586):
  nascar-586  min-wall ratio >= 1.015 AND pairs above 1 in at least 3 of 4 rounds
  quake-586   sign agrees with nascar (this row is short, so it is a direction
              check and a timing falsifier, not a magnitude claim)
A ratio below the bar is a PARK, not a fail: the change removes work and cannot
make the emulator slower, so a null result means the work was cheaper than the
profile suggested, and the honest report is the null.
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` (the shape
# an outer PowerShell produces from `-Fixtures @('a','b')`) ran ONE row of a
# two-row sweep and EXITED 0. With positional binding off, the stray token is a
# binder error before one line of this script runs. The safe multi-row spelling
# is the COMMA string: `-Rows nascar-586,quake-586`.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory = $true, ParameterSetName = "Run")][string]$Executable,
    [string]$OutDir = "",
    [int]$Rounds = 4,
    [string[]]$Rows = @("nascar-586", "quake-586"),
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
# creates a stray directory. Arguments and cycles are copied from
# scripts/run-fixture-scoreboard.ps1's fixture table; nascar-586 has no --video
# there and must not gain one here, because its recorded invariants were
# measured without it.
$fixtures = @{
    "nascar-586"       = @{
        folder    = "nascar1_c"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        cycles    = "4980000000"
    }
    "quake-586"        = @{
        folder    = "quake_c"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        cycles    = "6200000000"
    }
    # Not a default row: 74 s a leg against nascar's 56, and four rounds of it
    # alone is 20 minutes. Name it explicitly when the box is free for longer.
    "duke3d-586-short" = @{
        folder    = "duke3d_short_c"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        cycles    = "33200000000"
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

    $split = Resolve-RowSelection @("nascar-586,quake-586") $known
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "nascar-586" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "quake-586" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" nascar-586 , quake-586") $known
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("nascar-586,no-such-row") $known } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("nascar-586,") $known } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("nascar-586", "nascar-586") $known } `
        "more than once" "a row named twice"

    $pwshExecutable = (Get-Process -Id $PID).Path
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-extram-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
                "-Rows", "nascar-586", "quake-586", "-BindCheck")
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
            "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
            "-Rows", "nascar-586,quake-586", "-BindCheck")
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
    }
    finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-extended-ram-screen self-test passed"
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
# turn an observation into a different arm. IZARRAVM_EXTENDED_RAM_SCREEN is set
# per leg below and is the ONLY variable that differs between arms.
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
        # one of them in the other direction -- for it, empty means OFF -- which
        # is why it is always set to an explicit "0" or "1" below and never
        # cleared.
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

# The confounder this campaign actually suffers from, MEASURED per leg rather
# than assumed absent. The 2026-08-24 board was taken with an agent running and
# read four rows 8-16% slow; the emulator is single-threaded but the workload is
# L3- and memory-bandwidth sensitive, so a concurrent `cargo -j8` moves it even
# on a 32-core host. A leg whose window contained a builder is marked and can be
# re-run rather than silently averaged in. This matters more than usual here:
# a second agent shares this box, and the two of us hand the box back and forth.
function Get-BuilderCount {
    $builders = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -match '^(cargo|rustc|link|lld-link)$' }
    if ($null -eq $builders) { return 0 }
    return @($builders).Count
}

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
    $env:IZARRAVM_EXTENDED_RAM_SCREEN = $Arm

    $arguments = @()
    $arguments += $fixture.arguments
    $arguments += @("--hdd-folder", $copy)
    $arguments += @("--cycles", $fixture.cycles)
    $arguments += @("--profile-json", $profilePath)

    $buildersBefore = Get-BuilderCount
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $Executable @arguments | Out-Null
    $watch.Stop()
    $buildersAfter = Get-BuilderCount
    if ($LASTEXITCODE -ne 0) { throw "$stamp exited $LASTEXITCODE" }

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
Write-Host "=== IDENTITY GATE (host-only slice: these MUST be identical across arms) ==="
$identityFailed = $false
foreach ($row in $Rows) {
    foreach ($field in @("guest_s", "insns", "bus", "ticks", "probes")) {
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
Write-Host "=== WALL (OFF = main's gauntlet, ON = the screen; ratio > 1 means the screen is faster) ==="
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

if ($identityFailed) {
    Write-Host ""
    Write-Host "IDENTITY GATE FAILED. The slice is not host-only: the screen moved a"
    Write-Host "charged wait-state, which moved guest time. No wall number above is usable."
    exit 1
}
