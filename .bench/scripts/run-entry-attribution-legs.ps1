# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4
<#
.SYNOPSIS
Runs the four entry-attribution observer legs on the Tomb Raider DOS/4GW loader fixture.

.DESCRIPTION
Mirrors `scripts/run-tombraid-loader-gate.ps1`'s leg mechanics -- exact fixture copy, pinned
processor affinity, every IZARRAVM_* variable removed before the arm's own are set -- but runs the
observer's own legs rather than the gate's A/B: one disarmed, one FULL, one SAMPLE=N, one COARSE.

This is the OBSERVER's smoke and acceptance harness, not a performance gate: it makes no wall
claim and computes no median. `.bench/scripts/entry-attribution-report.py` reads what it writes.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$ResultsDirectory,
    [int]$ProcessorIndex = 8,
    [int]$SampleStride = 64,
    [int]$HostTimeoutSeconds = 300,
    [string]$PlainExecutable = "",
    # The fixture lives under the MAIN repository's `.bench`, which is gitignored and therefore
    # absent from a worktree. Defaulted below to the worktree's own copy when it has one.
    [string]$FixtureRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$fixtureRoot = if ($FixtureRoot -ne "") {
    $FixtureRoot
} else {
    Join-Path $repositoryRoot ".bench\tombraid_loader_c"
}
if (-not (Test-Path -LiteralPath $fixtureRoot)) {
    throw "no loader fixture at $fixtureRoot; pass -FixtureRoot (a worktree has none, since .bench is gitignored)"
}
$cycleBudget = [uint64]500000000
$fixtureFiles = @(
    "AUTOEXEC.BAT",
    "CONFIG.SYS",
    "GAMES\TOMBRAID\DOS4GW.EXE",
    "GAMES\TOMBRAID\TOMB.EXE"
)

function Copy-Fixture([string]$Destination) {
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    foreach ($relative in $fixtureFiles) {
        $target = Join-Path $Destination $relative
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
        Copy-Item -LiteralPath (Join-Path $fixtureRoot $relative) -Destination $target
    }
}

function Get-LegEnvironment([string]$HomePath, [string]$Arm, [int]$Stride) {
    $environment = @{}
    # Every IZARRAVM_* variable is REMOVED, not blanked: the standing env-null trap is that a
    # PowerShell null leaves the variable present and empty, which several knobs read as OFF while
    # unset means ON.
    foreach ($name in [Environment]::GetEnvironmentVariables().Keys) {
        if ([string]$name -like "IZARRAVM_*") { $environment[[string]$name] = $null }
    }
    $environment["RUST_LOG"] = $null
    $environment["HOME"] = $HomePath
    $environment["USERPROFILE"] = $HomePath
    $environment["IZARRAVM_JIT"] = "1"
    $environment["IZARRAVM_JIT16"] = "1"
    $environment["IZARRAVM_JIT16_486"] = "1"
    $environment["IZARRAVM_ONE_LOOKUP_LOAD"] = "1"
    $environment["IZARRAVM_ONE_LOOKUP_STORE"] = "1"
    $environment["IZARRAVM_PHASE_INTERVAL_MS"] = "2100"
    if ($Arm -ne "") { $environment["IZARRAVM_DIRECT_ENTRY_ATTRIBUTION"] = $Arm }
    if ($Stride -gt 1) {
        $environment["IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE"] = $Stride.ToString()
    }
    return $environment
}

function Invoke-Leg([string]$Name, [string]$Binary, [string]$Arm, [int]$Stride) {
    $legRoot = Join-Path $ResultsDirectory $Name
    New-Item -ItemType Directory -Force -Path $legRoot | Out-Null
    $workingFixture = Join-Path $legRoot "fixture"
    $legHomePath = Join-Path $legRoot "home"
    New-Item -ItemType Directory -Force -Path $legHomePath | Out-Null
    Copy-Fixture $workingFixture

    $profilePath = Join-Path $legRoot "profile.json"
    $arguments = @(
        "--cpu", "586", "--memory-mib", "64",
        "--hdd-folder", $workingFixture,
        "--cycles", $cycleBudget.ToString(),
        "--profile-json", $profilePath,
        "--result-ppm", (Join-Path $legRoot "final.ppm")
    )
    $environment = Get-LegEnvironment $legHomePath $Arm $Stride
    $startInfo = @{
        FilePath = $Binary
        ArgumentList = $arguments
        NoNewWindow = $true
        PassThru = $true
        RedirectStandardOutput = (Join-Path $legRoot "stdout.txt")
        RedirectStandardError = (Join-Path $legRoot "stderr.txt")
        Environment = $environment
    }
    $wall = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process @startInfo
    $process.Refresh()
    $childMask = $process.ProcessorAffinity.ToInt64()
    if ($childMask -ne $script:requiredAffinityMask) {
        try { $process.Kill($true) } catch { }
        throw "$Name child affinity is 0x$($childMask.ToString('x')), expected 0x$($script:requiredAffinityMask.ToString('x'))"
    }
    if (-not $process.WaitForExit($HostTimeoutSeconds * 1000)) {
        try { $process.Kill($true) } catch { }
        throw "$Name exceeded $HostTimeoutSeconds seconds"
    }
    $wall.Stop()
    if ($process.ExitCode -ne 0) { throw "$Name exited $($process.ExitCode)" }
    $hash = (Get-FileHash -LiteralPath (Join-Path $legRoot "final.ppm") -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Host ("{0,-12} wall {1,7:N2} s  frame {2}" -f $Name, $wall.Elapsed.TotalSeconds, $hash.Substring(0, 16))
    return [pscustomobject]@{ name = $Name; wall_s = $wall.Elapsed.TotalSeconds; frame = $hash; profile = $profilePath }
}

New-Item -ItemType Directory -Force -Path $ResultsDirectory | Out-Null
$parent = [Diagnostics.Process]::GetCurrentProcess()
$originalAffinity = $parent.ProcessorAffinity
$script:requiredAffinityMask = [int64]1 -shl $ProcessorIndex
$parent.ProcessorAffinity = [IntPtr]$script:requiredAffinityMask
try {
    $legs = @()
    $legs += Invoke-Leg "disarmed" $Executable "" 1
    $legs += Invoke-Leg "full"     $Executable "1" 1
    $legs += Invoke-Leg "sample"   $Executable "1" $SampleStride
    $legs += Invoke-Leg "coarse"   $Executable "2" 1
    if ($PlainExecutable -ne "") {
        $legs += Invoke-Leg "plain" $PlainExecutable "" 1
    }
    $legs | ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath (Join-Path $ResultsDirectory "legs.json") -Encoding utf8
} finally {
    $parent.ProcessorAffinity = $originalAffinity
}
