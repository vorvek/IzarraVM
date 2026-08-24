# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The PIT bulk-advance ladder: doom-486 and doom-586, both arms from ONE binary.

.DESCRIPTION
`run-fixture-scoreboard.ps1` REMOVES every IZARRAVM_* variable from the child
(see its Get-RowEnvironment), so it can only ever run a knob's DEFAULT arm. Arm
work therefore needs a direct invocation, and this is it.

The slice is HOST-ONLY. That gives a cheap, strong falsifier which this script
checks BEFORE it reports any wall number: guest_seconds, perf.instructions,
raw_bus_clocks, realtics and gametics must be IDENTICAL across the two arms. If
they are not, the slice is not host-only and no wall number can rescue it.

Arms, both reachable in one binary:
  IZARRAVM_PIT_BULK_ADVANCE=0  the OFF arm, main's per-CLK loop, the A/B base
  IZARRAVM_PIT_BULK_ADVANCE=1  the ON arm, the closed-form advance

Legs are INTERLEAVED A/B/B/A per round, never blocked by arm: a monotone host
drift over a blocked run would land entirely on one arm and read as an effect.

Pre-registered bars, from the implementer's design:
  doom-486  min-wall ratio >= 1.03 AND 12/12 pairs above 1
  doom-586  min-wall ratio >= 1.01 AND the sign agrees with the 486 row
#>

param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [string]$OutDir = "",
    [int]$Rounds = 6,
    [string[]]$Rows = @("doom-486", "doom-586")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))

# The benchmark workspace. See scripts/run-fixture-scoreboard.ps1's Resolve-BenchRoot
# for the full rule: IZARRAVM_BENCH_ROOT overrides <repo>/.bench, unset and empty both
# mean the default, and a set-but-missing directory is a hard error rather than a
# silent fallback to the wrong fixture set.
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
$benchRoot = Resolve-BenchRoot $repositoryRoot

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    throw "OutDir must be pinned explicitly. A results script that defaults its OutDir has already overwritten another campaign's profile once."
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$fixtures = @{
    "doom-486" = @{
        folder    = "jemmex_doom_c"
        arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
        cycles    = "8000000000"
    }
    "doom-586" = @{
        folder    = "jemmex_doom_c"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        cycles    = "6640000000"
    }
}

# Every knob the board sets explicitly, so a stray parent-shell value cannot
# turn an observation into a different arm. IZARRAVM_PIT_BULK_ADVANCE is set
# per leg below and is the ONLY variable that differs between arms.
function Set-BaseEnvironment {
    $env:IZARRAVM_JIT = "1"
    $env:IZARRAVM_JIT16 = "1"
    $env:IZARRAVM_JIT16_486 = "1"
    $env:IZARRAVM_ONE_LOOKUP_STORE = "1"
    $env:IZARRAVM_ONE_LOOKUP_LOAD = "1"
    $env:IZARRAVM_DIRECT_BARRIER_CENSUS = "0"
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        # REMOVAL, not the empty string: an empty value leaves the variable SET,
        # and several readers arm on var_os()/is_some().
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

# The confounder this campaign actually suffers from, MEASURED per leg rather
# than assumed absent. The 2026-08-24 board was taken with an agent running and
# read four rows 8-16% slow; the emulator is single-threaded but the workload is
# L3- and memory-bandwidth sensitive, so a concurrent `cargo -j8` moves it even
# on a 32-core host. A leg whose window contained a builder is marked and can be
# re-run rather than silently averaged in.
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
    Copy-Item -Recurse (Join-Path $benchRoot $fixture.folder) $copy
    $profilePath = Join-Path $OutDir "$stamp.json"

    Set-BaseEnvironment
    $env:IZARRAVM_PIT_BULK_ADVANCE = $Arm

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
        row       = $Row
        arm       = $Arm
        round     = $Round
        dirty     = ($buildersBefore -gt 0 -or $buildersAfter -gt 0)
        wall_s    = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s   = $report.guest_seconds
        insns     = $report.perf.instructions
        bus       = $report.raw_bus_clocks
        # Doom's oracle lives under `timedemo`, NOT at the top level. LOWER
        # realtics is faster; gametics 2134 is what says the demo itself is
        # unchanged. Both are guest-reported and so are robust to host noise,
        # which is exactly why they are the identity gate for a host-only slice.
        realtics  = $report.timedemo.realtics
        gametics  = $report.timedemo.gametics
        bulk      = if ($report.PSObject.Properties['pit_bulk_advance']) { $report.pit_bulk_advance } else { $null }
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
            "{0,-10} arm {1}  r{2}  wall {3,8:N3}s  guest {4,8:N3}  realtics {5}{6}" -f `
                $leg.row, $leg.arm, $leg.round, $leg.wall_s, $leg.guest_s, $leg.realtics, $flag | Write-Host
        }
    }
}

$legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")

Write-Host ""
Write-Host "=== IDENTITY GATE (host-only slice: these MUST be identical across arms) ==="
$identityFailed = $false
foreach ($row in $Rows) {
    foreach ($field in @("guest_s", "insns", "bus", "realtics", "gametics")) {
        # @(...) forces an array: Sort-Object -Unique returns a SCALAR when the
        # values are all equal, which is the passing case, and a bare .Count on
        # a scalar throws. The gate must not crash on success.
        $values = @($legs | Where-Object row -eq $row | ForEach-Object { $_.$field } | Sort-Object -Unique)
        $verdict = if ($values.Count -eq 1) { "OK" } else { "DIVERGED" }
        if ($values.Count -ne 1) { $identityFailed = $true }
        "{0,-10} {1,-10} {2,-9} {3}" -f $row, $field, $verdict, ($values -join " / ") | Write-Host
    }
}

Write-Host ""
Write-Host "=== WALL ==="
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
    "{0,-10} min-wall OFF {1,8:N3}s  ON {2,8:N3}s  ratio {3,6:N4}  pairs above 1: {4}/{5}" -f `
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
    Write-Host "IDENTITY GATE FAILED. The slice is not host-only. No wall number above is usable."
}
