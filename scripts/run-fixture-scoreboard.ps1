# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
One pass over every game fixture, reporting real-time factor and the JIT
counters beside each fixture's correctness invariant.

.DESCRIPTION
The formal gate (run-realtime-gate.ps1) compares a candidate against a pinned
baseline over three workloads and takes the better part of an hour. This is the
other instrument: one run per fixture, no pairing, no baseline, about half an
hour for the whole set. It answers "where does every workload sit right now",
which the gate cannot, because the gate only knows Doom and Quake.

Each fixture is invoked with the EXACT arguments recorded for it in
.bench/PROTOCOL.md. That is not a style choice: the framebuffer hashes below
were measured under those arguments, so changing a persona, a memory size or a
video card silently invalidates the invariant rather than failing loudly.

Real-time factor is guest seconds per wall second. 1.0 is real time, higher is
faster than the machine being emulated.

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -Label before-slice

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -Fixtures doom-486,wolf3d-486 -Label quick
#>

param(
    [string]$Executable = "target/release/izarravm.exe",
    [string[]]$Fixtures = @(),
    [string]$Label = "",
    [string]$ResultsDirectory = "",
    [int]$ProcessorIndex = -1,
    [int]$HostTimeoutSeconds = 1800,
    # Which JIT arm to drive. Both flags are read from the environment, so ONE
    # binary runs both arms and a comparison carries no build-to-build or
    # build-path-length variance at all.
    #
    #   on       IZARRAVM_JIT16=1  IZARRAVM_JIT16_486=1   (the shipped default)
    #   off      IZARRAVM_JIT16=0  IZARRAVM_JIT16_486=0   (pre-flip behaviour)
    #   jit16    IZARRAVM_JIT16=1  IZARRAVM_JIT16_486=0   (16-bit half alone)
    #   word486  IZARRAVM_JIT16=0  IZARRAVM_JIT16_486=1   (32-bit half alone)
    #
    # Both are set explicitly on every arm, never left to inherit. An empty
    # IZARRAVM_JIT16 parses as u8 and falls back to 1, so "unset it to turn it
    # off" is wrong in the dangerous direction, and IZARRAVM_JIT16_486 is on for
    # every value except exactly "0".
    [ValidateSet("on", "off", "jit16", "word486")]
    [string]$Arm = "on",
    # The one-lookup store emission arm (dev_docs/2026-08-07-one-lookup-store-design.md D8):
    # "1" is the shipped default, "0" restores the classic classify/resolve store emission.
    # Set explicitly on every run for the same inherit-hazard reason as the JIT16 pair —
    # IZARRAVM_ONE_LOOKUP_STORE is on for every value except exactly "0", so a stray "0" left
    # in the caller's environment would silently turn an "on" observation into an "off" one.
    [ValidateSet("1", "0")]
    [string]$OneLookupStore = "1",
    # The one-lookup LOAD emission arm (dev_docs/2026-08-07-one-lookup-load-design.md D7):
    # same contract and the same inherit hazard as the store knob above; independent of it so
    # either slice A/Bs alone.
    [ValidateSet("1", "0")]
    [string]$OneLookupLoad = "1",
    [switch]$RecordInvariants,
    [switch]$Force,
    [switch]$ListFixtures
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$invariantPath = Join-Path $PSScriptRoot "fixture-scoreboard-invariants.json"

# A busy host inflates wall and therefore deflates rt. The number below is a
# whole-machine percentage with the emulator's OWN consumption already
# subtracted, so it is genuinely other people's work.
#
# Calibrated on this host 2026-08-06: resting load with the owner's usual tray
# software running (Stream Deck, Epic, GOG Galaxy) measures about 17.7%, so an
# earlier 12% threshold marked every observation contaminated and was useless.
# 30% leaves headroom over resting while still catching a build, a render or a
# Defender sweep. Re-measure this if the host's resting set changes.
#
# A contaminated row still carries valid deterministic counters; it is only the
# wall and rt figures that get quarantined.
$maximumBackgroundLoadPercent = 30.0

# ---------------------------------------------------------------------------
# The fixture table. Arguments are copied from .bench/PROTOCOL.md; see the note
# in the .DESCRIPTION above about why they are copied rather than normalised.
# ---------------------------------------------------------------------------

function Get-FixtureTable {
    @(
        [pscustomobject]@{
            name = "doom-486"; folder = "jemmex_doom_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]8000000000
            # Guest-reported, so robust to host noise. LOWER realtics is faster.
            realticsMinimum = 2900; realticsMaximum = 3050; gametics = 2134
            qconsole = $false; resultPpm = $false; injection = @()
        }
        [pscustomobject]@{
            name = "doom-586"; folder = "jemmex_doom_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]6640000000
            realticsMinimum = 970; realticsMaximum = 1040; gametics = 2134
            qconsole = $false; resultPpm = $false; injection = @()
        }
        [pscustomobject]@{
            name = "quake-586"; folder = "quake_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]6200000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            # QCONSOLE.LOG is the invariant. perf.instructions is NOT one: the
            # demo finishes before the budget and the run stops in an idle tail
            # whose length moves with the timing model.
            qconsole = $true; resultPpm = $false; injection = @()
        }
        [pscustomobject]@{
            name = "prince-486"; folder = "prince_c"
            # 486 for cost, not compatibility. A 1989 game does not need 166 MHz,
            # and at 66 MHz the same guest time costs a third of the cycles.
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]4000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true
            # Six Shifts to reach level 1, then right HELD so he runs instead of
            # standing. A bare {right} is a tap and leaves him standing.
            injection = @("--inject-keys", ("400000000:{shift};600000000:{shift};" +
                "800000000:{shift};1000000000:{shift};1200000000:{shift};" +
                "1400000000:{shift};1600000000:{+right}"))
        }
        [pscustomobject]@{
            name = "wolf3d-486"; folder = "wolf3d_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]4000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @()
        }
        [pscustomobject]@{
            name = "wolf3d-586"; folder = "wolf3d_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]3320000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @()
        }
        [pscustomobject]@{
            name = "duke3d-486"; folder = "duke3d_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]7920000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @()
        }
        [pscustomobject]@{
            name = "duke3d-586"; folder = "duke3d_c"
            # The most expensive fixture in the set at roughly 12 minutes, and
            # currently the one furthest below real time, which is why it is the
            # workload the campaign's merge rule protects.
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]19920000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @()
        }
        [pscustomobject]@{
            name = "nascar-586"; folder = "nascar1_c"
            # No --video: PROTOCOL.md's recorded invocation omits it and the
            # invariant hash was measured that way.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]4980000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @()
        }
        [pscustomobject]@{
            name = "gp2-586"; folder = "gp2_c"
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]13280000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true
            # Three clicks: credits OK, Quickrace, Select Circuit OK. GP2 sets
            # its own INT 33h ratio and is 1 pixel per mickey on BOTH axes,
            # which is NOT the TOKAMOUS default.
            injection = @("--inject-mouse", ("3320000000:home;3652000000:move:320,386;" +
                "3984000000:click;4648000000:move:0,-115;5146000000:click;" +
                "5976000000:move:-273,181;6474000000:click"))
        }
    )
}

if ($ListFixtures) {
    Get-FixtureTable | Select-Object name, folder, cycles | Format-Table -AutoSize
    return
}

# ---------------------------------------------------------------------------
# Host load sampling
# ---------------------------------------------------------------------------

$logicalProcessorCount = [int](Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors

function Get-TotalBusySnapshot {
    # Get-Counter's counter paths are localized and this host returns them in
    # Spanish, so read the raw performance data instead of naming a counter.
    $sample = Get-CimInstance Win32_PerfRawData_PerfOS_Processor |
        Where-Object { $_.Name -eq "_Total" }
    return [pscustomobject]@{
        idle  = [double]$sample.PercentIdleTime
        stamp = [double]$sample.Timestamp_Sys100NS
    }
}

function Get-BusyPercentBetween($First, $Second) {
    $stampDelta = $Second.stamp - $First.stamp
    if ($stampDelta -le 0) { return 0.0 }
    $busyFraction = 1.0 - (($Second.idle - $First.idle) / $stampDelta)
    return [math]::Max(0.0, $busyFraction) * 100.0
}

<#
Wait for the emulator, sampling host load WHILE it runs, and return the median
background load with the emulator's own consumption removed.

Sampling after the run finished was the first attempt and it was wrong twice
over: it caught Defender chewing through the robocopy that had just happened
rather than anything present during the measurement, and it counted the
emulator's own core as background noise. What matters for a wall number is what
ELSE was competing during the run.

The subtraction is exact rather than assumed. The emulator's own processor time
over each interval is converted to a whole-machine percentage the same way the
total is, so what remains is genuinely other people's work.
#>
function Wait-WithLoadSampling($Process, [int]$TimeoutSeconds, [int]$IntervalMilliseconds = 4000) {
    $samples = @()
    $deadline = [datetime]::UtcNow.AddSeconds($TimeoutSeconds)
    $previousTotal = Get-TotalBusySnapshot
    $previousProcessCpu = [double]0
    try { $previousProcessCpu = $Process.TotalProcessorTime.TotalMilliseconds } catch { }
    $previousAt = [datetime]::UtcNow

    while (-not $Process.HasExited) {
        if ([datetime]::UtcNow -gt $deadline) {
            return [pscustomobject]@{ timedOut = $true; samples = $samples }
        }
        if ($Process.WaitForExit($IntervalMilliseconds)) { break }

        $currentTotal = Get-TotalBusySnapshot
        $currentAt = [datetime]::UtcNow
        $currentProcessCpu = $previousProcessCpu
        try { $currentProcessCpu = $Process.TotalProcessorTime.TotalMilliseconds } catch { }

        $totalBusy = Get-BusyPercentBetween $previousTotal $currentTotal
        $elapsedMs = ($currentAt - $previousAt).TotalMilliseconds
        $ownBusy = if ($elapsedMs -gt 0 -and $logicalProcessorCount -gt 0) {
            100.0 * ($currentProcessCpu - $previousProcessCpu) /
                ($elapsedMs * $logicalProcessorCount)
        } else { 0.0 }

        $samples += [math]::Round([math]::Max(0.0, $totalBusy - $ownBusy), 2)

        $previousTotal = $currentTotal
        $previousProcessCpu = $currentProcessCpu
        $previousAt = $currentAt
    }
    return [pscustomobject]@{ timedOut = $false; samples = $samples }
}

function Get-Median([double[]]$Values) {
    if ($null -eq $Values -or $Values.Count -eq 0) { return 0.0 }
    $sorted = @($Values | Sort-Object)
    $middle = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return [double]$sorted[$middle] }
    return ([double]$sorted[$middle - 1] + [double]$sorted[$middle]) / 2.0
}

# ---------------------------------------------------------------------------
# Fixture copy. Several fixtures mutate their own tree, so every observation
# gets a fresh one and deletes it afterwards.
# ---------------------------------------------------------------------------

function Copy-Fixture([string]$SourcePath, [string]$DestinationPath) {
    if (Test-Path -LiteralPath $DestinationPath) {
        throw "A scoreboard fixture path was reused: $DestinationPath"
    }
    $robocopy = Get-Command robocopy.exe -CommandType Application -ErrorAction Stop
    $output = @(& $robocopy.Source $SourcePath $DestinationPath /E /COPY:DAT /DCOPY:DAT `
        /R:1 /W:1 /NFL /NDL /NJH /NJS /NP 2>&1)
    $code = $LASTEXITCODE
    if ($code -lt 0 -or $code -gt 7) {
        throw "robocopy failed for $SourcePath with code ${code}: $($output -join ' ')"
    }
    if (-not (Test-Path -LiteralPath $DestinationPath -PathType Container)) {
        throw "robocopy did not create $DestinationPath"
    }
}

function Get-FileSha256([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# ---------------------------------------------------------------------------
# Invariants. Held in a sidecar JSON so a legitimate move is a reviewable diff
# rather than an edit buried in a script.
# ---------------------------------------------------------------------------

function Read-Invariants {
    if (-not (Test-Path -LiteralPath $invariantPath)) {
        return [ordered]@{}
    }
    $raw = Get-Content -LiteralPath $invariantPath -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) { return [ordered]@{} }
    # -AsHashtable so entries are addressable by fixture name. Normalised into an
    # OrderedDictionary so membership is `.Contains` everywhere in this script;
    # Hashtable and OrderedDictionary disagree about ContainsKey and mixing them
    # throws only on the branch that has a recorded invariant to compare.
    $parsed = $raw | ConvertFrom-Json -AsHashtable
    if ($null -eq $parsed) { return [ordered]@{} }
    $normalised = [ordered]@{}
    foreach ($key in @($parsed.Keys | Sort-Object)) { $normalised[$key] = $parsed[$key] }
    return $normalised
}

function Write-Invariants($Table) {
    $Table.GetEnumerator() |
        Sort-Object Key |
        ForEach-Object -Begin { $ordered = [ordered]@{} } `
            -Process { $ordered[$_.Key] = $_.Value } `
            -End { $ordered } |
        ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath $invariantPath -Encoding utf8
}

# ---------------------------------------------------------------------------
# One observation
# ---------------------------------------------------------------------------

function Invoke-Fixture($Fixture, [string]$ExecutablePath, [string]$ScratchRoot,
    [string]$KeepProfilesIn) {
    $fixtureSource = Join-Path $repositoryRoot ".bench" $Fixture.folder
    if (-not (Test-Path -LiteralPath $fixtureSource -PathType Container)) {
        throw "Fixture folder is missing: $fixtureSource"
    }

    $stamp = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $workingCopy = Join-Path $ScratchRoot "$($Fixture.name)-$stamp"
    $profilePath = Join-Path $ScratchRoot "$($Fixture.name)-$stamp.json"
    $ppmPath = Join-Path $ScratchRoot "$($Fixture.name)-$stamp.ppm"

    Copy-Fixture $fixtureSource $workingCopy

    # Quake appends to this and the oracle is its LAST line, so a stale file
    # from the source tree would be read as this run's result.
    $staleQuakeLog = Join-Path $workingCopy "QUAKE\ID1\QCONSOLE.LOG"
    if (Test-Path -LiteralPath $staleQuakeLog) {
        Remove-Item -LiteralPath $staleQuakeLog -Force
    }

    $arguments = @()
    $arguments += $Fixture.arguments
    $arguments += @("--hdd-folder", $workingCopy)
    $arguments += @("--cycles", $Fixture.cycles.ToString())
    $arguments += @("--profile-json", $profilePath)
    if ($Fixture.resultPpm) { $arguments += @("--result-ppm", $ppmPath) }
    $arguments += $Fixture.injection

    # Set every variable explicitly. Restoring one that was never set writes an
    # EMPTY STRING rather than removing it, and an empty IZARRAVM_JIT reads as
    # OFF, which has silently turned real observations into interpreter runs.
    # The barrier census stays OFF: it only does work when the JIT is active, so
    # it taxes exactly the runs this is trying to time.
    $armFlags = switch ($Arm) {
        "on"      { @{ jit16 = "1"; word486 = "1" } }
        "off"     { @{ jit16 = "0"; word486 = "0" } }
        "jit16"   { @{ jit16 = "1"; word486 = "0" } }
        "word486" { @{ jit16 = "0"; word486 = "1" } }
    }
    $environment = @{
        "IZARRAVM_JIT16"                 = $armFlags.jit16
        "IZARRAVM_JIT16_486"             = $armFlags.word486
        "IZARRAVM_ONE_LOOKUP_STORE"      = $OneLookupStore
        "IZARRAVM_ONE_LOOKUP_LOAD"       = $OneLookupLoad
        "IZARRAVM_DIRECT_BARRIER_CENSUS" = "0"
        "IZARRAVM_CPU_PROFILE"           = ""
        "IZARRAVM_MACHINE_PROFILE"       = ""
        "IZARRAVM_RIP_PROFILE"           = ""
        "IZARRAVM_PHASE_INTERVAL_MS"     = ""
    }

    $start = @{
        FilePath               = $ExecutablePath
        ArgumentList           = $arguments
        NoNewWindow            = $true
        PassThru               = $true
        RedirectStandardOutput = (Join-Path $ScratchRoot "$($Fixture.name)-$stamp.out")
        RedirectStandardError  = (Join-Path $ScratchRoot "$($Fixture.name)-$stamp.err")
        Environment            = $environment
    }

    $wallStart = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process @start
    if ($ProcessorIndex -ge 0) {
        try {
            $process.ProcessorAffinity = [IntPtr]([int64]1 -shl $ProcessorIndex)
        } catch {
            Write-Warning "Could not pin $($Fixture.name) to processor ${ProcessorIndex}: $_"
        }
    }
    $waited = Wait-WithLoadSampling $process $HostTimeoutSeconds
    if ($waited.timedOut) {
        try { $process.Kill($true) } catch { }
        throw "$($Fixture.name) exceeded $HostTimeoutSeconds seconds."
    }
    $wallStart.Stop()
    $exitCode = $process.ExitCode

    $backgroundLoad = [math]::Round((Get-Median ([double[]]$waited.samples)), 2)
    $peakLoad = if ($waited.samples.Count -gt 0) {
        [math]::Round((($waited.samples | Measure-Object -Maximum).Maximum), 2)
    } else { 0.0 }

    $result = [ordered]@{
        name             = $Fixture.name
        arm              = $Arm
        one_lookup_store = $OneLookupStore
        one_lookup_load  = $OneLookupLoad
        exit_code        = $exitCode
        host_wall_s      = [math]::Round($wallStart.Elapsed.TotalSeconds, 3)
        background_load  = $backgroundLoad
        background_peak  = $peakLoad
        load_samples     = $waited.samples.Count
        contaminated     = $false
        invariant        = "unchecked"
        notes            = @()
    }

    if (-not (Test-Path -LiteralPath $profilePath)) {
        $result.invariant = "no-profile"
        $result.notes += "the run produced no profile JSON; exit code $exitCode"
        Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
        return $result
    }

    $profile = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json
    $perf = $profile.perf
    $entries = [double]$perf.jit_direct_entries
    $entries16 = [double]$perf.jit_direct_entries_sixteen_bit
    $insns16 = [double]$perf.jit_direct_insns_sixteen_bit

    $result.real_time_factor = [math]::Round($profile.real_time_factor, 4)
    $result.guest_seconds = [math]::Round($profile.guest_seconds, 3)
    $result.wall_seconds = [math]::Round($profile.wall_seconds, 3)
    $result.instructions = $perf.instructions
    $result.native_coverage = [math]::Round($profile.direct_native_coverage, 4)
    $result.entries = $perf.jit_direct_entries
    $result.native_insns = $perf.jit_direct_insns
    # The campaign ranks by this, never by coverage. Coverage rising while
    # entries FALL is blocks lengthening; coverage rising with entries flat is
    # more short blocks, which loses.
    $result.insns_per_entry = if ($entries -gt 0) {
        [math]::Round([double]$perf.jit_direct_insns / $entries, 3)
    } else { 0.0 }
    $result.entries_16bit = $perf.jit_direct_entries_sixteen_bit
    $result.insns_16bit = $perf.jit_direct_insns_sixteen_bit
    $result.insns_per_entry_16bit = if ($entries16 -gt 0) {
        [math]::Round($insns16 / $entries16, 3)
    } else { 0.0 }
    $result.stop = $profile.stop

    if ($backgroundLoad -ge $maximumBackgroundLoadPercent) {
        $result.contaminated = $true
        $result.notes += ("background load median {0}% over {1} samples, peak {2}%, threshold {3}%" -f
            $backgroundLoad, $waited.samples.Count, $peakLoad, $maximumBackgroundLoadPercent)
    }

    # --- invariants -------------------------------------------------------
    $failures = @()

    if ($null -ne $Fixture.realticsMinimum) {
        if ($null -eq $profile.timedemo) {
            $failures += "no timedemo line was produced"
        } else {
            $realtics = [int]$profile.timedemo.realtics
            $gametics = [int]$profile.timedemo.gametics
            $result.realtics = $realtics
            $result.gametics = $gametics
            if ($realtics -lt $Fixture.realticsMinimum -or
                $realtics -gt $Fixture.realticsMaximum) {
                $failures += ("realtics {0} outside [{1}, {2}]" -f
                    $realtics, $Fixture.realticsMinimum, $Fixture.realticsMaximum)
            }
            if ($null -ne $Fixture.gametics -and $gametics -ne $Fixture.gametics) {
                $failures += "gametics $gametics is not $($Fixture.gametics)"
            }
        }
    }

    if ($Fixture.qconsole) {
        $logPath = Join-Path $workingCopy "QUAKE\ID1\QCONSOLE.LOG"
        if (-not (Test-Path -LiteralPath $logPath)) {
            $failures += "QCONSOLE.LOG was never written"
        } else {
            $lines = @(Get-Content -LiteralPath $logPath |
                Where-Object { $_ -match "\d+\s+frames" })
            if ($lines.Count -eq 0) {
                $failures += "QCONSOLE.LOG has no timedemo result line"
            } else {
                $result.qconsole = $lines[-1].Trim()
                if ($result.qconsole -notmatch "^969 frames") {
                    $failures += "QCONSOLE result is '$($result.qconsole)', expected 969 frames"
                }
            }
        }
    }

    if ($Fixture.resultPpm) {
        $hash = Get-FileSha256 $ppmPath
        if ($null -eq $hash) {
            $failures += "no result PPM was written"
        } else {
            $result.frame_sha256 = $hash
        }
    }

    $result.invariant = if ($failures.Count -eq 0) { "pass" } else { "FAIL" }
    $result.notes += $failures

    # Keep the RAW profile JSON, not just the handful of fields extracted above.
    # It carries the whole perf block -- fastmap hit/miss, dev_write, the bus and
    # stall counters, direct_stalls with its link and dormant splits -- and which
    # of those matters is never known in advance. An earlier version of this
    # script deleted them with the scratch tree, and answering "why is NASCAR
    # slow" then needed a fresh 5-minute run for data that had already been
    # computed and thrown away.
    if (-not [string]::IsNullOrWhiteSpace($KeepProfilesIn)) {
        Copy-Item -LiteralPath $profilePath `
            -Destination (Join-Path $KeepProfilesIn "$($Fixture.name).json") -Force
    }

    Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
    return $result
}

# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------

$executablePath = [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Executable))
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    $executablePath = [IO.Path]::GetFullPath($Executable)
}
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "Executable not found: $Executable"
}

$table = Get-FixtureTable
if ($Fixtures.Count -gt 0) {
    $known = $table.name
    foreach ($requested in $Fixtures) {
        if ($known -notcontains $requested) {
            throw "Unknown fixture '$requested'. Known: $($known -join ', ')"
        }
    }
    $table = @($table | Where-Object { $Fixtures -contains $_.name })
}

if ([string]::IsNullOrWhiteSpace($ResultsDirectory)) {
    $suffix = if ([string]::IsNullOrWhiteSpace($Label)) { "" } else { "-$Label" }
    $ResultsDirectory = Join-Path $repositoryRoot ".bench/results" `
        ("scoreboard-" + (Get-Date -Format "yyyyMMdd-HHmmss") + "-arm$Arm" + $suffix)
}
New-Item -ItemType Directory -Force -Path $ResultsDirectory | Out-Null

$scratchRoot = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-scoreboard-" +
    [Guid]::NewGuid().ToString("N").Substring(0, 10))
New-Item -ItemType Directory -Force -Path $scratchRoot | Out-Null

$invariants = Read-Invariants
$rows = @()
$profileArchive = Join-Path $ResultsDirectory "profiles"
New-Item -ItemType Directory -Force -Path $profileArchive | Out-Null

try {
    foreach ($fixture in $table) {
        Write-Host ("running {0} ..." -f $fixture.name) -NoNewline
        $row = Invoke-Fixture $fixture $executablePath $scratchRoot $profileArchive

        # Compare or record the framebuffer hash. `$row` is still an ordered
        # hashtable here, so membership is Contains and NOT
        # `PSObject.Properties.Name`, which on a hashtable enumerates Count and
        # Keys rather than the entries and silently answers false for every
        # lookup. That mistake made this whole comparison dead code once already.
        if ($row.Contains("frame_sha256")) {
            $expected = if ($invariants.Contains($fixture.name)) {
                $invariants[$fixture.name].frame_sha256
            } else { $null }

            if ($RecordInvariants) {
                if ($null -ne $expected -and $expected -ne $row.frame_sha256 -and -not $Force) {
                    throw ("$($fixture.name) already has a recorded frame hash and this run " +
                        "disagrees with it. Re-recording would erase the evidence of a real " +
                        "change. Pass -Force only if you have established that the move is " +
                        "legitimate.")
                }
                if (-not $invariants.Contains($fixture.name)) {
                    $invariants[$fixture.name] = @{}
                }
                $invariants[$fixture.name].frame_sha256 = $row.frame_sha256
                $row.notes += "frame hash recorded"
            } elseif ($null -eq $expected) {
                $row.notes += "no recorded frame hash to compare against"
                if ($row.invariant -eq "pass") { $row.invariant = "unpinned" }
            } elseif ($expected -ne $row.frame_sha256) {
                $row.invariant = "FAIL"
                $row.notes += "frame hash moved: expected $expected, got $($row.frame_sha256)"
            }
        }

        $rows += [pscustomobject]$row
        Write-Host ("  {0}  rt {1}  load {2}%{3}" -f $row.invariant,
            $(if ($row.Contains("real_time_factor")) { $row.real_time_factor } else { "n/a" }),
            $row.background_load,
            $(if ($row.contaminated) { "  (CONTAMINATED)" } else { "" }))
    }

    if ($RecordInvariants) { Write-Invariants $invariants }
} finally {
    Remove-Item -LiteralPath $scratchRoot -Recurse -Force -ErrorAction SilentlyContinue
}

$summary = [ordered]@{
    schema           = "izarravm-fixture-scoreboard-v1"
    label            = $Label
    arm              = $Arm
    one_lookup_store = $OneLookupStore
    one_lookup_load  = $OneLookupLoad
    recorded_at      = (Get-Date).ToString("o")
    executable       = $executablePath
    rows             = $rows
}
$jsonPath = Join-Path $ResultsDirectory "scoreboard.json"
$summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $jsonPath -Encoding utf8

$markdown = @()
$markdown += "# Fixture scoreboard$(if ($Label) { ": $Label" })"
$markdown += ""
$markdown += "Recorded $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')), JIT arm ``$Arm``, one-lookup store ``$OneLookupStore``, one-lookup load ``$OneLookupLoad``. rt is guest seconds per wall second; 1.0 is real time."
$markdown += ""
$markdown += "| fixture | rt | wall s | coverage | entries | insns/entry | 16-bit insns/entry | invariant |"
$markdown += "|---|---|---|---|---|---|---|---|"
foreach ($row in $rows) {
    $has = { param($n) $row.PSObject.Properties.Name -contains $n }
    $markdown += ("| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7}{8} |" -f
        $row.name,
        $(if (& $has "real_time_factor") { $row.real_time_factor } else { "-" }),
        $(if (& $has "wall_seconds") { $row.wall_seconds } else { "-" }),
        $(if (& $has "native_coverage") { $row.native_coverage } else { "-" }),
        $(if (& $has "entries") { $row.entries } else { "-" }),
        $(if (& $has "insns_per_entry") { $row.insns_per_entry } else { "-" }),
        $(if (& $has "insns_per_entry_16bit") { $row.insns_per_entry_16bit } else { "-" }),
        $row.invariant,
        $(if ($row.contaminated) { " (contaminated)" } else { "" }))
}
$markdown += ""
foreach ($row in $rows) {
    if ($row.notes.Count -gt 0) {
        $markdown += "* **$($row.name)**: $($row.notes -join '; ')"
    }
}
$markdownPath = Join-Path $ResultsDirectory "scoreboard.md"
$markdown -join "`n" | Set-Content -LiteralPath $markdownPath -Encoding utf8

Write-Host ""
Write-Host ($markdown -join "`n")
Write-Host ""
Write-Host "wrote $jsonPath"

$failed = @($rows | Where-Object { $_.invariant -eq "FAIL" })
if ($failed.Count -gt 0) {
    Write-Host ""
    Write-Error ("{0} fixture(s) failed their invariant: {1}" -f
        $failed.Count, ($failed.name -join ", "))
    exit 1
}
