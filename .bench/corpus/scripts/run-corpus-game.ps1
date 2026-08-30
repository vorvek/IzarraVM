# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#
# Run one game from the Neville collection under IzarraVM and profile it.
#
# The fixture manifest points at the network drive. The runner copies the game
# to a local scratch tree per run, because games write to their own trees and
# the collection must stay unmodified. The scratch tree dies after the run
# unless -KeepScratch is set.
#
# Recipe files live in .bench/corpus/games/<slug>.json. A recipe records the
# entry command, the persona, the schedules and the notes for one game, so any
# session can replay the run. -Recipe loads one; explicit parameters override
# its fields.

param(
    [Parameter(Mandatory)][string]$Game,
    [ValidateSet('486', '586')][string]$Cpu = '586',
    [double]$GuestSeconds = 120,
    [string]$Entry,
    [string]$InjectKeys,
    [string]$InjectMouse,
    [string]$Label = 'probe',
    [int]$MemoryMiB = 64,
    [string]$Video = 'vega',
    [int]$PhaseMs = 1000,
    [string]$CdImage,
    [string]$Exe = 'D:\ctd\cep\release\izarravm.exe',
    [string]$Dosroot = 'R:\La Colección by Neville\dosroot',
    [string]$Recipe,
    [int]$ScreenDumpMs = 0,
    [switch]$BarrierCensus,
    [string[]]$ConfigExtra = @(),
    [switch]$NoLoop,
    [switch]$NoEmm,
    [switch]$Mouse,
    [string[]]$EmuExtra = @(),
    [switch]$KeepScratch
)

$ErrorActionPreference = 'Stop'

if ($Recipe) {
    $r = Get-Content -Raw $Recipe | ConvertFrom-Json
    if (-not $PSBoundParameters.ContainsKey('Cpu') -and $r.cpu) { $Cpu = $r.cpu }
    if (-not $PSBoundParameters.ContainsKey('GuestSeconds') -and $r.guest_seconds) { $GuestSeconds = $r.guest_seconds }
    if (-not $Entry -and $r.entry) { $Entry = $r.entry }
    if (-not $InjectKeys -and $r.inject_keys) { $InjectKeys = $r.inject_keys }
    if (-not $InjectMouse -and $r.inject_mouse) { $InjectMouse = $r.inject_mouse }
    if (-not $CdImage -and $r.cd_image) { $CdImage = $r.cd_image }
    if ($ConfigExtra.Count -eq 0 -and $r.config_extra) { $ConfigExtra = @($r.config_extra) }
    if (-not $PSBoundParameters.ContainsKey('NoLoop') -and $r.no_loop) { $NoLoop = $true }
    if (-not $PSBoundParameters.ContainsKey('Mouse') -and $r.mouse_driver) { $Mouse = $true }
}

$gameSource = Join-Path $Dosroot $Game
if (-not (Test-Path -LiteralPath $gameSource)) { throw "Game folder is missing: $gameSource" }
if (-not (Test-Path -LiteralPath $Exe)) { throw "Emulator binary is missing: $Exe" }

# Slug: ASCII lower case, runs of other characters become one dash.
$slug = ($Game.ToLowerInvariant() -replace '[^a-z0-9]+', '-').Trim('-')
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$corpusRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$resultDir = Join-Path $corpusRoot "results\$slug\$stamp-$Label"
New-Item -ItemType Directory -Force $resultDir | Out-Null

# Scratch on a short local path: the network drive is slow and the worktree
# paths are long enough to break MAX_PATH-unaware DOS trees.
$scratch = "D:\ctd\corpus-scratch\$slug"
if (Test-Path -LiteralPath $scratch) { Remove-Item -Recurse -Force -LiteralPath $scratch }
New-Item -ItemType Directory -Force (Join-Path $scratch 'GAME') | Out-Null
robocopy $gameSource (Join-Path $scratch 'GAME') /E /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy failed with exit code $LASTEXITCODE" }

# Entry resolution: an explicit entry, else GAME.BAT, else the only executable.
if (-not $Entry) {
    $files = Get-ChildItem -LiteralPath (Join-Path $scratch 'GAME') -File
    $bat = $files | Where-Object { $_.Name -ieq 'game.bat' }
    if ($bat) {
        $Entry = 'CALL GAME.BAT'
    } else {
        $exes = @($files | Where-Object { $_.Extension -match '^\.(exe|com|bat)$' })
        if ($exes.Count -eq 1) { $Entry = $exes[0].Name }
        else { throw "No GAME.BAT and $($exes.Count) executables. Pass -Entry. Candidates: $($exes.Name -join ', ')" }
    }
}

$configLines = @('FILES=40', 'LASTDRIVE=D')
if (-not $NoEmm) { $configLines += 'DEVICE=C:\DOS\TOKAEMM.SYS' }
$configLines += 'DOS=HIGH,UMB'
$configLines += $ConfigExtra + @(
    'SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT'
)
Set-Content -LiteralPath (Join-Path $scratch 'CONFIG.SYS') -Value ($configLines -join "`r`n") -Encoding ascii

$autoexecLines = @('@echo off')
if ($Mouse) { $autoexecLines += 'C:\DOS\TOKAMOUS.COM' }
$autoexecLines += 'cd \GAME'
if ($NoLoop) {
    $autoexecLines += $Entry
} else {
    $autoexecLines += @(':loop', $Entry, 'goto loop')
}
Set-Content -LiteralPath (Join-Path $scratch 'AUTOEXEC.BAT') -Value ($autoexecLines -join "`r`n") -Encoding ascii

$rate = if ($Cpu -eq '486') { 66e6 } else { 166e6 }
$cycles = [uint64]($GuestSeconds * $rate)

$emuArgs = @(
    '--cpu', $Cpu
    '--memory-mib', $MemoryMiB
    '--video', $Video
    '--hdd-folder', $scratch
    '--cycles', $cycles
    '--profile-json', (Join-Path $resultDir 'profile.json')
    '--presented-ppm', (Join-Path $resultDir 'end-frame.ppm')
)
if ($InjectKeys) { $emuArgs += @('--inject-keys', $InjectKeys) }
if ($InjectMouse) { $emuArgs += @('--inject-mouse', $InjectMouse) }
if ($CdImage) { $emuArgs += @('--cd-image', $CdImage) }
if ($EmuExtra.Count -gt 0) { $emuArgs += $EmuExtra }
if ($ScreenDumpMs -gt 0) {
    # Slices the run. A diagnostic for steering key schedules, never a benchmark.
    $dumpDir = Join-Path $resultDir 'screens'
    New-Item -ItemType Directory -Force $dumpDir | Out-Null
    $emuArgs += @('--screen-dump-dir', $dumpDir, '--screen-dump-interval-ms', $ScreenDumpMs)
}

$meta = [ordered]@{
    game          = $Game
    game_source   = $gameSource
    slug          = $slug
    cpu           = $Cpu
    guest_seconds = $GuestSeconds
    cycles        = $cycles
    entry         = $Entry
    inject_keys   = $InjectKeys
    inject_mouse  = $InjectMouse
    cd_image      = $CdImage
    memory_mib    = $MemoryMiB
    video         = $Video
    phase_ms      = $PhaseMs
    config_extra  = $ConfigExtra
    no_loop       = [bool]$NoLoop
    exe           = $Exe
    exe_sha256    = (Get-FileHash -Algorithm SHA256 -LiteralPath $Exe).Hash.ToLowerInvariant()
    barrier_census = [bool]$BarrierCensus
    label         = $Label
}
$meta | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $resultDir 'run-meta.json') -Encoding utf8

$env:IZARRAVM_PHASE_INTERVAL_MS = "$PhaseMs"
if ($BarrierCensus) { $env:IZARRAVM_DIRECT_BARRIER_CENSUS = '1' }
$wall = [Diagnostics.Stopwatch]::StartNew()
& $Exe @emuArgs *> (Join-Path $resultDir 'emulator.log')
$exit = $LASTEXITCODE
$wall.Stop()
Remove-Item Env:IZARRAVM_PHASE_INTERVAL_MS
if ($BarrierCensus) { Remove-Item Env:IZARRAVM_DIRECT_BARRIER_CENSUS }

"exit=$exit wall_s=$([math]::Round($wall.Elapsed.TotalSeconds, 1))" |
    Set-Content -LiteralPath (Join-Path $resultDir 'outcome.txt') -Encoding ascii

if (-not $KeepScratch) { Remove-Item -Recurse -Force -LiteralPath $scratch }

Write-Output "result_dir=$resultDir"
Write-Output "exit=$exit wall_s=$([math]::Round($wall.Elapsed.TotalSeconds, 1))"
$profilePath = Join-Path $resultDir 'profile.json'
if (Test-Path -LiteralPath $profilePath) {
    $p = Get-Content -Raw $profilePath | ConvertFrom-Json
    $rt = [math]::Round($p.real_time_factor, 3)
    $cov = [math]::Round($p.direct_native_coverage, 4)
    Write-Output "rt=$rt native_coverage=$cov guest_seconds=$($p.guest_seconds) stop=$($p.stop.reason)"
} else {
    Write-Output 'WARNING: no profile.json was written'
}
