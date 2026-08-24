# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The Jcc shadow-flags ladder: duke3d-586-short, both arms from ONE binary.

.DESCRIPTION
NULLING TRAP, and it points the opposite way to the PIT knob merged the same day:
`IZARRAVM_JCC_SHADOW` reads unset == "" == 0 == off as OFF, so **the ON leg must
EXPORT `1`**. A nulled variable silently runs the base and would be read as "the
slice did nothing".

VACUITY CHECK, run before any wall number is believed: the ON arm must show
non-zero `jcc_sites_*`, and the OFF arm must show all four zero. Without it a
mistyped leg reads as a null result rather than as a mistake. This session found
seven instruments that reported green regardless of what they gated; this is the
cheap guard against being the eighth.

Contamination is MEASURED per leg, not assumed absent: builder processes AND host
CPU, because the builder check is blind to everything outside our own toolchain --
a browser or mail client moved a leg by 5.5 s earlier tonight with `dirty` false.
#>

param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [int]$Rounds = 4
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

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Get-BuilderCount {
    $builders = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -match '^(cargo|rustc|link|lld-link)$' }
    if ($null -eq $builders) { return 0 }
    return @($builders).Count
}

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

function Invoke-Leg([string]$Arm, [int]$Round) {
    $stamp = "duke3d-586-short-arm$Arm-r$Round"
    $copy = Join-Path $OutDir "work-$stamp"
    if (Test-Path $copy) { Remove-Item -Recurse -Force $copy }
    Copy-Item -Recurse (Join-Path $benchRoot "duke3d_short_c") $copy
    $profilePath = Join-Path $OutDir "$stamp.json"

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
    $env:IZARRAVM_SEGMENT_RETIRE_GOVERNOR = "cap"
    # Both merged today, both stated rather than inherited.
    $env:IZARRAVM_CHAIN_ENTRY_CHECK = "1"
    $env:IZARRAVM_PIT_BULK_ADVANCE = "1"
    # THE ARM UNDER TEST. Exported on BOTH legs; see the nulling trap above.
    $env:IZARRAVM_JCC_SHADOW = $Arm
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }

    $arguments = @(
        "--cpu", "586", "--memory-mib", "64", "--video", "vega",
        "--hdd-folder", $copy, "--cycles", "33200000000", "--profile-json", $profilePath
    )

    $before = Get-BuilderCount
    $cpuBefore = Get-HostCpuPercent
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $Executable @arguments | Out-Null
    $watch.Stop()
    $after = Get-BuilderCount
    $cpuAfter = Get-HostCpuPercent
    if ($LASTEXITCODE -ne 0) { throw "$stamp exited $LASTEXITCODE" }

    Remove-Item -Recurse -Force $copy
    $report = Get-Content $profilePath -Raw | ConvertFrom-Json
    $perf = Get-Field $report "perf"
    $stalls = Get-Field $report "direct_stalls"
    $total = Get-Field $perf "instructions"
    $direct = Get-Field $perf "jit_direct_insns"

    $sites = 0
    if ($null -ne $stalls) {
        foreach ($p in $stalls.PSObject.Properties) {
            if ($p.Name -like "jcc_sites*" -and $p.Value -is [int64]) { $sites += $p.Value }
            elseif ($p.Name -like "jcc_sites*" -and $p.Value -is [int]) { $sites += $p.Value }
        }
    }

    [pscustomobject]@{
        arm        = $Arm
        round      = $Round
        dirty      = ($before -gt 0 -or $after -gt 0)
        cpu_before = $cpuBefore
        cpu_after  = $cpuAfter
        wall_s     = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s    = Get-Field $report "guest_seconds"
        insns      = $total
        bus        = Get-Field $report "raw_bus_clocks"
        entries    = Get-Field $perf "jit_direct_entries"
        native_pct = if ($total) { [math]::Round(100.0 * $direct / $total, 3) } else { $null }
        jcc_sites  = $sites
    }
}

$legs = [Collections.Generic.List[object]]::new()
for ($round = 1; $round -le $Rounds; $round++) {
    # A/B/B/A, order reversed on even rounds so a monotone drift cancels.
    $order = if ($round % 2 -eq 1) { @("0", "1", "1", "0") } else { @("1", "0", "0", "1") }
    foreach ($arm in $order) {
        $leg = Invoke-Leg $arm $round
        $legs.Add($leg)
        $flag = if ($leg.dirty) { "  <-- BUILDER ACTIVE" } else { "" }
        "arm {0}  r{1}  wall {2,9:N3}s  native {3,7:N2}%  jcc_sites {4,12:N0}  cpu {5}/{6}%{7}" -f `
            $leg.arm, $leg.round, $leg.wall_s, $leg.native_pct, $leg.jcc_sites,
        $leg.cpu_before, $leg.cpu_after, $flag | Write-Host
        $legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")
    }
}

Write-Host ""
Write-Host "=== VACUITY CHECK: ON must engage, OFF must not ==="
$onSites = @($legs | Where-Object arm -eq "1" | ForEach-Object { $_.jcc_sites } | Sort-Object -Unique)
$offSites = @($legs | Where-Object arm -eq "0" | ForEach-Object { $_.jcc_sites } | Sort-Object -Unique)
"ON  jcc_sites: $($onSites -join ', ')"  | Write-Host
"OFF jcc_sites: $($offSites -join ', ')" | Write-Host
# @(...) forces an array: Where-Object returns $null when nothing matches, which
# is the PASSING case here, and a bare .Count on $null throws. A vacuity check
# that crashes on success is its own kind of instrument that cannot fail.
$vacuous = ($onSites -contains 0) -or (@($offSites | Where-Object { $_ -ne 0 }).Count -gt 0)
if ($vacuous) { Write-Host "VACUOUS: the arms did not do what they claim. No wall number below is usable." }

Write-Host ""
Write-Host "=== GUEST IDENTITY (must be identical: the slice changes emitted code, not guest work) ==="
foreach ($field in @("guest_s", "insns", "bus", "entries")) {
    $values = @($legs | ForEach-Object { $_.$field } | Sort-Object -Unique)
    "{0,-10} {1,-9} {2}" -f $field, $(if ($values.Count -eq 1) { "OK" } else { "DIVERGED" }), ($values -join " / ") |
        Write-Host
}

Write-Host ""
Write-Host "=== WALL ==="
$clean = @($legs | Where-Object { -not $_.dirty })
$pool = if ($clean.Count -ge 4) { $clean } else { $legs }
$off = ($pool | Where-Object arm -eq "0" | Measure-Object wall_s -Minimum).Minimum
$on = ($pool | Where-Object arm -eq "1" | Measure-Object wall_s -Minimum).Minimum
$pairs = 0
for ($round = 1; $round -le $Rounds; $round++) {
    $o = ($pool | Where-Object { $_.arm -eq "0" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
    $n = ($pool | Where-Object { $_.arm -eq "1" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
    if ($null -ne $o -and $null -ne $n -and $o -gt $n) { $pairs++ }
}
"min-wall OFF {0,9:N3}s  ON {1,9:N3}s  ratio {2,6:N4}  rounds ON-faster {3}/{4}" -f `
    $off, $on, ($off / $on), $pairs, $Rounds | Write-Host
"legs used: {0} of {1} ({2} dirty)" -f $pool.Count, $legs.Count, @($legs | Where-Object dirty).Count | Write-Host
