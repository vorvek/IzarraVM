param(
    [string]$Executable = "target/release/izarravm.exe",
    [string]$DoomFolder = ".bench/jemmex_doom_c",
    [string]$QuakeFolder = ".bench/quake_c",
    [string]$ResultsDirectory = "",
    [int]$Runs = 3,
    [int]$HostTimeoutSeconds = 900,
    [ValidateSet("Both", "Doom", "Quake")]
    [string]$Workload = "Both",
    [ValidateSet("0", "1")]
    [string]$Jit = "1",
    [switch]$SkipBuild,
    [switch]$ReportOnly,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function ConvertFrom-QuakeTimedemoLine([string]$Line) {
    $pattern = '^\s*(?<frames>\d+)\s+frames\s+(?<seconds>\d+(?:\.\d+)?)\s+seconds\s+(?<fps>\d+(?:\.\d+)?)\s+fps\s*$'
    $match = [regex]::Match($Line, $pattern, [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    if (-not $match.Success) {
        return $null
    }
    return [pscustomobject][ordered]@{
        frames = [uint32]::Parse($match.Groups["frames"].Value, [Globalization.CultureInfo]::InvariantCulture)
        seconds = [double]::Parse($match.Groups["seconds"].Value, [Globalization.CultureInfo]::InvariantCulture)
        fps = [double]::Parse($match.Groups["fps"].Value, [Globalization.CultureInfo]::InvariantCulture)
        line = $Line.Trim()
    }
}

function Assert-QuakeAutoexecText([string]$Text) {
    if ($Text -notmatch '(?im)^\s*quake\.exe\b[^\r\n]*\+timedemo\s+demo1(?:\s|$)') {
        throw "The Quake fixture must launch +timedemo demo1."
    }
    if ($Text -match '(?im)^\s*quake\.exe\b[^\r\n]*\+exec\s+bench\.cfg(?:\s|$)') {
        throw "The Quake fixture must not execute bench.cfg; the fixed cycle cap ends the workload."
    }
}

function Read-QuakeTimedemoIdentity([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Quake did not produce QCONSOLE.LOG."
    }
    $identities = @()
    foreach ($line in [IO.File]::ReadLines($Path)) {
        $identity = ConvertFrom-QuakeTimedemoLine $line
        if ($null -ne $identity) {
            $identities += $identity
        }
    }
    if ($identities.Count -ne 1) {
        throw "Quake must produce exactly one timedemo identity line; found $($identities.Count)."
    }
    $identity = $identities[0]
    if ($identity.frames -ne 969 -or $identity.seconds -le 0 -or $identity.fps -le 0) {
        throw "Quake did not complete the 969-frame fixed demo."
    }
    $derivedFps = $identity.frames / $identity.seconds
    if ([Math]::Abs($derivedFps - $identity.fps) -gt 0.2) {
        throw "Quake's timedemo seconds and fps are inconsistent."
    }
    return $identity
}

function Assert-SelfTestThrows([scriptblock]$Action, [string]$MessagePart) {
    try {
        & $Action
    } catch {
        if ($_.Exception.Message -notlike "*$MessagePart*") {
            throw "Unexpected self-test error: $($_.Exception.Message)"
        }
        return
    }
    throw "Self-test expected an error containing '$MessagePart'."
}

if ($SelfTest) {
    $identity = ConvertFrom-QuakeTimedemoLine "969 frames  22.8 seconds  42.6 fps"
    if ($null -eq $identity -or $identity.frames -ne 969 -or
        $identity.seconds -ne 22.8 -or $identity.fps -ne 42.6) {
        throw "The Quake timedemo parser rejected a valid identity."
    }
    if ($null -ne (ConvertFrom-QuakeTimedemoLine "969 frames, 22.8 seconds")) {
        throw "The Quake timedemo parser accepted an invalid identity."
    }
    Assert-QuakeAutoexecText "quake.exe -nosound +timedemo demo1"
    Assert-SelfTestThrows {
        Assert-QuakeAutoexecText "quake.exe -nosound +timedemo demo1 +exec bench.cfg"
    } "must not execute bench.cfg"
    Assert-SelfTestThrows {
        Assert-QuakeAutoexecText "quake.exe -nosound"
    } "must launch +timedemo demo1"
    Write-Host "run-realtime-gate self-test passed"
    return
}

if ($Runs -lt 1) {
    throw "Runs must be at least one."
}
if (-not $ReportOnly -and $Runs -ne 3) {
    throw "The throughput gate requires exactly three clean runs. Use -ReportOnly for ad hoc counts."
}
if (-not $ReportOnly -and $Workload -ne "Both") {
    throw "The throughput gate requires both workloads. Use -ReportOnly for a single workload."
}
if (-not $ReportOnly -and $Jit -ne "1") {
    throw "The throughput gate requires the direct JIT. Use -ReportOnly for a JIT-off control."
}
if (-not $ReportOnly -and $SkipBuild) {
    throw "The throughput gate requires a fresh release build. SkipBuild is diagnostic-only."
}
if ($HostTimeoutSeconds -lt 1) {
    throw "HostTimeoutSeconds must be positive."
}
if ($Workload -in @("Both", "Doom")) {
    if (-not (Test-Path -LiteralPath "$DoomFolder/AUTOEXEC.BAT" -PathType Leaf) -or
        -not (Select-String -LiteralPath "$DoomFolder/AUTOEXEC.BAT" -SimpleMatch "C:\EXITVM.COM" -Quiet)) {
        throw "The Doom fixture must run C:\EXITVM.COM after the timedemo."
    }
}
if ($Workload -in @("Both", "Quake")) {
    $quakeAutoexec = "$QuakeFolder/AUTOEXEC.BAT"
    if (-not (Test-Path -LiteralPath $quakeAutoexec -PathType Leaf)) {
        throw "The Quake fixture needs AUTOEXEC.BAT."
    }
    Assert-QuakeAutoexecText (Get-Content -LiteralPath $quakeAutoexec -Raw)
}

if (-not $SkipBuild) {
    & cargo build --release -p izarravm -j 8
    if ($LASTEXITCODE -ne 0) {
        throw "The release build failed."
    }
}
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "Release executable not found at $Executable."
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ($Workload -in @("Both", "Doom")) {
    $DoomFolder = (Resolve-Path -LiteralPath $DoomFolder).Path
}
if ($Workload -in @("Both", "Quake")) {
    $QuakeFolder = (Resolve-Path -LiteralPath $QuakeFolder).Path
}

$revision = (& git rev-parse --verify HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($revision)) {
    throw "Unable to read the Git revision."
}
$shortRevision = $revision.Substring(0, 12)
$statusLines = @(& git status --porcelain --untracked-files=normal)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to read the Git worktree state."
}
$diffText = @(& git diff --no-ext-diff HEAD) -join "`n"
if ($LASTEXITCODE -ne 0) {
    throw "Unable to read the tracked worktree diff."
}
$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
    $diffBytes = [System.Text.Encoding]::UTF8.GetBytes($diffText)
    $diffHash = ([BitConverter]::ToString($sha256.ComputeHash($diffBytes))).Replace("-", "").ToLowerInvariant()
} finally {
    $sha256.Dispose()
}
$executableHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()

if ([string]::IsNullOrWhiteSpace($ResultsDirectory)) {
    $stamp = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss")
    $ResultsDirectory = ".bench/results/$shortRevision-$stamp"
}
New-Item -ItemType Directory -Path $ResultsDirectory -Force | Out-Null
$ResultsDirectory = (Resolve-Path -LiteralPath $ResultsDirectory).Path

function Get-Median([double[]]$Values) {
    $ordered = @($Values | Sort-Object)
    $middle = [Math]::Floor($ordered.Count / 2)
    if ($ordered.Count % 2 -eq 1) {
        return $ordered[$middle]
    }
    return ($ordered[$middle - 1] + $ordered[$middle]) / 2.0
}

function Get-FiniteNumber($Value, [string]$Name) {
    if ($null -eq $Value -or $Value -is [bool] -or $Value -isnot [ValueType]) {
        throw "Profile field '$Name' is missing or is not numeric."
    }
    try {
        $number = [Convert]::ToDouble($Value, [Globalization.CultureInfo]::InvariantCulture)
    } catch {
        throw "Profile field '$Name' is not a supported numeric value."
    }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
        throw "Profile field '$Name' is not finite."
    }
    return $number
}

function Assert-NearlyEqual([double]$Actual, [double]$Expected, [string]$Name) {
    $tolerance = 1.0e-9 * [Math]::Max(1.0, [Math]::Abs($Expected))
    if ([Math]::Abs($Actual - $Expected) -gt $tolerance) {
        throw "Profile field '$Name' does not match its raw counters."
    }
}

function Get-BytesSha256([byte[]]$Bytes) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-WorkloadInputHashes([string]$Root, [string[]]$RelativePaths) {
    $hashes = [ordered]@{}
    foreach ($relativePath in $RelativePaths) {
        $path = Join-Path $Root $relativePath
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required workload input not found: $path"
        }
        $key = $relativePath.Replace("\", "/")
        $hashes[$key] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    return $hashes
}

function Quote-ProcessArgument([string]$Value) {
    if ($Value -notmatch '[\s"]') {
        return $Value
    }
    return '"' + ($Value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-IzarraProcess([string[]]$Arguments, [string]$StdoutPath, [string]$StderrPath) {
    $argumentLine = ($Arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
    $childEnvironment = @{ IZARRAVM_JIT = $Jit }
    foreach ($name in $diagnosticVariables) {
        $childEnvironment[$name] = $null
    }
    $start = @{
        FilePath = $Executable
        ArgumentList = $argumentLine
        RedirectStandardOutput = $StdoutPath
        RedirectStandardError = $StderrPath
        WindowStyle = "Hidden"
        PassThru = $true
    }
    if ((Get-Command Start-Process).Parameters.ContainsKey("Environment")) {
        $start.Environment = $childEnvironment
    }
    $process = Start-Process @start
    # Windows PowerShell can discard the native process handle after a fast child exit unless it
    # is materialized while the process is live. Keep it so ExitCode remains available below.
    $null = $process.Handle
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while (-not $process.WaitForExit(1000)) {
        foreach ($path in @($StdoutPath, $StderrPath)) {
            if ((Test-Path -LiteralPath $path -PathType Leaf) -and
                (Get-Item -LiteralPath $path).Length -gt 64MB) {
                Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                throw "IzarraVM produced more than 64 MiB of diagnostic output."
            }
        }
        if ($watch.Elapsed.TotalSeconds -ge $HostTimeoutSeconds) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            throw "IzarraVM exceeded the $HostTimeoutSeconds second host timeout."
        }
    }
    $process.WaitForExit()
    $process.Refresh()
    if ($null -eq $process.ExitCode) {
        throw "IzarraVM exited without a readable process exit code."
    }
    return [int]$process.ExitCode
}

$exitVmBytes = [byte[]](
    0xB0, 0x0C, 0xE6, 0xE4,
    0xB0, 0x00, 0xE6, 0xE5,
    0xB0, 0x03, 0xE6, 0xE6,
    0xF4, 0xEB, 0xFD
)
$gateScriptHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
$exitVmHash = Get-BytesSha256 $exitVmBytes
$workloadInputHashes = [ordered]@{}
if ($Workload -in @("Both", "Doom")) {
    $workloadInputHashes.doom_486 = Get-WorkloadInputHashes $DoomFolder @(
        "AUTOEXEC.BAT",
        "CONFIG.SYS",
        "JEMMEX.EXE",
        "DOOM/DOOM.EXE",
        "DOOM/DOOM1.WAD",
        "DOOM/MAX.CFG"
    )
}
if ($Workload -in @("Both", "Quake")) {
    $workloadInputHashes.quake_586 = Get-WorkloadInputHashes $QuakeFolder @(
        "AUTOEXEC.BAT",
        "CONFIG.SYS",
        "QUAKE/CWSDPMI.EXE",
        "QUAKE/QUAKE.EXE",
        "QUAKE/ID1/CONFIG.CFG",
        "QUAKE/ID1/PAK0.PAK"
    )
}
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-gate-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

function Invoke-Workload(
    [string]$Name,
    [string]$Mode,
    [string]$SourceFolder,
    [UInt64]$CycleBudget
) {
    $samples = @()
    for ($run = 1; $run -le $Runs; $run++) {
        $fixture = Join-Path $temporaryRoot "$Name-run$run"
        Copy-Item -LiteralPath $SourceFolder -Destination $fixture -Recurse
        [IO.File]::WriteAllBytes((Join-Path $fixture "EXITVM.COM"), $exitVmBytes)
        $qconsole = Join-Path $fixture "QUAKE/ID1/QCONSOLE.LOG"
        if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
            Remove-Item -LiteralPath $qconsole
        }

        $jsonPath = Join-Path $ResultsDirectory "$Name-run$run.json"
        $stdoutPath = Join-Path $ResultsDirectory "$Name-run$run.stdout.log"
        $stderrPath = Join-Path $ResultsDirectory "$Name-run$run.stderr.log"
        Remove-Item -LiteralPath $jsonPath -Force -ErrorAction SilentlyContinue
        $processArguments = @(
            "--cpu", $Mode,
            "--memory-mib", "24",
            "--video", "vega",
            "--hdd-folder", $fixture,
            "--cycles", $CycleBudget.ToString(),
            "--dump-result",
            "--profile-json", $jsonPath
        )
        if ($Name -eq "doom-486") {
            $processArguments += "--expect-test-exit"
        }
        $exitCode = Invoke-IzarraProcess $processArguments $stdoutPath $stderrPath
        if ($exitCode -ne 0) {
            throw "$Name run $run failed with exit code $exitCode. See $stdoutPath and $stderrPath."
        }
        if (-not (Test-Path -LiteralPath $jsonPath -PathType Leaf)) {
            throw "$Name run $run did not produce its profile JSON."
        }
        $sample = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json
        if ($sample.schema -ne "izarravm-hdd-profile-v1" -or $sample.mode -ne $Mode) {
            throw "$Name run $run produced an unexpected schema or CPU mode."
        }
        if ($Name -eq "doom-486") {
            if ($sample.stop.kind -ne "test_exit" -or $sample.stop.code -ne 0) {
                throw "$Name run $run did not reach TestExit code 0."
            }
        } elseif ($sample.stop.kind -ne "cycle_limit" -or
            [uint64]$sample.stop.requested -ne $CycleBudget) {
            throw "$Name run $run did not reach its fixed cycle limit."
        }
        $wallSeconds = Get-FiniteNumber $sample.wall_seconds "wall_seconds"
        $guestSeconds = Get-FiniteNumber $sample.guest_seconds "guest_seconds"
        $realTimeFactor = Get-FiniteNumber $sample.real_time_factor "real_time_factor"
        $instructionsPerSecond = Get-FiniteNumber $sample.instructions_per_host_second "instructions_per_host_second"
        $directCoverage = Get-FiniteNumber $sample.direct_native_coverage "direct_native_coverage"
        $directExitsPer100 = Get-FiniteNumber $sample.direct_slow_exits_per_100_instructions "direct_slow_exits_per_100_instructions"
        $instructions = Get-FiniteNumber $sample.perf.instructions "perf.instructions"
        $directEntries = Get-FiniteNumber $sample.perf.jit_direct_entries "perf.jit_direct_entries"
        $directInstructions = Get-FiniteNumber $sample.perf.jit_direct_insns "perf.jit_direct_insns"
        $directSideExits = Get-FiniteNumber $sample.perf.jit_direct_side_exits "perf.jit_direct_side_exits"

        if ($wallSeconds -le 0 -or $guestSeconds -le 0 -or $realTimeFactor -le 0 -or
            $instructionsPerSecond -le 0 -or $instructions -le 0) {
            throw "$Name run $run reported a non-positive timing or instruction metric."
        }
        if ($directCoverage -lt 0 -or $directCoverage -gt 1 -or $directExitsPer100 -lt 0 -or
            $directEntries -lt 0 -or $directInstructions -lt 0 -or $directSideExits -lt 0) {
            throw "$Name run $run reported an out-of-range direct JIT metric."
        }
        if ($directInstructions -gt $instructions) {
            throw "$Name run $run retired more direct instructions than total instructions."
        }
        if ($directSideExits -gt $directEntries) {
            throw "$Name run $run reported more direct side exits than direct entries."
        }
        Assert-NearlyEqual $realTimeFactor ($guestSeconds / $wallSeconds) "real_time_factor"
        Assert-NearlyEqual $instructionsPerSecond ($instructions / $wallSeconds) "instructions_per_host_second"
        Assert-NearlyEqual $directCoverage ($directInstructions / $instructions) "direct_native_coverage"
        Assert-NearlyEqual $directExitsPer100 (100.0 * $directSideExits / $instructions) "direct_slow_exits_per_100_instructions"
        if ($Name -eq "doom-486") {
            if ($null -eq $sample.timedemo -or $sample.timedemo.gametics -ne 2134 -or
                $sample.timedemo.realtics -lt 1950 -or $sample.timedemo.realtics -gt 2000) {
                throw "Doom run $run failed its 2134-gametic timing identity check."
            }
        } else {
            $preservedQconsole = Join-Path $ResultsDirectory "$Name-run$run-qconsole.log"
            if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
                Copy-Item -LiteralPath $qconsole -Destination $preservedQconsole -Force
            }
            $quakeIdentity = Read-QuakeTimedemoIdentity $preservedQconsole
            $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
        }
        $samples += $sample
    }

    if (@($samples.perf.instructions | Sort-Object -Unique).Count -ne 1) {
        throw "$Name did not retire a deterministic instruction count across clean runs."
    }
    if ($Name -eq "quake-586" -and
        @($samples.quake_timedemo.line | Sort-Object -Unique).Count -ne 1) {
        throw "Quake did not produce a deterministic timedemo identity across clean runs."
    }

    return [ordered]@{
        name = $Name
        mode = $Mode
        runs = $samples
        median = [ordered]@{
            wall_seconds = Get-Median @($samples.wall_seconds)
            guest_seconds = Get-Median @($samples.guest_seconds)
            real_time_factor = Get-Median @($samples.real_time_factor)
            instructions_per_host_second = Get-Median @($samples.instructions_per_host_second)
            direct_native_coverage = Get-Median @($samples.direct_native_coverage)
            direct_slow_exits_per_100_instructions = Get-Median @($samples.direct_slow_exits_per_100_instructions)
        }
    }
}

$diagnosticVariables = @(
    "IZARRAVM_AUDIO_DEBUG",
    "IZARRAVM_CPU_PROFILE",
    "IZARRAVM_DECODE_CACHE_LINES",
    "IZARRAVM_DIFF_TRACE",
    "IZARRAVM_DUMP_LINEAR",
    "IZARRAVM_FAULT_TRACE",
    "IZARRAVM_JIT_FOLD",
    "IZARRAVM_JIT_REGION",
    "IZARRAVM_MACHINE_PROFILE",
    "IZARRAVM_RUNTIME_PROFILE",
    "RUST_LOG"
)
$savedEnvironment = @{}
foreach ($name in $diagnosticVariables) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    [Environment]::SetEnvironmentVariable($name, $null, "Process")
}
$savedEnvironment["IZARRAVM_JIT"] = [Environment]::GetEnvironmentVariable("IZARRAVM_JIT", "Process")
[Environment]::SetEnvironmentVariable("IZARRAVM_JIT", $Jit, "Process")

try {
    $workloads = @()
    if ($Workload -in @("Both", "Doom")) {
        $workloads += Invoke-Workload "doom-486" "486" $DoomFolder 8000000000
    }
    if ($Workload -in @("Both", "Quake")) {
        $workloads += Invoke-Workload "quake-586" "586" $QuakeFolder 6200000000
    }
} finally {
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
    $resolvedTemporaryRoot = [IO.Path]::GetFullPath($temporaryRoot)
    $resolvedSystemTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ($resolvedTemporaryRoot.StartsWith($resolvedSystemTemp, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$summary = [ordered]@{
    schema = "izarravm-throughput-gate-v1"
    jit = $Jit
    fresh_build = -not $SkipBuild
    revision = $revision
    worktree_dirty = $statusLines.Count -gt 0
    worktree_status = $statusLines
    tracked_diff_sha256 = $diffHash
    executable_sha256 = $executableHash
    gate_script_sha256 = $gateScriptHash
    injected_exitvm_sha256 = $exitVmHash
    workload_inputs_sha256 = $workloadInputHashes
    generated_utc = [DateTime]::UtcNow.ToString("o")
    runs_per_workload = $Runs
    scope = "Headless Doom and Quake throughput only. GUI pacing and audio require separate validation."
    acceptance = [ordered]@{
        minimum_real_time_factor = 1.25
        minimum_direct_native_coverage = 0.90
        maximum_direct_slow_exits_per_100_instructions = 5.0
    }
    workloads = $workloads
}

$summaryPath = Join-Path $ResultsDirectory "summary.json"
$summary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $summaryPath -Encoding utf8

foreach ($workloadResult in $summary.workloads) {
    $median = $workloadResult.median
    Write-Host ("{0}: rt={1:N3} direct-native={2:P2} direct-exits/100={3:N3}" -f `
        $workloadResult.name, $median.real_time_factor, $median.direct_native_coverage, `
        $median.direct_slow_exits_per_100_instructions)
}
Write-Host "Summary: $summaryPath"

if (-not $ReportOnly) {
    $failures = @($summary.workloads | Where-Object {
        $_.median.real_time_factor -lt 1.25 -or
        $_.median.direct_native_coverage -lt 0.90 -or
        $_.median.direct_slow_exits_per_100_instructions -ge 5.0
    })
    if ($failures.Count -gt 0) {
        throw "The fixed-machine throughput gate failed for: $($failures.name -join ', ')."
    }
}
