# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
Boot-phase profile across CPU personas.

.DESCRIPTION
Boots the C: drive the GUI would mount, once per persona, and prints a
comparison of where wall time goes in each boot phase: POST, boot, prompt idle,
command exec, and disk load.

This is a PROFILER, NOT AN A/B LADDER. It runs each persona once, with no
pairing, no interleaving, no measurement lock and no determinism check, and its
phase slicing perturbs the run loop. Never make an accept/reject decision from
these numbers -- that is scripts/run-realtime-gate.ps1's job.

.EXAMPLE
./scripts/run-boot-profile.ps1
Compare 486 and 586, the differential that isolates a persona-scaling problem.

.EXAMPLE
./scripts/run-boot-profile.ps1 -Modes all -CpuCensus
All four personas, each with a sampled guest opcode census.
#>

param(
    [ValidateSet("Both", "386-slow", "386", "486", "586", "all")]
    [string]$Modes = "Both",
    [int]$IdleSeconds = 10,
    [string]$LoadFile = "",
    [string]$ResultsDirectory = "",
    [switch]$CpuCensus,
    [switch]$MachinePhases,
    [switch]$RipProfile,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$modeList = switch ($Modes) {
    "Both" { @("486", "586") }
    "all" { @("386-slow", "386", "486", "586") }
    default { @($Modes) }
}

# The RIP sampler needs symbols, which the release profile strips.
$cargoProfile = if ($RipProfile) { "profiling" } else { "release" }
$executable = "target/$cargoProfile/izarravm.exe"

if (-not $SkipBuild) {
    Write-Host "building ($cargoProfile)..." -ForegroundColor Cyan
    # -j8: a full-core build on this box starves everything else.
    cargo build -j8 --profile $cargoProfile -p izarravm
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "missing executable: $executable"
}

if ([string]::IsNullOrWhiteSpace($ResultsDirectory)) {
    $ResultsDirectory = Join-Path ([IO.Path]::GetTempPath()) "izarra-boot-profile-$PID"
}
New-Item -ItemType Directory -Force $ResultsDirectory | Out-Null
Write-Host "results: $ResultsDirectory" -ForegroundColor Cyan

$rows = [Collections.Generic.List[object]]::new()
foreach ($mode in $modeList) {
    Write-Host ""
    Write-Host "=== $mode ===" -ForegroundColor Yellow
    $jsonPath = Join-Path $ResultsDirectory "boot-profile-$mode.json"
    $arguments = @(
        "--headless-boot-profile"
        "--cpu", $mode
        "--idle-seconds", $IdleSeconds
        "--profile-json", $jsonPath
    )
    if (-not [string]::IsNullOrWhiteSpace($LoadFile)) {
        $arguments += @("--load-file", $LoadFile)
    }

    if ($CpuCensus) {
        $env:IZARRAVM_CPU_PROFILE = "512"
    } else {
        Remove-Item Env:IZARRAVM_CPU_PROFILE -ErrorAction SilentlyContinue
    }
    if ($MachinePhases) {
        $env:IZARRAVM_MACHINE_PROFILE = "1"
    } else {
        Remove-Item Env:IZARRAVM_MACHINE_PROFILE -ErrorAction SilentlyContinue
    }
    if ($RipProfile) {
        $env:IZARRAVM_RIP_PROFILE = Join-Path $ResultsDirectory "rip-$mode.txt"
    } else {
        Remove-Item Env:IZARRAVM_RIP_PROFILE -ErrorAction SilentlyContinue
    }

    & $executable @arguments 2>&1 | Tee-Object -FilePath (
        Join-Path $ResultsDirectory "run-$mode.log"
    )
    if (-not (Test-Path -LiteralPath $jsonPath -PathType Leaf)) {
        Write-Warning "$mode produced no profile JSON; skipping it in the summary"
        continue
    }
    $report = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json
    foreach ($phase in $report.phases) {
        $rows.Add([pscustomobject][ordered]@{
            mode = $mode
            phase = $phase.name
            reached = $phase.reached
            wall_s = $phase.wall_seconds
            guest_s = $phase.guest_seconds
            rt = $phase.real_time_factor
            native = $phase.direct_native_coverage
            sectors = $phase.katea.sector_reads
            host_reads = $phase.katea.host_file_reads
            host_ms = $phase.katea.host_wall_ns / 1e6
        })
    }
}

if ($rows.Count -eq 0) {
    throw "no persona produced a profile"
}

Write-Host ""
Write-Host "=== real-time factor by phase (guest seconds per wall second) ===" -ForegroundColor Green
$phaseOrder = @("post", "boot", "idle", "exec", "diskload")
$table = foreach ($phase in $phaseOrder) {
    $record = [ordered]@{ phase = $phase }
    foreach ($mode in $modeList) {
        $row = $rows | Where-Object { $_.mode -eq $mode -and $_.phase -eq $phase }
        # Formatted invariantly: this box's locale uses a decimal comma, and a
        # table reading "0,22" next to the emulator's own "0.223" invites a
        # misread of the one number the whole report exists to convey.
        $record[$mode] = if ($null -eq $row -or -not $row.reached) {
            "n/a"
        } else {
            ([double]$row.rt).ToString("F3", [Globalization.CultureInfo]::InvariantCulture)
        }
    }
    [pscustomobject]$record
}
$table | Format-Table -AutoSize

Write-Host "1.000 is real time. A phase far below it is a phase the emulator cannot keep up with."
Write-Host "Reminder: this is a profiler. Do not accept or reject a change on these numbers."
