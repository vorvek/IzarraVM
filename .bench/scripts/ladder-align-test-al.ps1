# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
GATE 1 for IZARRAVM_DIRECT_ALIGN_TEST_AL: wolf3d-586, one binary, two arms, n=8 ABBA pairs.

.DESCRIPTION
Identity first (one OFF, one ON), then 8 interleaved A/B/B/A pairs. Pairing is
index-matched-within-arm in leg order, declared before the run. Contamination
re-run bar is cpu_before/after > 25 (this host's idle sits 14-23%).

Wall is REPORT only: a NEUTRAL result does not decline the knob. Identity
(hash, guest clocks, bus clocks) is a STOP. jit_direct_insns/entries may
move if occupancy changes; that is logged, not a STOP.

See dev_docs/specs/2026-08-29-align-test-al-design.md section 7.3.
#>

param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$OutDir,
    [int]$Pairs = 8,
    [int]$ProcessorIndex = 8,
    [switch]$IdentityOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))
$bench = Join-Path $repo ".bench"
$source = Join-Path $bench "wolf3d_c"
$lockPath = Join-Path $bench "locks\align-test-al-ladder.lock"
$expectedHash = "e33418bbd34c13ad9c99e23c3d1cddb68df08beecec8676378557a4cc102f963"

if (-not (Test-Path -LiteralPath $Executable)) { throw "Missing executable: $Executable" }
if (-not (Test-Path -LiteralPath $source)) { throw "Missing fixture: $source" }

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
"$PID align-test-al-ladder" | Set-Content -LiteralPath $lockPath -NoNewline

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

function Set-LadderEnvironment([string]$AlignArm) {
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
    $env:IZARRAVM_DIRECT_ALIGN_TEST_AL = $AlignArm
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

function Invoke-Leg([string]$Arm, [string]$Label) {
    $scratch = Join-Path $OutDir "work-$Label"
    if (Test-Path -LiteralPath $scratch) { Remove-Item -Recurse -Force -LiteralPath $scratch }
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    $robo = Start-Process -FilePath robocopy.exe -ArgumentList @(
        $source, $scratch, "/MIR", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:2", "/W:1"
    ) -NoNewWindow -Wait -PassThru
    if ($robo.ExitCode -ge 8) { throw "robocopy failed ($($robo.ExitCode)) for $Label" }

    $json = Join-Path $OutDir "$Label.json"
    $ppm = Join-Path $OutDir "$Label.ppm"
    $outLog = Join-Path $OutDir "$Label.out"
    $errLog = Join-Path $OutDir "$Label.err"
    $arguments = @(
        "--cpu", "586", "--memory-mib", "64", "--video", "vega",
        "--hdd-folder", $scratch,
        "--cycles", "12000000000",
        "--profile-json", $json,
        "--result-ppm", $ppm,
        "--inject-keys", "2000000000:`n"
    )

    Set-LadderEnvironment $Arm
    $cpuBefore = Get-HostCpuPercent
    $self = Get-Process -Id $PID
    $parentMask = $self.ProcessorAffinity.ToInt64()
    $mask = [int64]1 -shl $ProcessorIndex
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $self.ProcessorAffinity = [IntPtr]$mask
        $self.Refresh()
        $proc = Start-Process -FilePath $Executable -ArgumentList $arguments -NoNewWindow -PassThru `
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

    Remove-Item -Recurse -Force -LiteralPath $scratch -ErrorAction SilentlyContinue
    $report = Get-Content -LiteralPath $json -Raw | ConvertFrom-Json
    $perf = Get-Field $report "perf"
    $stalls = Get-Field $report "direct_stalls"
    $sites = Get-Field $stalls "align_test_al_sites"
    if ($null -eq $sites) {
        throw "$Label missing direct_stalls.align_test_al_sites"
    }
    $hash = $null
    if (Test-Path -LiteralPath $ppm) {
        $hash = (Get-FileHash -LiteralPath $ppm -Algorithm SHA256).Hash.ToLowerInvariant()
    }

    [pscustomobject]@{
        label                 = $Label
        arm                   = $Arm
        cpu_before            = $cpuBefore
        cpu_after             = $cpuAfter
        flagged               = ($cpuBefore -gt 8 -or $cpuAfter -gt 8)
        contaminated          = ($cpuBefore -gt 25 -or $cpuAfter -gt 25)
        wall_s                = [double](Get-Field $report "wall_seconds")
        stopwatch_s           = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s               = Get-Field $report "guest_seconds"
        insns                 = Get-Field $perf "instructions"
        core_clocks           = Get-Field $report "executed_cpu_core_clocks"
        raw_bus               = Get-Field $report "raw_bus_clocks"
        scaled_bus            = Get-Field $report "scaled_bus_clocks"
        master_ticks          = Get-Field $report "master_ticks"
        jit_direct_insns      = Get-Field $perf "jit_direct_insns"
        jit_direct_entries    = Get-Field $perf "jit_direct_entries"
        align_test_al_sites   = [uint64]$sites
        frame_sha256          = $hash
    }
}

function Invoke-LegUntilClean([string]$Arm, [string]$Label) {
    $discards = 0
    while ($true) {
        $leg = Invoke-Leg $Arm $Label
        if (-not $leg.contaminated) { return @{ leg = $leg; discards = $discards } }
        $discards++
        $stamp = Get-Date -Format "HHmmss"
        Write-Host ("CONTAMINATED {0} cpu_before={1} cpu_after={2} (re-run bar 25); discard #{3}" -f `
                $Label, $leg.cpu_before, $leg.cpu_after, $discards)
        $leg | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutDir "$Label-discard-$stamp.json")
        Start-Sleep -Seconds 2
    }
}

try {
    $legs = [Collections.Generic.List[object]]::new()
    $totalDiscards = 0

    Write-Host "IDENTITY pair (OFF then ON)..."
    $idOff = Invoke-LegUntilClean "0" "identity-off"
    $idOn = Invoke-LegUntilClean "1" "identity-on"
    $totalDiscards += $idOff.discards + $idOn.discards
    $off = $idOff.leg
    $on = $idOn.leg
    $legs.Add($off)
    $legs.Add($on)

    $identityOk = $true
    $identityNotes = [Collections.Generic.List[string]]::new()
    if ($off.frame_sha256 -ne $expectedHash) {
        $identityOk = $false
        $identityNotes.Add("OFF frame hash $($off.frame_sha256) != pin $expectedHash")
    }
    if ($on.frame_sha256 -ne $off.frame_sha256) {
        $identityOk = $false
        $identityNotes.Add("ON frame hash $($on.frame_sha256) != OFF $($off.frame_sha256)")
    }
    foreach ($field in @("insns", "core_clocks", "raw_bus", "scaled_bus", "master_ticks")) {
        if ($off.$field -ne $on.$field) {
            $identityOk = $false
            $identityNotes.Add("$field OFF=$($off.$field) ON=$($on.$field)")
        }
    }
    $occupancyNotes = [Collections.Generic.List[string]]::new()
    foreach ($field in @("jit_direct_insns", "jit_direct_entries")) {
        if ($off.$field -ne $on.$field) {
            $occupancyNotes.Add("$field OFF=$($off.$field) ON=$($on.$field)")
        }
    }
    if ([uint64]$off.align_test_al_sites -ne 0) {
        $identityOk = $false
        $identityNotes.Add("OFF align_test_al_sites=$($off.align_test_al_sites) (must be 0)")
    }
    if ([uint64]$on.align_test_al_sites -eq 0) {
        $identityOk = $false
        $identityNotes.Add("ON align_test_al_sites=0 (vacuous; knob did not engage)")
    }

    Write-Host ("IDENTITY OFF wall={0:N3}s ON wall={1:N3}s sites OFF={2} ON={3} hash={4}" -f `
            $off.wall_s, $on.wall_s, $off.align_test_al_sites, $on.align_test_al_sites, $off.frame_sha256)
    if ($occupancyNotes.Count -gt 0) {
        Write-Host "OCCUPANCY (not a STOP if hash and clocks hold):"
        $occupancyNotes | ForEach-Object { Write-Host "  $_" }
    }
    if (-not $identityOk) {
        $identityNotes | ForEach-Object { Write-Host "IDENTITY FAIL: $_" }
        throw "Identity gate failed. See $OutDir. Not running the wall ladder."
    }
    Write-Host "IDENTITY PASS"

    if ($IdentityOnly) {
        $summary = [pscustomobject]@{ identity = "pass"; discards = $totalDiscards; occupancy = $occupancyNotes; off = $off; on = $on }
        $summary | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "identity.json")
        return
    }

    Write-Host "WALL ladder: $Pairs ABBA pairs on wolf3d-586 (report only; NEUTRAL does not decline)"
    $pairResults = [Collections.Generic.List[object]]::new()
    for ($p = 1; $p -le $Pairs; $p++) {
        $pairLegs = [Collections.Generic.List[object]]::new()
        foreach ($step in @(@("0", "off"), @("1", "on"), @("1", "on"), @("0", "off"))) {
            $arm, $role = $step
            $label = "p$p-$role-$([guid]::NewGuid().ToString('N').Substring(0, 6))"
            Write-Host "pair $p / $role ($arm) ..."
            $ran = Invoke-LegUntilClean $arm $label
            $totalDiscards += $ran.discards
            $leg = $ran.leg
            $leg | Add-Member -NotePropertyName pair -NotePropertyValue $p
            $leg | Add-Member -NotePropertyName role -NotePropertyValue $role
            $legs.Add($leg)
            $pairLegs.Add($leg)
            Write-Host ("  wall {0,9:N3}s  sites {1}  cpu {2}/{3}" -f `
                    $leg.wall_s, $leg.align_test_al_sites, $leg.cpu_before, $leg.cpu_after)
        }
        $offWalls = @($pairLegs | Where-Object { $_.arm -eq "0" } | ForEach-Object { $_.wall_s })
        $onWalls = @($pairLegs | Where-Object { $_.arm -eq "1" } | ForEach-Object { $_.wall_s })
        $offMin = ($offWalls | Measure-Object -Minimum).Minimum
        $onMin = ($onWalls | Measure-Object -Minimum).Minimum
        $onWins = $onMin -lt $offMin
        $pairResults.Add([pscustomobject]@{
                pair    = $p
                off_min = $offMin
                on_min  = $onMin
                ratio   = $offMin / $onMin
                on_wins = $onWins
            })
        Write-Host ("pair {0}: OFF min {1:N3} ON min {2:N3} ratio {3:N4} {4}" -f `
                $p, $offMin, $onMin, ($offMin / $onMin), $(if ($onWins) { "ON" } else { "OFF" }))
    }

    $offAll = @($legs | Where-Object { $_.arm -eq "0" -and $_.label -notlike "identity-*" } | ForEach-Object { $_.wall_s })
    $onAll = @($legs | Where-Object { $_.arm -eq "1" -and $_.label -notlike "identity-*" } | ForEach-Object { $_.wall_s })
    $offMinAll = ($offAll | Measure-Object -Minimum).Minimum
    $onMinAll = ($onAll | Measure-Object -Minimum).Minimum
    $offMean = ($offAll | Measure-Object -Average).Average
    $onMean = ($onAll | Measure-Object -Average).Average
    $minRatio = $offMinAll / $onMinAll
    $meanRatio = $offMean / $onMean
    $signWins = @($pairResults | Where-Object { $_.on_wins }).Count
    $signN = $pairResults.Count

    $label = if ($minRatio -ge 1.060 -and $signWins -eq $signN) {
        "WALL_LARGE (default still OFF unless owner flips)"
    }
    elseif ($minRatio -ge 1.020 -and $signWins -ge 7) {
        "WALL_REAL_AND_SMALL (keep default OFF)"
    }
    else {
        "WALL_NEUTRAL (keep default OFF; density already passed)"
    }

    $summary = [pscustomobject]@{
        identity_pass      = $true
        occupancy_notes    = $occupancyNotes
        pairs              = $signN
        sign_on_wins       = $signWins
        min_wall_off       = $offMinAll
        min_wall_on        = $onMinAll
        min_wall_ratio     = $minRatio
        mean_wall_off      = $offMean
        mean_wall_on       = $onMean
        mean_wall_ratio    = $meanRatio
        discards           = $totalDiscards
        pairing            = "index-matched-within-arm in pair order; ON wins if pair ON min < pair OFF min"
        expected_hash      = $expectedHash
        wall_label         = $label
        merge_bar          = "identity + Guard 3 + density; wall is report-only"
        pair_rows          = $pairResults
        identity_off       = $off
        identity_on        = $on
    }
    $summary | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutDir "GATE.json")
    $legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")

    Write-Host ""
    Write-Host ("GATE 1 sign {0}/{1}  min-wall ratio {2:N4}  mean ratio {3:N4}  discards {4}" -f `
            $signWins, $signN, $minRatio, $meanRatio, $totalDiscards)
    Write-Host "WALL: $label"
    Write-Host "MERGE BAR: identity + Guard 3 + density (already green). Wall does not decline."
}
finally {
    if (Test-Path -LiteralPath $lockPath) {
        $held = Get-Content -LiteralPath $lockPath -Raw
        if ($held -match "^$PID\b") { Remove-Item -LiteralPath $lockPath -Force }
    }
}
