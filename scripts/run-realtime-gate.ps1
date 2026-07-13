# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

param(
    [string]$Executable = "target/release/izarravm.exe",
    [string]$DoomFolder = ".bench/jemmex_doom_c",
    [string]$QuakeFolder = ".bench/quake_c",
    [string]$BaselineRevision = "",
    [string]$ResultsDirectory = "",
    [int]$Runs = 6,
    [int]$PairSeed = 0,
    [int]$HostTimeoutSeconds = 900,
    [int]$ProcessorIndex = -1,
    [ValidateSet("Both", "Doom", "Doom586", "Quake")]
    [string]$Workload = "Both",
    [ValidateSet("0", "1")]
    [string]$Jit = "1",
    [switch]$BackendBakeoff,
    [switch]$Screening,
    [string]$MeasurementLockPath = "",
    [switch]$SkipBuild,
    [switch]$ReportOnly,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$acceptedBaselineTree = "88ac6f20cde853c9c8497cd634f3e8fa8a1ec067"
$minimumDirectCoverage = 0.90
$maximumDirectExitsPer100 = 5.0
$minimumFloorPasses = 4
$gateScriptHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()

function Set-GateProcessEnvironment([string]$Name, [object]$Value) {
    if ($null -eq $Value) {
        Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
        if ($null -ne [Environment]::GetEnvironmentVariable($Name, "Process")) {
            throw "Unable to remove process environment variable '$Name'."
        }
        return
    }
    [Environment]::SetEnvironmentVariable($Name, [string]$Value, "Process")
}

function Assert-BackendBakeoffMode(
    [bool]$IsReportOnly,
    [bool]$HasBaseline,
    [bool]$HasExplicitJit,
    [bool]$HasExplicitExecutable,
    [bool]$SkipRequested,
    [int]$RequestedProcessor,
    [string]$LockPath
) {
    if ($IsReportOnly) {
        throw "Backend bakeoff mode cannot be combined with ReportOnly."
    }
    if ($HasBaseline) {
        throw "Backend bakeoff mode compares one executable and does not accept BaselineRevision."
    }
    if ($HasExplicitJit) {
        throw "Backend bakeoff mode assigns IZARRAVM_JIT per role and does not accept Jit."
    }
    if ($HasExplicitExecutable -or $SkipRequested) {
        throw "Backend bakeoff mode requires one clean isolated build."
    }
    if ($RequestedProcessor -ne 8) {
        throw "Backend bakeoff mode requires ProcessorIndex 8."
    }
    if ([string]::IsNullOrWhiteSpace($LockPath) -or
        -not [IO.Path]::IsPathFullyQualified($LockPath)) {
        throw "Backend bakeoff mode requires an absolute MeasurementLockPath."
    }
}

function Enter-MeasurementLock([string]$Path) {
    $absolutePath = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($absolutePath)
    if ([string]::IsNullOrWhiteSpace($parent) -or
        -not (Test-Path -LiteralPath $parent -PathType Container)) {
        throw "Measurement lock parent directory does not exist: $parent"
    }
    try {
        $stream = [IO.File]::Open(
            $absolutePath,
            [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::None
        )
    } catch {
        throw "Measurement lock is already held or unavailable: $absolutePath"
    }
    try {
        $metadata = [ordered]@{
            pid = $PID
            acquired_utc = [DateTime]::UtcNow.ToString("o")
        } | ConvertTo-Json -Compress
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($metadata + "`n")
        $stream.SetLength(0)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
        return $stream
    } catch {
        $stream.Dispose()
        throw
    }
}

function Get-NormalizedCargoArguments([string[]]$Arguments) {
    $normalized = @()
    $replaceTarget = $false
    foreach ($argument in $Arguments) {
        if ($replaceTarget) {
            $normalized += "<isolated-target>"
            $replaceTarget = $false
        } else {
            $normalized += $argument
            if ($argument -eq "--target-dir") {
                $replaceTarget = $true
            }
        }
    }
    if ($replaceTarget) {
        throw "Cargo arguments ended after --target-dir."
    }
    return $normalized
}

function Get-BuildRecipeFingerprint($Recipe) {
    $canonical = [ordered]@{}
    foreach ($entry in $Recipe.GetEnumerator()) {
        if ($entry.Key -eq "cargo_arguments") {
            $canonical[$entry.Key] = @(Get-NormalizedCargoArguments ([string[]]$entry.Value))
        } else {
            $canonical[$entry.Key] = $entry.Value
        }
    }
    $json = $canonical | ConvertTo-Json -Depth 8 -Compress
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString(
            $algorithm.ComputeHash([Text.Encoding]::UTF8.GetBytes($json))
        )).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Assert-FormalBaselinePolicy(
    [bool]$IsReportOnly,
    [string]$BaselineTree,
    [bool]$IsAncestor
) {
    if ($IsReportOnly) {
        return
    }
    if ($BaselineTree -ne $acceptedBaselineTree) {
        throw "The formal gate requires the accepted uninstrumented baseline tree $acceptedBaselineTree."
    }
    if (-not $IsAncestor) {
        throw "The formal baseline must be an ancestor of candidate HEAD."
    }
}

function Assert-NoBuildEnvironmentOverrides([hashtable]$Overrides) {
    if ($Overrides.Count -gt 0) {
        throw "The formal gate refuses build environment overrides: $($Overrides.Keys -join ', ')."
    }
}

function Assert-WorkloadInputHashes($Actual, $Expected, [string]$Context, [bool]$Enforce) {
    $mismatches = @()
    foreach ($property in $Expected.PSObject.Properties) {
        if (-not $Actual.Contains($property.Name) -or $Actual[$property.Name] -ne $property.Value) {
            $mismatches += $property.Name
        }
    }
    if ($Actual.Count -ne @($Expected.PSObject.Properties).Count) {
        $mismatches += "input set"
    }
    $matches = $mismatches.Count -eq 0
    if (-not $matches -and $Enforce) {
        throw "$Context does not match the accepted workload manifest: $($mismatches -join ', ')."
    }
    return $matches
}

function Assert-ExpectedSha256([string]$Actual, [string]$Expected, [string]$Context, [bool]$Enforce) {
    $matches = $Actual -eq $Expected
    if (-not $matches -and $Enforce) {
        throw "$Context does not match the accepted workload manifest."
    }
    return $matches
}

function Get-PairedMetricVerdict([double]$Median, [double]$Lower95) {
    if ([double]::IsNaN($Median) -or [double]::IsInfinity($Median) -or
        [double]::IsNaN($Lower95) -or [double]::IsInfinity($Lower95) -or
        $Median -le 0 -or $Lower95 -le 0) {
        throw "Paired metric verdict inputs must be finite and positive."
    }
    if ($Median -lt 0.98) {
        return "regression"
    }
    if ($Lower95 -lt 0.97) {
        return "inconclusive"
    }
    return "pass"
}

function Get-CandidateSampleChecks($Policy, [object[]]$Samples) {
    return [pscustomobject][ordered]@{
        samples = $Samples.Count
        coverage_passes = @($Samples | Where-Object {
            $_.direct_native_coverage -ge $minimumDirectCoverage
        }).Count
        exit_rate_passes = @($Samples | Where-Object {
            $_.direct_slow_exits_per_100_instructions -lt $maximumDirectExitsPer100
        }).Count
        real_time_floor_passes = @($Samples | Where-Object {
            $_.real_time_factor -ge $Policy.minimum_real_time_factor
        }).Count
    }
}

function Assert-RoleDeterminism([string]$Name, [object[]]$Samples) {
    if (@($Samples.perf.instructions | Sort-Object -Unique).Count -ne 1 -or
        @($Samples.perf.jit_direct_entries | Sort-Object -Unique).Count -ne 1 -or
        @($Samples.perf.jit_direct_insns | Sort-Object -Unique).Count -ne 1 -or
        @($Samples.perf.jit_direct_side_exits | Sort-Object -Unique).Count -ne 1) {
        throw "$Name did not retire deterministic instruction and direct-JIT counters within one executable."
    }
    if ($Name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if (@($Samples.timedemo.gametics | Sort-Object -Unique).Count -ne 1 -or
            @($Samples.timedemo.realtics | Sort-Object -Unique).Count -ne 1) {
            throw "$Name did not produce a deterministic timedemo identity within one executable."
        }
    } elseif (@($Samples.quake_timedemo.line | Sort-Object -Unique).Count -ne 1) {
        throw "Quake did not produce a deterministic timedemo identity within one executable."
    }
}

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

function Read-QuakeTimedemoIdentities([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or
        -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return @()
    }
    $identities = @()
    foreach ($line in [IO.File]::ReadLines($Path)) {
        $identity = ConvertFrom-QuakeTimedemoLine $line
        if ($null -ne $identity) {
            $identities += $identity
        }
    }
    return @($identities)
}

function Read-QuakeTimedemoIdentity([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Quake did not produce QCONSOLE.LOG."
    }
    $identities = @(Read-QuakeTimedemoIdentities $Path)
    if ($identities.Count -ne 1) {
        throw "Quake must produce exactly one timedemo identity line; found $($identities.Count)."
    }
    $identity = $identities[0]
    if ($identity.frames -ne 969 -or $identity.seconds -le 0 -or
        $identity.fps -lt 41.0 -or $identity.fps -gt 44.0) {
        throw "Quake did not complete the 969-frame demo near its 42 fps target."
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

function Assert-UninstrumentedProfileSample($Sample, [string]$Context) {
    $property = $Sample.PSObject.Properties["machine_phase_timing_enabled"]
    if ($null -eq $property -or $property.Value -isnot [bool]) {
        throw "$Context profile is missing a boolean machine_phase_timing_enabled field."
    }
    if ($property.Value) {
        throw "$Context enabled machine phase timing and contaminated its wall sample."
    }
}

function Get-WorkloadPolicy([string]$Name) {
    switch ($Name) {
        "doom-486" {
            return [pscustomobject][ordered]@{
                name = $Name
                mode = "486"
                cycle_budget = [uint64]8000000000
                minimum_real_time_factor = 3.5
                minimum_realtics = 2900
                maximum_realtics = 3050
            }
        }
        "doom-586" {
            return [pscustomobject][ordered]@{
                name = $Name
                mode = "586"
                cycle_budget = [uint64]8000000000
                minimum_real_time_factor = 1.4
                minimum_realtics = 820
                maximum_realtics = 850
            }
        }
        "quake-586" {
            return [pscustomobject][ordered]@{
                name = $Name
                mode = "586"
                cycle_budget = [uint64]6200000000
                minimum_real_time_factor = 1.4
                minimum_realtics = $null
                maximum_realtics = $null
            }
        }
        default { throw "Unknown workload '$Name'." }
    }
}

function Get-WorkloadPolicies([string]$Selection) {
    $names = switch ($Selection) {
        "Both" { @("doom-486", "doom-586", "quake-586"); break }
        "Doom" { @("doom-486"); break }
        "Doom586" { @("doom-586"); break }
        "Quake" { @("quake-586"); break }
        default { throw "Unknown workload selection '$Selection'." }
    }
    return @($names | ForEach-Object { Get-WorkloadPolicy $_ })
}

function Get-PairOrder(
    [int]$PairNumber,
    [int]$Seed,
    [string[]]$Roles = @("candidate", "baseline")
) {
    if ($Roles.Count -ne 2 -or $Roles[0] -eq $Roles[1]) {
        throw "Paired measurements require two distinct role names."
    }
    $candidateFirstOnOddPairs = ($Seed -band 1) -eq 0
    $candidateFirst = if ($PairNumber % 2 -eq 1) {
        $candidateFirstOnOddPairs
    } else {
        -not $candidateFirstOnOddPairs
    }
    if ($candidateFirst) {
        return @($Roles[0], $Roles[1])
    }
    return @($Roles[1], $Roles[0])
}

function Get-Median([double[]]$Values) {
    $ordered = @($Values | Sort-Object)
    $middle = [Math]::Floor($ordered.Count / 2)
    if ($ordered.Count % 2 -eq 1) {
        return $ordered[$middle]
    }
    return ($ordered[$middle - 1] + $ordered[$middle]) / 2.0
}

function Get-OneSided95TCritical([int]$SampleCount) {
    if ($SampleCount -lt 2) {
        throw "A Student-t confidence bound requires at least two samples."
    }
    $criticalByDegreesOfFreedom = [double[]](
        6.313751515, 2.919985580, 2.353363435, 2.131846786, 2.015048,
        1.943180281, 1.894578605, 1.859548038, 1.833112933, 1.812461123,
        1.795884819, 1.782287556, 1.770933396, 1.761310136, 1.753050356,
        1.745883676, 1.739606726, 1.734063607, 1.729132812, 1.724718243,
        1.720742903, 1.717144374, 1.713871528, 1.710882080, 1.708140761,
        1.705617920, 1.703288446, 1.701130934, 1.699127027, 1.697260887
    )
    $degreesOfFreedom = $SampleCount - 1
    if ($degreesOfFreedom -gt $criticalByDegreesOfFreedom.Count) {
        return $criticalByDegreesOfFreedom[-1]
    }
    return $criticalByDegreesOfFreedom[$degreesOfFreedom - 1]
}

function Format-AffinityMask([int64]$Mask) {
    return "0x" + $Mask.ToString("x16", [Globalization.CultureInfo]::InvariantCulture)
}

function Get-PairedMetric([double[]]$Ratios) {
    if ($Ratios.Count -lt 2) {
        throw "Paired metrics require at least two ratios."
    }
    if (@($Ratios | Where-Object {
        $_ -le 0 -or [double]::IsNaN($_) -or [double]::IsInfinity($_)
    }).Count -gt 0) {
        throw "Paired ratios must be finite and positive."
    }
    $logs = @($Ratios | ForEach-Object { [Math]::Log($_) })
    $mean = ($logs | Measure-Object -Average).Average
    $sumSquares = 0.0
    foreach ($value in $logs) {
        $sumSquares += ($value - $mean) * ($value - $mean)
    }
    $sampleDeviation = [Math]::Sqrt($sumSquares / ($logs.Count - 1))
    $critical = Get-OneSided95TCritical $logs.Count
    $lower95 = [Math]::Exp($mean - $critical * $sampleDeviation / [Math]::Sqrt($logs.Count))
    $median = Get-Median $Ratios
    $verdict = Get-PairedMetricVerdict $median $lower95
    return [pscustomobject][ordered]@{
        median_ratio = $median
        lower_95_ratio = $lower95
        lower_bound_confidence = "one-sided 95% Student-t"
        verdict = $verdict
    }
}

function Get-BackendPairedMetric([double[]]$Ratios) {
    $metric = Get-PairedMetric $Ratios
    $survivalVerdict = if ($metric.median_ratio -ge 1.05 -and
        $metric.lower_95_ratio -gt 1.0) {
        "pass"
    } else {
        "fail"
    }
    return [pscustomobject][ordered]@{
        median_ratio = $metric.median_ratio
        lower_95_ratio = $metric.lower_95_ratio
        lower_bound_confidence = $metric.lower_bound_confidence
        required_median_ratio = 1.05
        required_lower_95_ratio_exclusive = 1.0
        verdict = $survivalVerdict
    }
}

function Get-ArtifactSelectionPolicy(
    [bool]$IsReportOnly,
    [bool]$ExplicitExecutable,
    [bool]$SkipRequested
) {
    if (-not $IsReportOnly -and ($ExplicitExecutable -or $SkipRequested)) {
        throw "The formal gate refuses custom or prebuilt executables."
    }
    if ($ExplicitExecutable -or $SkipRequested) {
        return "unverified_prebuilt"
    }
    return "isolated_build"
}

function Get-BytesSha256([byte[]]$Bytes) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-FileSha256([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Evidence artifact is missing: $Path"
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-NormalizedResultBlock([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return [pscustomobject][ordered]@{
            status = "missing_file"
            block_count = 0
            sha256 = $null
            normalized_bytes = 0
        }
    }
    $normalized = [IO.File]::ReadAllText($Path).Replace("`r`n", "`n").Replace("`r", "`n")
    $pattern = '(?ms)^--- BEGIN RESULT ---\n.*?^--- END RESULT ---[ \t]*(?:\n|$)'
    $matches = [regex]::Matches($normalized, $pattern)
    if ($matches.Count -ne 1) {
        return [pscustomobject][ordered]@{
            status = "invalid_block_count"
            block_count = $matches.Count
            sha256 = $null
            normalized_bytes = 0
        }
    }
    $block = $matches[0].Value.TrimEnd() + "`n"
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($block)
    return [pscustomobject][ordered]@{
        status = "valid"
        block_count = 1
        sha256 = Get-BytesSha256 $bytes
        normalized_bytes = $bytes.Length
    }
}

function Get-StopIdentityKey($Sample) {
    if ($null -eq $Sample.stop) {
        return "missing"
    }
    $code = if ($null -ne $Sample.stop.PSObject.Properties["code"]) {
        [string]$Sample.stop.code
    } else {
        ""
    }
    $requested = if ($null -ne $Sample.stop.PSObject.Properties["requested"]) {
        [string]$Sample.stop.requested
    } else {
        ""
    }
    $message = if ($null -ne $Sample.stop.PSObject.Properties["message"]) {
        [string]$Sample.stop.message
    } else {
        ""
    }
    return "$($Sample.stop.kind)|code=$code|requested=$requested|message=$message"
}

function Get-TimedemoIdentityKey([string]$WorkloadName, $Sample) {
    if ($WorkloadName.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if ($null -eq $Sample.timedemo) {
            return "missing"
        }
        return "doom|$($Sample.timedemo.gametics)|$($Sample.timedemo.realtics)"
    }
    if ($null -eq $Sample.quake_timedemo) {
        $count = if ($null -ne $Sample.PSObject.Properties["quake_timedemo_identity_count"]) {
            $Sample.quake_timedemo_identity_count
        } else {
            "missing"
        }
        return "quake|missing|count=$count"
    }
    return "quake|$($Sample.quake_timedemo.line)"
}

function Get-EqualWorkRecord([string]$WorkloadName, $Sample) {
    $resultStatus = [string]$Sample.gate_artifacts.result_block_status
    $resultHash = [string]$Sample.gate_artifacts.result_block_sha256
    return [ordered]@{
        instructions = [uint64]$Sample.perf.instructions
        master_ticks = [uint64]$Sample.master_ticks
        elapsed_budget_clocks = [uint64]$Sample.elapsed_budget_clocks
        executed_cpu_core_clocks = [uint64]$Sample.executed_cpu_core_clocks
        raw_bus_clocks = [uint64]$Sample.raw_bus_clocks
        stop = Get-StopIdentityKey $Sample
        timedemo_identity = Get-TimedemoIdentityKey $WorkloadName $Sample
        result_block_identity = "$resultStatus|$resultHash"
    }
}

function Compare-EqualWorkRecords($Left, $Right) {
    $mismatches = @()
    foreach ($field in $Left.Keys) {
        if ([string]$Left[$field] -cne [string]$Right[$field]) {
            $mismatches += $field
        }
    }
    return [pscustomobject][ordered]@{
        matches = $mismatches.Count -eq 0
        mismatched_fields = $mismatches
    }
}

function Get-RoleExactDeterminism([string]$WorkloadName, [object[]]$Samples) {
    if ($Samples.Count -eq 0) {
        return [pscustomobject][ordered]@{
            deterministic = $false
            mismatched_fields = @("missing_samples")
        }
    }
    $reference = Get-EqualWorkRecord $WorkloadName $Samples[0]
    $mismatches = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    for ($index = 1; $index -lt $Samples.Count; $index++) {
        $comparison = Compare-EqualWorkRecords `
            $reference (Get-EqualWorkRecord $WorkloadName $Samples[$index])
        foreach ($field in $comparison.mismatched_fields) {
            $null = $mismatches.Add($field)
        }
    }
    return [pscustomobject][ordered]@{
        deterministic = $mismatches.Count -eq 0
        mismatched_fields = @($mismatches | Sort-Object)
    }
}

function Get-BackendEvidencePolicy([bool]$IsScreening) {
    return [pscustomobject][ordered]@{
        evidence_grade = if ($IsScreening) { "screening" } else { "final" }
        measured_pairs = if ($IsScreening) { 3 } else { 6 }
        final_eligible = -not $IsScreening
    }
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
    Assert-UninstrumentedProfileSample ([pscustomobject]@{
        machine_phase_timing_enabled = $false
    }) "clean sample"
    Assert-SelfTestThrows {
        Assert-UninstrumentedProfileSample ([pscustomobject]@{
            machine_phase_timing_enabled = $true
        }) "instrumented sample"
    } "contaminated"
    Assert-SelfTestThrows {
        Assert-UninstrumentedProfileSample ([pscustomobject]@{}) "legacy sample"
    } "missing a boolean"
    $policies = Get-WorkloadPolicies "Both"
    if ($policies.Count -ne 3 -or ($policies.name -join ",") -ne "doom-486,doom-586,quake-586" -or
        $policies[0].minimum_real_time_factor -ne 3.5 -or
        $policies[1].minimum_real_time_factor -ne 1.4 -or
        $policies[2].minimum_real_time_factor -ne 1.4) {
        throw "Both did not expand to the three workload-specific policies."
    }
    if ((Get-PairOrder 1 0) -join "," -ne "candidate,baseline" -or
        (Get-PairOrder 2 0) -join "," -ne "baseline,candidate") {
        throw "Pair order did not alternate."
    }
    $backendRoles = @("automatic", "interpreter")
    if ((Get-PairOrder 1 0 $backendRoles) -join "," -ne "automatic,interpreter" -or
        (Get-PairOrder 2 0 $backendRoles) -join "," -ne "interpreter,automatic") {
        throw "Backend pair order did not alternate."
    }
    Assert-SelfTestThrows {
        Get-PairOrder 1 0 @("automatic", "automatic")
    } "distinct role names"
    if ((Get-PairedMetric ([double[]](1, 1, 1, 1, 1, 1))).verdict -ne "pass" -or
        (Get-PairedMetric ([double[]](0.97, 0.97, 0.97, 0.97, 0.97, 0.97))).verdict -ne "regression" -or
        (Get-PairedMetric ([double[]](0.90, 0.91, 1.00, 1.01, 1.10, 1.11))).verdict -ne "inconclusive") {
        throw "Paired metric verdict boundaries are wrong."
    }
    if ((Get-BackendPairedMetric ([double[]](1.05, 1.05, 1.05, 1.05, 1.05, 1.05))).verdict -ne "pass" -or
        (Get-BackendPairedMetric ([double[]](1.049, 1.049, 1.049, 1.049, 1.049, 1.049))).verdict -ne "fail" -or
        (Get-BackendPairedMetric ([double[]](0.9, 1.2, 0.9, 1.2, 0.9, 1.2))).verdict -ne "fail") {
        throw "Backend survival boundaries are wrong."
    }
    $expectedCriticals = [ordered]@{
        2 = 6.313751515
        3 = 2.919985580
        4 = 2.353363435
        5 = 2.131846786
        6 = 2.015048
    }
    foreach ($entry in $expectedCriticals.GetEnumerator()) {
        if ([Math]::Abs((Get-OneSided95TCritical $entry.Key) - $entry.Value) -gt 1.0e-9) {
            throw "The one-sided Student-t critical for $($entry.Key) samples is wrong."
        }
    }
    if ((Get-OneSided95TCritical 100) -ne 1.697260887) {
        throw "Large paired samples must use the conservative 30-degree critical."
    }
    $twoSampleMetric = Get-PairedMetric ([double[]](1.0, 2.0))
    $expectedTwoSampleLower = [Math]::Exp(
        [Math]::Log(2.0) / 2.0 - 6.313751515 * [Math]::Log(2.0) / 2.0
    )
    if ([Math]::Abs($twoSampleMetric.lower_95_ratio - $expectedTwoSampleLower) -gt 1.0e-12) {
        throw "The two-sample one-sided lower confidence bound is wrong."
    }
    Assert-SelfTestThrows {
        Get-OneSided95TCritical 1
    } "at least two samples"
    if ((Format-AffinityMask ([int64]1 -shl 62)) -ne "0x4000000000000000") {
        throw "Affinity masks are not serialized as fixed-width hexadecimal strings."
    }
    $environmentSelfTestName = "IZARRAVM_GATE_SELF_TEST_$([guid]::NewGuid().ToString('N'))"
    try {
        Set-GateProcessEnvironment $environmentSelfTestName "present"
        if ([Environment]::GetEnvironmentVariable($environmentSelfTestName, "Process") -ne
            "present") {
            throw "The gate process environment helper did not set a value."
        }
        Set-GateProcessEnvironment $environmentSelfTestName $null
        if (Test-Path -LiteralPath "Env:$environmentSelfTestName") {
            throw "The gate process environment helper left an empty variable behind."
        }
    } finally {
        Set-GateProcessEnvironment $environmentSelfTestName $null
    }
    if ((Get-PairedMetricVerdict 0.98 0.97) -ne "pass" -or
        (Get-PairedMetricVerdict 0.979999 0.99) -ne "regression" -or
        (Get-PairedMetricVerdict 0.98 0.969999) -ne "inconclusive") {
        throw "Exact paired metric acceptance boundaries are wrong."
    }
    Assert-SelfTestThrows {
        Get-PairedMetricVerdict ([double]::NaN) 0.97
    } "finite and positive"
    Assert-SelfTestThrows {
        Get-PairedMetric ([double[]](1, 1, 1, 1, 1, [double]::PositiveInfinity))
    } "finite and positive"
    $acceptedInput = "a" * 64
    $actualInputs = [ordered]@{ "AUTOEXEC.BAT" = $acceptedInput }
    $expectedInputs = [pscustomobject]@{ "AUTOEXEC.BAT" = $acceptedInput }
    if (-not (Assert-WorkloadInputHashes $actualInputs $expectedInputs "fixture" $true)) {
        throw "The accepted workload manifest self-test did not match."
    }
    $actualInputs["AUTOEXEC.BAT"] = "b" + $acceptedInput.Substring(1)
    Assert-SelfTestThrows {
        Assert-WorkloadInputHashes $actualInputs $expectedInputs "fixture" $true
    } "AUTOEXEC.BAT"
    Assert-SelfTestThrows {
        Assert-ExpectedSha256 ("b" + $acceptedInput.Substring(1)) $acceptedInput "fixture tree" $true
    } "accepted workload manifest"
    $boundaryPolicy = [pscustomobject]@{ minimum_real_time_factor = 1.4 }
    $boundarySamples = @(
        [pscustomobject]@{ direct_native_coverage = 0.90; direct_slow_exits_per_100_instructions = 4.999; real_time_factor = 1.4 },
        [pscustomobject]@{ direct_native_coverage = 0.91; direct_slow_exits_per_100_instructions = 4.0; real_time_factor = 1.5 },
        [pscustomobject]@{ direct_native_coverage = 0.92; direct_slow_exits_per_100_instructions = 3.0; real_time_factor = 1.6 },
        [pscustomobject]@{ direct_native_coverage = 0.93; direct_slow_exits_per_100_instructions = 2.0; real_time_factor = 1.7 },
        [pscustomobject]@{ direct_native_coverage = 0.94; direct_slow_exits_per_100_instructions = 1.0; real_time_factor = 1.3 },
        [pscustomobject]@{ direct_native_coverage = 0.95; direct_slow_exits_per_100_instructions = 0.0; real_time_factor = 1.2 }
    )
    $boundaryChecks = Get-CandidateSampleChecks $boundaryPolicy $boundarySamples
    if ($boundaryChecks.coverage_passes -ne 6 -or $boundaryChecks.exit_rate_passes -ne 6 -or
        $boundaryChecks.real_time_floor_passes -ne 4) {
        throw "Every-sample and four-of-six acceptance boundaries are wrong."
    }
    $boundarySamples[0].direct_native_coverage = 0.899999
    $boundarySamples[1].direct_slow_exits_per_100_instructions = 5.0
    $boundarySamples[3].real_time_factor = 1.3
    $boundaryChecks = Get-CandidateSampleChecks $boundaryPolicy $boundarySamples
    if ($boundaryChecks.coverage_passes -ne 5 -or $boundaryChecks.exit_rate_passes -ne 5 -or
        $boundaryChecks.real_time_floor_passes -ne 3) {
        throw "Every-sample failure and three-of-six floor boundaries are wrong."
    }
    $deterministicSamples = @(
        [pscustomobject]@{
            perf = [pscustomobject]@{ instructions = 10; jit_direct_entries = 8; jit_direct_insns = 9; jit_direct_side_exits = 1 }
            timedemo = [pscustomobject]@{ gametics = 2134; realtics = 830 }
        },
        [pscustomobject]@{
            perf = [pscustomobject]@{ instructions = 10; jit_direct_entries = 8; jit_direct_insns = 9; jit_direct_side_exits = 1 }
            timedemo = [pscustomobject]@{ gametics = 2134; realtics = 831 }
        }
    )
    Assert-SelfTestThrows {
        Assert-RoleDeterminism "doom-586" $deterministicSamples
    } "deterministic timedemo"
    Assert-FormalBaselinePolicy $false $acceptedBaselineTree $true
    Assert-SelfTestThrows {
        Assert-FormalBaselinePolicy $false ("0" * 40) $true
    } "accepted uninstrumented baseline"
    Assert-SelfTestThrows {
        Assert-FormalBaselinePolicy $false $acceptedBaselineTree $false
    } "must be an ancestor"
    Assert-NoBuildEnvironmentOverrides @{}
    Assert-SelfTestThrows {
        Assert-NoBuildEnvironmentOverrides @{ RUSTFLAGS = "-C target-cpu=native" }
    } "RUSTFLAGS"
    $recipeA = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "C:\scratch-a\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeB = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeToolchainChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc changed"
        source_config = "none"
    }
    $recipeConfigChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "present"
    }
    if ((Get-BuildRecipeFingerprint $recipeA) -ne (Get-BuildRecipeFingerprint $recipeB) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeChanged) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeToolchainChanged) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeConfigChanged)) {
        throw "Build recipe fingerprint normalization is wrong."
    }
    if ((Get-ArtifactSelectionPolicy $true $true $false) -ne "unverified_prebuilt") {
        throw "Custom report-only artifacts must remain unverified."
    }
    Assert-SelfTestThrows {
        Get-ArtifactSelectionPolicy $false $true $false
    } "refuses custom"
    Assert-SelfTestThrows {
        Get-ArtifactSelectionPolicy $false $false $true
    } "refuses custom"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $true $false $false $false $false 8 "C:\gate.lock"
    } "ReportOnly"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $true $false $false $false 8 "C:\gate.lock"
    } "BaselineRevision"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $true $false $false 8 "C:\gate.lock"
    } "does not accept Jit"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $false $false $false 7 "C:\gate.lock"
    } "ProcessorIndex 8"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $false $false $false 8 "relative.lock"
    } "absolute MeasurementLockPath"
    $resultBlockSelfTestPath = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-result-block-$([guid]::NewGuid().ToString('N')).log"
    )
    try {
        [IO.File]::WriteAllText(
            $resultBlockSelfTestPath,
            "prefix`r`n--- BEGIN RESULT ---`r`nstop: TestExit { code: 0 }`r`n--- END RESULT ---`r`nwall: 1.0s`r`n"
        )
        $resultBlock = Get-NormalizedResultBlock $resultBlockSelfTestPath
        if ($resultBlock.status -ne "valid" -or $resultBlock.block_count -ne 1 -or
            $resultBlock.normalized_bytes -le 0 -or $resultBlock.sha256.Length -ne 64) {
            throw "Normalized result-block evidence is incomplete."
        }
        [IO.File]::WriteAllText($resultBlockSelfTestPath, "no result block`n")
        $invalidResultBlock = Get-NormalizedResultBlock $resultBlockSelfTestPath
        if ($invalidResultBlock.status -ne "invalid_block_count" -or
            $invalidResultBlock.block_count -ne 0 -or $null -ne $invalidResultBlock.sha256) {
            throw "Invalid result-block evidence was not preserved as summary data."
        }
    } finally {
        Remove-Item -LiteralPath $resultBlockSelfTestPath -Force -ErrorAction SilentlyContinue
    }
    $exactSampleA = [pscustomobject]@{
        perf = [pscustomobject]@{ instructions = 10 }
        master_ticks = 20
        elapsed_budget_clocks = 30
        executed_cpu_core_clocks = 11
        raw_bus_clocks = 19
        stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
        timedemo = [pscustomobject]@{ gametics = 2134; realtics = 830 }
        gate_artifacts = [pscustomobject]@{
            result_block_status = "valid"
            result_block_sha256 = "a" * 64
        }
    }
    $exactSampleB = $exactSampleA.PSObject.Copy()
    $equalComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if (-not $equalComparison.matches) {
        throw "Equal-work comparison rejected identical samples."
    }
    $exactSampleB.raw_bus_clocks = 18
    $unequalComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if ($unequalComparison.matches -or
        $unequalComparison.mismatched_fields -notcontains "raw_bus_clocks") {
        throw "Equal-work comparison did not preserve a valid negative result."
    }
    $screeningPolicy = Get-BackendEvidencePolicy $true
    $finalPolicy = Get-BackendEvidencePolicy $false
    if ($screeningPolicy.final_eligible -or $screeningPolicy.measured_pairs -ne 3 -or
        -not $finalPolicy.final_eligible -or $finalPolicy.measured_pairs -ne 6) {
        throw "Backend evidence-grade eligibility is wrong."
    }
    $lockSelfTestPath = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-measurement-$([guid]::NewGuid().ToString('N')).lock"
    )
    $lockSelfTest = $null
    try {
        $lockSelfTest = Enter-MeasurementLock $lockSelfTestPath
        Assert-SelfTestThrows {
            $secondLock = Enter-MeasurementLock $lockSelfTestPath
            $secondLock.Dispose()
        } "already held or unavailable"
    } finally {
        if ($null -ne $lockSelfTest) {
            $lockSelfTest.Dispose()
        }
        Remove-Item -LiteralPath $lockSelfTestPath -Force -ErrorAction SilentlyContinue
    }
    Write-Host "run-realtime-gate self-test passed"
    return
}

$explicitExecutable = $PSBoundParameters.ContainsKey("Executable")
$explicitJit = $PSBoundParameters.ContainsKey("Jit")
$explicitRuns = $PSBoundParameters.ContainsKey("Runs")
if ($Screening -and -not $BackendBakeoff) {
    throw "Screening is only valid with BackendBakeoff."
}
if ($BackendBakeoff) {
    Assert-BackendBakeoffMode `
        ([bool]$ReportOnly) `
        (-not [string]::IsNullOrWhiteSpace($BaselineRevision)) `
        $explicitJit `
        $explicitExecutable `
        ([bool]$SkipBuild) `
        $ProcessorIndex `
        $MeasurementLockPath
    if ($Screening) {
        if ($explicitRuns -and $Runs -ne 3) {
            throw "Backend screening requires exactly three measured pairs."
        }
        $Runs = 3
    } elseif ($Runs -ne 6) {
        throw "Final backend bakeoff requires exactly six measured pairs."
    }
    if ($Workload -ne "Both") {
        throw "Backend bakeoff requires all three workloads."
    }
} else {
    if ($Runs -lt 1) {
        throw "Runs must be at least one."
    }
    if (-not $ReportOnly -and $Runs -ne 6) {
        throw "The throughput gate requires exactly six clean pairs. Use -ReportOnly for ad hoc counts."
    }
    if (-not $ReportOnly -and $Workload -ne "Both") {
        throw "The throughput gate requires all three workloads. Use -ReportOnly for a subset."
    }
    if (-not $ReportOnly -and $Jit -ne "1") {
        throw "The throughput gate requires the direct JIT. Use -ReportOnly for a JIT-off control."
    }
    if (-not $ReportOnly -and [string]::IsNullOrWhiteSpace($BaselineRevision)) {
        throw "The throughput gate requires an explicit accepted BaselineRevision."
    }
    if ($ReportOnly -and -not [string]::IsNullOrWhiteSpace($BaselineRevision) -and $Runs -lt 2) {
        throw "Paired report-only measurements require at least two pairs."
    }
}
$artifactSelection = Get-ArtifactSelectionPolicy ([bool]$ReportOnly) $explicitExecutable ([bool]$SkipBuild)
if ($PairSeed -eq 0) {
    $PairSeed = [Security.Cryptography.RandomNumberGenerator]::GetInt32(1, [int]::MaxValue)
}
if ($HostTimeoutSeconds -lt 1) {
    throw "HostTimeoutSeconds must be positive."
}
if ($ProcessorIndex -lt -1 -or $ProcessorIndex -gt 62) {
    throw "ProcessorIndex must be -1 (unpinned) or a bit index from 0 through 62."
}
if (-not $ReportOnly -and $ProcessorIndex -lt 0) {
    throw "The formal gate requires an explicit ProcessorIndex."
}

$runtimeInformation = [Runtime.InteropServices.RuntimeInformation]
$isWindowsHost = $runtimeInformation::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)
$logicalProcessorCount = [Environment]::ProcessorCount
$gateProcess = $null
$originalGateAffinity = $null
$requestedProcessorMask = $null
if ($isWindowsHost) {
    $gateProcess = [Diagnostics.Process]::GetCurrentProcess()
    $gateProcess.Refresh()
    $originalGateAffinity = $gateProcess.ProcessorAffinity.ToInt64()
}
if ($ProcessorIndex -ge 0) {
    if (-not $isWindowsHost -or -not [Environment]::Is64BitProcess -or [IntPtr]::Size -ne 8) {
        throw "Pinned measurements require 64-bit PowerShell on Windows."
    }
    if ($logicalProcessorCount -gt 64) {
        throw "ProcessorAffinity is ambiguous across processor groups on hosts with more than 64 logical processors."
    }
    $requestedProcessorMask = [int64]1 -shl $ProcessorIndex
    if (($originalGateAffinity -band $requestedProcessorMask) -ne $requestedProcessorMask) {
        throw "ProcessorIndex $ProcessorIndex is not available in the gate process affinity mask."
    }
}

$processorIdentifier = [Environment]::GetEnvironmentVariable("PROCESSOR_IDENTIFIER", "Process")
$processorName = $null
$activePowerScheme = $null
function Get-ActivePowerScheme {
    try {
        $powercfg = Get-Command powercfg.exe -CommandType Application -ErrorAction Stop
        $powerOutput = @(& $powercfg.Source /getactivescheme 2>$null)
        if ($LASTEXITCODE -eq 0) {
            return ($powerOutput -join " ").Trim()
        }
    } catch {
        return $null
    }
    return $null
}
if ($isWindowsHost) {
    try {
        $processorName = (Get-ItemProperty `
            -LiteralPath "HKLM:\HARDWARE\DESCRIPTION\System\CentralProcessor\0" `
            -ErrorAction Stop).ProcessorNameString.Trim()
    } catch {
        $processorName = $null
    }
    $activePowerScheme = Get-ActivePowerScheme
}
$hostIdentity = [ordered]@{
    os_description = $runtimeInformation::OSDescription
    os_architecture = $runtimeInformation::OSArchitecture.ToString()
    process_architecture = $runtimeInformation::ProcessArchitecture.ToString()
    framework_description = $runtimeInformation::FrameworkDescription
    logical_processor_count = $logicalProcessorCount
    processor_name = $processorName
    processor_identifier = $processorIdentifier
    active_power_scheme = $activePowerScheme
}
$verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()

function Get-RepositoryState([string]$Root) {
    $head = (& git -C $Root rev-parse --verify HEAD).Trim()
    $tree = (& git -C $Root rev-parse --verify "HEAD^{tree}").Trim()
    $branch = (& git -C $Root symbolic-ref --quiet --short HEAD 2>$null)
    if ($LASTEXITCODE -ne 0) {
        $branch = $null
    } else {
        $branch = $branch.Trim()
    }
    $status = @(& git -C $Root status --porcelain=v2 --untracked-files=normal)
    if ([string]::IsNullOrWhiteSpace($head) -or [string]::IsNullOrWhiteSpace($tree) -or
        $LASTEXITCODE -ne 0) {
        throw "Unable to read the Git repository state."
    }
    return [pscustomobject][ordered]@{
        head_commit = $head
        head_tree = $tree
        branch = $branch
        dirty = $status.Count -gt 0
        status = $status
    }
}

$repositoryHint = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$repositoryRoot = (& git -C $repositoryHint rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($repositoryRoot)) {
    throw "Unable to find the Git repository that owns this gate script."
}
$repositoryRoot = (Resolve-Path -LiteralPath $repositoryRoot).Path

function Get-RepositoryRelativePath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

$Executable = Get-RepositoryRelativePath $Executable
if ($Workload -in @("Both", "Doom", "Doom586")) {
    $DoomFolder = Get-RepositoryRelativePath $DoomFolder
    if (-not (Test-Path -LiteralPath "$DoomFolder/AUTOEXEC.BAT" -PathType Leaf)) {
        throw "The Doom fixture needs AUTOEXEC.BAT."
    }
}
if ($Workload -in @("Both", "Quake")) {
    $QuakeFolder = Get-RepositoryRelativePath $QuakeFolder
    if (-not (Test-Path -LiteralPath "$QuakeFolder/AUTOEXEC.BAT" -PathType Leaf)) {
        throw "The Quake fixture needs AUTOEXEC.BAT."
    }
}

$repositoryAtSelection = Get-RepositoryState $repositoryRoot
if (-not $ReportOnly -and $repositoryAtSelection.dirty) {
    throw "The formal gate requires a clean candidate worktree."
}
$revision = $repositoryAtSelection.head_commit
$shortRevision = $revision.Substring(0, 12)
$baselineCommit = $null
$baselineTree = $null
if (-not [string]::IsNullOrWhiteSpace($BaselineRevision)) {
    $baselineCommit = (& git -C $repositoryRoot rev-parse --verify "${BaselineRevision}^{commit}").Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($baselineCommit)) {
        throw "Unable to resolve BaselineRevision '$BaselineRevision'."
    }
    if ($baselineCommit -eq $revision -and -not $ReportOnly) {
        throw "The formal baseline must differ from candidate HEAD."
    }
    $baselineTree = (& git -C $repositoryRoot rev-parse --verify "$baselineCommit^{tree}").Trim()
    $mergeBase = (& git -C $repositoryRoot merge-base $baselineCommit $revision).Trim()
    $baselineIsAncestor = $LASTEXITCODE -eq 0 -and $mergeBase -eq $baselineCommit
    Assert-FormalBaselinePolicy ([bool]$ReportOnly) $baselineTree $baselineIsAncestor
}

if ([string]::IsNullOrWhiteSpace($ResultsDirectory)) {
    $stamp = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss")
    $suffix = [guid]::NewGuid().ToString("N").Substring(0, 8)
    $ResultsDirectory = ".bench/results/$shortRevision-$stamp-$suffix"
}
$ResultsDirectory = Get-RepositoryRelativePath $ResultsDirectory
if (-not $ReportOnly -and (Test-Path -LiteralPath $ResultsDirectory)) {
    throw "The formal gate requires a new results directory."
}
New-Item -ItemType Directory -Path $ResultsDirectory | Out-Null
$ResultsDirectory = (Resolve-Path -LiteralPath $ResultsDirectory).Path

$measurementLockHandle = $null
$measurementLockEvidence = $null
if ($BackendBakeoff) {
    $MeasurementLockPath = [IO.Path]::GetFullPath($MeasurementLockPath)
    $measurementLockAcquiredUtc = [DateTime]::UtcNow.ToString("o")
    $measurementLockHandle = Enter-MeasurementLock $MeasurementLockPath
    $measurementLockEvidence = [ordered]@{
        path = $MeasurementLockPath
        acquired_utc = $measurementLockAcquiredUtc
        pid = $PID
        share_mode = "FileShare.None"
        scope = "cooperating campaign tools"
        held_through_summary_write = $true
    }
}

try {

$buildOverrideNames = @(Get-ChildItem Env: | Where-Object {
    ($_.Name.StartsWith("CARGO_", [StringComparison]::OrdinalIgnoreCase) -and
        $_.Name -ne "CARGO_HOME") -or
    $_.Name.StartsWith("CMAKE_", [StringComparison]::OrdinalIgnoreCase) -or
    $_.Name.StartsWith("VCPKG_", [StringComparison]::OrdinalIgnoreCase) -or
    $_.Name.StartsWith("PKG_CONFIG_", [StringComparison]::OrdinalIgnoreCase) -or
    $_.Name.StartsWith("BINDGEN_", [StringComparison]::OrdinalIgnoreCase) -or
    $_.Name.StartsWith("CRATE_CC_", [StringComparison]::OrdinalIgnoreCase) -or
    $_.Name -match '^(?:(?:HOST|TARGET)_)?(?:CC|CXX|AR|RANLIB|RC|ASM|CFLAGS|CXXFLAGS|ARFLAGS|RANLIBFLAGS|RCFLAGS|ASMFLAGS)(?:_.+)?$' -or
    $_.Name -in @(
        "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN", "CC", "CFLAGS", "CXX", "CXXFLAGS", "LD", "LDFLAGS",
        "CL", "_CL_", "LINK", "_LINK_", "LIB", "INCLUDE"
    )
} | ForEach-Object { $_.Name } | Sort-Object -Unique)
$detectedBuildEnvironmentOverrides = @{}
foreach ($name in $buildOverrideNames) {
    $detectedBuildEnvironmentOverrides[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
if (-not $ReportOnly) {
    Assert-NoBuildEnvironmentOverrides $detectedBuildEnvironmentOverrides
}
$packageCacheCargoHome = [Environment]::GetEnvironmentVariable("CARGO_HOME", "Process")
if ([string]::IsNullOrWhiteSpace($packageCacheCargoHome)) {
    $packageCacheCargoHome = Join-Path $HOME ".cargo"
}
$packageCacheCargoHome = [IO.Path]::GetFullPath($packageCacheCargoHome)

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

function Get-DirectoryTreeSha256([string]$Root, [string[]]$ExcludedRelativePaths = @()) {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $excluded = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($relativePath in $ExcludedRelativePaths) {
        $null = $excluded.Add($relativePath.Replace("\", "/"))
    }
    $files = @(Get-ChildItem -LiteralPath $rootPath -File -Recurse -Force | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($rootPath, $_.FullName).Replace("\", "/")
        if (-not $excluded.Contains($relative)) {
            [pscustomobject]@{
                relative = $relative
                path = $_.FullName
                length = $_.Length
            }
        }
    })
    [Array]::Sort($files, [Comparison[object]]{
        param($left, $right)
        [StringComparer]::Ordinal.Compare($left.relative, $right.relative)
    })
    $records = foreach ($file in $files) {
        $hash = (Get-FileHash -LiteralPath $file.path -Algorithm SHA256).Hash.ToLowerInvariant()
        "$($file.relative)`0$($file.length)`0$hash`n"
    }
    return Get-BytesSha256 ([Text.Encoding]::UTF8.GetBytes(($records -join "")))
}

function Get-AmbientCargoConfigurationPaths([string]$SourceRoot) {
    $paths = @()
    $directory = [IO.DirectoryInfo]::new([IO.Path]::GetFullPath($SourceRoot)).Parent
    while ($null -ne $directory) {
        foreach ($relativePath in @(".cargo/config", ".cargo/config.toml")) {
            $candidate = Join-Path $directory.FullName $relativePath
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $paths += [IO.Path]::GetFullPath($candidate)
            }
        }
        $directory = $directory.Parent
    }
    return @($paths | Sort-Object -Unique)
}

function Get-ToolExecutableIdentity([string]$Path) {
    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "Native build tool not found at $resolved."
    }
    $file = Get-Item -LiteralPath $resolved
    return [ordered]@{
        path = $resolved
        sha256 = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
        file_version = $file.VersionInfo.FileVersion
        product_version = $file.VersionInfo.ProductVersion
    }
}

function Get-NativeBuildToolchainIdentity(
    [string]$TargetRoot,
    [string]$CmakePath,
    [string[]]$CmakeVersion
) {
    $cacheFiles = @(Get-ChildItem -LiteralPath $TargetRoot -Filter "CMakeCache.txt" -File -Recurse -Force |
        Sort-Object FullName)
    if ($cacheFiles.Count -eq 0) {
        throw "The release build did not produce native-synth CMake caches."
    }
    $wantedCacheKeys = @(
        "CMAKE_C_COMPILER",
        "CMAKE_CXX_COMPILER",
        "CMAKE_GENERATOR",
        "CMAKE_GENERATOR_INSTANCE",
        "CMAKE_GENERATOR_PLATFORM",
        "CMAKE_GENERATOR_TOOLSET",
        "CMAKE_LINKER",
        "CMAKE_RC_COMPILER",
        "CMAKE_SYSTEM_NAME",
        "CMAKE_SYSTEM_PROCESSOR",
        "CMAKE_SYSTEM_VERSION"
    )
    $projects = [ordered]@{}
    $toolPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($cacheFile in $cacheFiles) {
        $projectName = $cacheFile.Directory.Name
        $values = [ordered]@{}
        foreach ($line in [IO.File]::ReadLines($cacheFile.FullName)) {
            if ($line -match '^(?<key>[^:#]+):[^=]*=(?<value>.*)$') {
                $key = $Matches["key"]
                $value = $Matches["value"]
                if ($key -in $wantedCacheKeys) {
                    $values[$key] = $value
                    if ($key -in @("CMAKE_C_COMPILER", "CMAKE_CXX_COMPILER", "CMAKE_LINKER", "CMAKE_RC_COMPILER") -and
                        -not [string]::IsNullOrWhiteSpace($value)) {
                        $null = $toolPaths.Add($value)
                    }
                }
            }
        }
        $projectFile = Join-Path $cacheFile.Directory.FullName "ALL_BUILD.vcxproj"
        if (Test-Path -LiteralPath $projectFile -PathType Leaf) {
            $projectText = Get-Content -LiteralPath $projectFile -Raw
            foreach ($element in @("PlatformToolset", "WindowsTargetPlatformVersion")) {
                $matches = @([regex]::Matches($projectText, "<$element>(?<value>[^<]+)</$element>") |
                    ForEach-Object { $_.Groups["value"].Value } | Sort-Object -Unique)
                if ($matches.Count -gt 0) {
                    $values[$element] = $matches
                }
            }
            $toolsVersions = @([regex]::Matches($projectText, 'ToolsVersion="(?<value>[^"]+)"') |
                ForEach-Object { $_.Groups["value"].Value } | Sort-Object -Unique)
            if ($toolsVersions.Count -gt 0) {
                $values["VisualStudioProjectToolsVersion"] = $toolsVersions
            }
        }
        if ($values.Contains("CMAKE_GENERATOR_INSTANCE")) {
            foreach ($msbuildRelativePath in @(
                "MSBuild/Current/Bin/MSBuild.exe",
                "MSBuild/Current/Bin/amd64/MSBuild.exe"
            )) {
                $msbuildPath = Join-Path $values["CMAKE_GENERATOR_INSTANCE"] $msbuildRelativePath
                if (Test-Path -LiteralPath $msbuildPath -PathType Leaf) {
                    $null = $toolPaths.Add($msbuildPath)
                }
            }
        }
        $projects[$projectName] = $values
    }
    foreach ($path in @($toolPaths)) {
        if ([IO.Path]::GetFileName($path).Equals("link.exe", [StringComparison]::OrdinalIgnoreCase)) {
            $compilerPath = Join-Path ([IO.Path]::GetDirectoryName($path)) "cl.exe"
            if (Test-Path -LiteralPath $compilerPath -PathType Leaf) {
                $null = $toolPaths.Add($compilerPath)
            }
            $libraryManagerPath = Join-Path ([IO.Path]::GetDirectoryName($path)) "lib.exe"
            if (Test-Path -LiteralPath $libraryManagerPath -PathType Leaf) {
                $null = $toolPaths.Add($libraryManagerPath)
            }
        }
    }
    $toolExecutables = @($toolPaths | Sort-Object | ForEach-Object {
        Get-ToolExecutableIdentity $_
    })
    return [ordered]@{
        cmake = [ordered]@{
            executable = (Get-ToolExecutableIdentity $CmakePath)
            version = $CmakeVersion
        }
        projects = $projects
        tool_executables = $toolExecutables
    }
}

function Invoke-IsolatedRevisionBuild(
    [string]$RepositoryRoot,
    [string]$Revision,
    [string]$Label,
    [string]$DestinationDirectory
) {
    $scratch = [IO.Directory]::CreateTempSubdirectory().FullName
    $source = Join-Path $scratch "source"
    $target = Join-Path $scratch "target"
    $isolatedCargoHome = Join-Path $scratch "cargo-home"
    $archive = Join-Path $scratch "source.tar"
    $started = [DateTime]::UtcNow
    try {
        New-Item -ItemType Directory -Path $source -ErrorAction Stop | Out-Null
        New-Item -ItemType Directory -Path $isolatedCargoHome -ErrorAction Stop | Out-Null
        & git -C $RepositoryRoot archive --format=tar --output=$archive $Revision
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to archive revision $Revision."
        }
        & tar -xf $archive -C $source
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to extract revision $Revision."
        }
        $tree = (& git -C $RepositoryRoot rev-parse --verify "$Revision^{tree}").Trim()
        $cargoArguments = @(
            "build", "--release", "--locked", "-p", "izarravm", "-j", "8",
            "--target-dir", $target, "--message-format=json-render-diagnostics"
        )
        $ambientCargoConfigurations = @(Get-AmbientCargoConfigurationPaths $source)
        if ($ambientCargoConfigurations.Count -gt 0) {
            throw "An ambient Cargo configuration can alter the isolated build: $($ambientCargoConfigurations -join ', ')."
        }
        $cacheLinks = @()
        if ($null -ne $packageCacheCargoHome -and (Test-Path -LiteralPath $packageCacheCargoHome -PathType Container)) {
            foreach ($cacheName in @("registry", "git")) {
                $cacheSource = Join-Path $packageCacheCargoHome $cacheName
                if (Test-Path -LiteralPath $cacheSource -PathType Container) {
                    $cacheDestination = Join-Path $isolatedCargoHome $cacheName
                    New-Item -ItemType Junction -Path $cacheDestination -Target $cacheSource | Out-Null
                    $cacheLinks += $cacheName
                }
            }
        }
        $buildIsolationNames = @((Get-ChildItem Env: | Where-Object {
            $_.Name.StartsWith("CARGO_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name.StartsWith("CMAKE_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name.StartsWith("VCPKG_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name.StartsWith("PKG_CONFIG_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name.StartsWith("BINDGEN_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name.StartsWith("CRATE_CC_", [StringComparison]::OrdinalIgnoreCase) -or
            $_.Name -match '^(?:(?:HOST|TARGET)_)?(?:CC|CXX|AR|RANLIB|RC|ASM|CFLAGS|CXXFLAGS|ARFLAGS|RANLIBFLAGS|RCFLAGS|ASMFLAGS)(?:_.+)?$'
        } | ForEach-Object { $_.Name }) + @(
            "CARGO_HOME",
            "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "RUSTUP_TOOLCHAIN", "CC", "CFLAGS", "CXX", "CXXFLAGS", "LD", "LDFLAGS",
            "CL", "_CL_", "LINK", "_LINK_", "LIB", "INCLUDE"
        ) | Sort-Object -Unique)
        $savedBuildEnvironment = @{}
        try {
            foreach ($name in $buildIsolationNames) {
                $savedBuildEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
                Set-GateProcessEnvironment $name $null
            }
            Set-GateProcessEnvironment "CARGO_HOME" $isolatedCargoHome
            Write-Host "Building $Label from $($Revision.Substring(0, 12)) in an isolated target..."
            Push-Location $source
            try {
                $cargoPath = (Get-Command cargo -CommandType Application).Source
                $rustcPath = (Get-Command rustc -CommandType Application).Source
                $cmakePath = (Get-Command cmake -CommandType Application).Source
                $cargoVersion = (& cargo --version).Trim()
                $rustcVerboseVersion = @(& rustc -vV)
                $cmakeVersion = @(& cmake --version)
                $cargoOutput = @(& cargo @cargoArguments)
                $cargoExit = $LASTEXITCODE
                $ambientCargoConfigurationsAfter = @(Get-AmbientCargoConfigurationPaths $source)
                if ($ambientCargoConfigurationsAfter.Count -gt 0) {
                    throw "An ambient Cargo configuration appeared during the isolated build."
                }
            } finally {
                Pop-Location
            }
        } finally {
            foreach ($entry in $savedBuildEnvironment.GetEnumerator()) {
                Set-GateProcessEnvironment $entry.Key $entry.Value
            }
        }
        if ($cargoExit -ne 0) {
            throw "$Label release build failed."
        }
        $nativeToolchain = Get-NativeBuildToolchainIdentity $target $cmakePath $cmakeVersion
        $builtExecutable = $null
        foreach ($line in $cargoOutput) {
            try {
                $event = $line | ConvertFrom-Json -ErrorAction Stop
            } catch {
                continue
            }
            if ($event.reason -eq "compiler-artifact" -and $event.target.name -eq "izarravm" -and
                @($event.target.kind) -contains "bin" -and
                $null -ne $event.executable) {
                $builtExecutable = [string]$event.executable
            }
        }
        if ([string]::IsNullOrWhiteSpace($builtExecutable) -or
            -not (Test-Path -LiteralPath $builtExecutable -PathType Leaf)) {
            throw "$Label build did not report an executable artifact."
        }
        $builtExecutable = [IO.Path]::GetFullPath($builtExecutable)
        $targetPrefix = [IO.Path]::GetFullPath($target).TrimEnd(
            [IO.Path]::DirectorySeparatorChar
        ) + [IO.Path]::DirectorySeparatorChar
        if (-not $builtExecutable.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "$Label build reported an executable outside its isolated target directory."
        }
        $frozenPath = Join-Path $DestinationDirectory "$Label-izarravm.exe"
        Copy-Item -LiteralPath $builtExecutable -Destination $frozenPath -Force
        $frozenPath = (Resolve-Path -LiteralPath $frozenPath).Path
        $file = Get-Item -LiteralPath $frozenPath
        $hash = (Get-FileHash -LiteralPath $frozenPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $cargoLockHash = (Get-FileHash -LiteralPath (Join-Path $source "Cargo.lock") -Algorithm SHA256).Hash.ToLowerInvariant()
        $sourceCargoConfiguration = [ordered]@{}
        foreach ($relativeConfig in @(".cargo/config", ".cargo/config.toml")) {
            $configPath = Join-Path $source $relativeConfig
            if (Test-Path -LiteralPath $configPath -PathType Leaf) {
                $sourceCargoConfiguration[$relativeConfig] = (
                    Get-FileHash -LiteralPath $configPath -Algorithm SHA256
                ).Hash.ToLowerInvariant()
            }
        }
        $recipe = [ordered]@{
            recipe_id = "release-default-isolated-v2"
            cargo_arguments = $cargoArguments
            cargo_path = $cargoPath
            rustc_path = $rustcPath
            cargo_version = $cargoVersion
            rustc_verbose_version = $rustcVerboseVersion
            native_toolchain = $nativeToolchain
            source_cargo_configuration_sha256 = $sourceCargoConfiguration
            build_environment = [ordered]@{
                cargo_home = "temporary config-free home"
                ambient_cargo_configuration = "none in source ancestors"
                inherited_build_overrides = "cleared"
            }
        }
        $recipeFingerprint = Get-BuildRecipeFingerprint $recipe
        return [pscustomobject][ordered]@{
            requested_path = $null
            source_path = $null
            executed_copy_path = $frozenPath
            sha256 = $hash
            size = $file.Length
            built_this_invocation = $true
            verified = $true
            artifact_source = [ordered]@{
                head_commit = $Revision
                head_tree = $tree
                dirty = $false
                source_snapshot = "git-tree:$tree"
            }
            build = [ordered]@{
                recipe_id = $recipe.recipe_id
                recipe_fingerprint_sha256 = $recipeFingerprint
                cargo_arguments = $recipe.cargo_arguments
                cargo_lock_sha256 = $cargoLockHash
                cargo_path = $cargoPath
                rustc_path = $rustcPath
                cargo_version = $cargoVersion
                rustc_verbose_version = $rustcVerboseVersion
                native_toolchain = $nativeToolchain
                source_cargo_configuration_sha256 = $sourceCargoConfiguration
                package_cache_links = $cacheLinks
                build_environment = $recipe.build_environment
                started_utc = $started.ToString("o")
                finished_utc = [DateTime]::UtcNow.ToString("o")
            }
        }
    } finally {
        $resolvedScratch = [IO.Path]::GetFullPath($scratch)
        $resolvedTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        if ($resolvedScratch.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $resolvedScratch -Recurse -Force -ErrorAction SilentlyContinue
            if (Test-Path -LiteralPath $resolvedScratch) {
                Start-Sleep -Milliseconds 100
                Remove-Item -LiteralPath $resolvedScratch -Recurse -Force -ErrorAction SilentlyContinue
            }
            if (Test-Path -LiteralPath $resolvedScratch) {
                Write-Warning "Unable to remove isolated build scratch directory $resolvedScratch."
            }
        }
    }
}

function Get-UnverifiedArtifact(
    [string]$Path,
    [string]$Label,
    [string]$DestinationDirectory
) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Executable not found at $Path."
    }
    $requested = $Path
    $source = (Resolve-Path -LiteralPath $Path).Path
    $frozenPath = Join-Path $DestinationDirectory "$Label-izarravm.exe"
    Copy-Item -LiteralPath $source -Destination $frozenPath -Force
    $frozenPath = (Resolve-Path -LiteralPath $frozenPath).Path
    $file = Get-Item -LiteralPath $frozenPath
    return [pscustomobject][ordered]@{
        requested_path = $requested
        source_path = $source
        executed_copy_path = $frozenPath
        sha256 = (Get-FileHash -LiteralPath $frozenPath -Algorithm SHA256).Hash.ToLowerInvariant()
        size = $file.Length
        built_this_invocation = $false
        verified = $false
        artifact_source = $null
        build = $null
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

$doomRequiredInputs = @(
    "AUTOEXEC.BAT",
    "CONFIG.SYS",
    "JEMMEX.EXE",
    "DOOM/DOOM.EXE",
    "DOOM/DOOM1.WAD",
    "DOOM/MAX.CFG"
)
$quakeRequiredInputs = @(
    "AUTOEXEC.BAT",
    "CONFIG.SYS",
    "QUAKE/CWSDPMI.EXE",
    "QUAKE/QUAKE.EXE",
    "QUAKE/ID1/CONFIG.CFG",
    "QUAKE/ID1/PAK0.PAK"
)
$doomCanonicalTreeExclusions = @("EXITVM.COM")
$quakeCanonicalTreeExclusions = @("EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG")
$fixtureManifestPath = Join-Path $PSScriptRoot "realtime-gate-inputs.json"
if (-not (Test-Path -LiteralPath $fixtureManifestPath -PathType Leaf)) {
    throw "The accepted workload manifest is missing."
}
$fixtureManifestHash = (Get-FileHash -LiteralPath $fixtureManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$fixtureManifest = Get-Content -LiteralPath $fixtureManifestPath -Raw | ConvertFrom-Json
if ($fixtureManifest.schema -ne "izarravm-throughput-fixtures-v1" -or
    $null -eq $fixtureManifest.doom -or $null -eq $fixtureManifest.quake -or
    $null -eq $fixtureManifest.canonical_trees) {
    throw "The accepted workload manifest has an unexpected schema."
}
$fixtureManifestMatches = [ordered]@{}
if ($Workload -in @("Both", "Doom", "Doom586")) {
    $doomPreflightHashes = Get-WorkloadInputHashes $DoomFolder $doomRequiredInputs
    $doomPreflightTreeHash = Get-DirectoryTreeSha256 $DoomFolder $doomCanonicalTreeExclusions
    $fixtureManifestMatches.doom = [ordered]@{
        preflight_required_inputs = (Assert-WorkloadInputHashes `
            $doomPreflightHashes $fixtureManifest.doom "Doom fixture" (-not $ReportOnly))
        preflight_canonical_tree = (Assert-ExpectedSha256 `
            $doomPreflightTreeHash $fixtureManifest.canonical_trees.doom `
            "Doom fixture tree" (-not $ReportOnly))
    }
}
if ($Workload -in @("Both", "Quake")) {
    $quakePreflightHashes = Get-WorkloadInputHashes $QuakeFolder $quakeRequiredInputs
    $quakePreflightTreeHash = Get-DirectoryTreeSha256 $QuakeFolder $quakeCanonicalTreeExclusions
    $fixtureManifestMatches.quake = [ordered]@{
        preflight_required_inputs = (Assert-WorkloadInputHashes `
            $quakePreflightHashes $fixtureManifest.quake "Quake fixture" (-not $ReportOnly))
        preflight_canonical_tree = (Assert-ExpectedSha256 `
            $quakePreflightTreeHash $fixtureManifest.canonical_trees.quake `
            "Quake fixture tree" (-not $ReportOnly))
    }
}

$candidateArtifact = if ($artifactSelection -eq "isolated_build") {
    if ($repositoryAtSelection.dirty) {
        throw "An isolated revision build requires a clean worktree. Use ReportOnly with SkipBuild for dirty diagnostics."
    }
    Invoke-IsolatedRevisionBuild $repositoryRoot $revision "candidate" $ResultsDirectory
} else {
    Get-UnverifiedArtifact $Executable "candidate" $ResultsDirectory
}
$baselineArtifact = $null
if ($null -ne $baselineCommit) {
    $baselineArtifact = Invoke-IsolatedRevisionBuild $repositoryRoot $baselineCommit "baseline" $ResultsDirectory
}
$revisionPairedRun = $null -ne $baselineArtifact
$pairedRun = $BackendBakeoff -or $revisionPairedRun
if (-not $ReportOnly -and -not $pairedRun) {
    throw "The formal gate requires a freshly built baseline artifact."
}
if ($revisionPairedRun -and $candidateArtifact.verified -and $baselineArtifact.verified -and
    $candidateArtifact.build.recipe_fingerprint_sha256 -ne
        $baselineArtifact.build.recipe_fingerprint_sha256) {
    throw "Candidate and baseline were not built with the same isolated recipe and toolchain."
}

function Quote-ProcessArgument([string]$Value) {
    if ($Value -notmatch '[\s"]') {
        return $Value
    }
    return '"' + ($Value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-IzarraProcess(
    [string]$ExecutablePath,
    [string[]]$Arguments,
    [string]$StdoutPath,
    [string]$StderrPath,
    [string]$HomePath,
    [string]$BackendRole
) {
    if ($BackendBakeoff -and $BackendRole -notin @("automatic", "interpreter")) {
        throw "Unknown backend bakeoff role '$BackendRole'."
    }
    $argumentLine = ($Arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
    $childEnvironment = @{
        HOME = $HomePath
        USERPROFILE = $HomePath
        APPDATA = $HomePath
        LOCALAPPDATA = $HomePath
    }
    foreach ($name in $diagnosticVariables) {
        $childEnvironment[$name] = $null
    }
    $childEnvironment["IZARRAVM_JIT"] = if ($BackendBakeoff) {
        if ($BackendRole -eq "automatic") { "1" } else { "0" }
    } else {
        $Jit
    }
    $start = @{
        FilePath = $ExecutablePath
        ArgumentList = $argumentLine
        RedirectStandardOutput = $StdoutPath
        RedirectStandardError = $StderrPath
        WindowStyle = "Hidden"
        PassThru = $true
    }
    if ((Get-Command Start-Process).Parameters.ContainsKey("Environment")) {
        $start.Environment = $childEnvironment
    } elseif (-not $ReportOnly) {
        throw "The formal gate requires PowerShell Start-Process environment isolation."
    }
    $process = $null
    $effectiveAffinityMask = $null
    if ($ProcessorIndex -ge 0) {
        $spawnFailure = $null
        $restoreFailure = $null
        $gateProcess.Refresh()
        $parentMaskBeforeSpawn = $gateProcess.ProcessorAffinity.ToInt64()
        try {
            if (($parentMaskBeforeSpawn -band $requestedProcessorMask) -ne $requestedProcessorMask) {
                throw "The requested processor left the gate process affinity mask before launch."
            }
            $gateProcess.ProcessorAffinity = [IntPtr]$requestedProcessorMask
            $gateProcess.Refresh()
            if ($gateProcess.ProcessorAffinity.ToInt64() -ne $requestedProcessorMask) {
                throw "The gate process did not accept the requested one-processor affinity."
            }
            $process = Start-Process @start
            # Keep the native handle alive so ExitCode remains available after a fast child exit.
            $null = $process.Handle
            $process.Refresh()
            $effectiveAffinityMask = $process.ProcessorAffinity.ToInt64()
            if ($effectiveAffinityMask -ne $requestedProcessorMask) {
                throw "The benchmark child did not inherit the requested one-processor affinity."
            }
        } catch {
            $spawnFailure = $_
        } finally {
            try {
                $gateProcess.ProcessorAffinity = [IntPtr]$parentMaskBeforeSpawn
                $gateProcess.Refresh()
                if ($gateProcess.ProcessorAffinity.ToInt64() -ne $parentMaskBeforeSpawn) {
                    throw "The gate process affinity did not restore after child launch."
                }
            } catch {
                $restoreFailure = $_
            }
        }
        if ($null -ne $spawnFailure -or $null -ne $restoreFailure) {
            if ($null -ne $process) {
                try {
                    if (-not $process.HasExited) {
                        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                    }
                } finally {
                    $process.Dispose()
                }
            }
            if ($null -ne $spawnFailure) {
                if ($null -ne $restoreFailure) {
                    throw [InvalidOperationException]::new(
                        "$($spawnFailure.Exception.Message) Parent affinity restoration also failed: $($restoreFailure.Exception.Message)",
                        $spawnFailure.Exception
                    )
                }
                throw $spawnFailure
            }
            throw $restoreFailure
        }
        $verifiedChildAffinityMasks.Add((Format-AffinityMask $effectiveAffinityMask))
    } else {
        $process = Start-Process @start
        # Keep the native handle alive so ExitCode remains available after a fast child exit.
        $null = $process.Handle
    }
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
    foreach ($path in @($StdoutPath, $StderrPath)) {
        if ((Test-Path -LiteralPath $path -PathType Leaf) -and
            (Get-Item -LiteralPath $path).Length -gt 64MB) {
            throw "IzarraVM produced more than 64 MiB of diagnostic output."
        }
    }
    if ($null -eq $process.ExitCode) {
        throw "IzarraVM exited without a readable process exit code."
    }
    return [pscustomobject][ordered]@{
        exit_code = [int]$process.ExitCode
        processor_index = if ($ProcessorIndex -ge 0) { $ProcessorIndex } else { $null }
        processor_affinity_mask = if ($null -ne $effectiveAffinityMask) {
            Format-AffinityMask $effectiveAffinityMask
        } else {
            $null
        }
        processor_affinity_verified = $ProcessorIndex -ge 0
    }
}

$exitVmBytes = [byte[]](
    0xB0, 0x0C, 0xE6, 0xE4,
    0xB0, 0x00, 0xE6, 0xE5,
    0xB0, 0x03, 0xE6, 0xE6,
    0xF4, 0xEB, 0xFD
)
$exitVmHash = Get-BytesSha256 $exitVmBytes
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-gate-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

function Remove-GateTemporaryRoot([string]$Path) {
    $resolvedTemporaryRoot = [IO.Path]::GetFullPath($Path)
    $resolvedSystemTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolvedTemporaryRoot.StartsWith($resolvedSystemTemp, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove workload scratch outside the system temporary directory."
    }
    Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $resolvedTemporaryRoot) {
        Start-Sleep -Milliseconds 100
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $resolvedTemporaryRoot) {
        Write-Warning "Unable to remove workload scratch directory $resolvedTemporaryRoot."
    }
}

function New-FrozenWorkload([string]$Source, [string]$Label) {
    $initialHash = Get-DirectoryTreeSha256 $Source
    $frozenPath = Join-Path $temporaryRoot "frozen-$Label"
    Copy-Item -LiteralPath $Source -Destination $frozenPath -Recurse -Force
    $sourceHashAfterCopy = Get-DirectoryTreeSha256 $Source
    $frozenHash = Get-DirectoryTreeSha256 $frozenPath
    if ($initialHash -ne $sourceHashAfterCopy -or $initialHash -ne $frozenHash) {
        throw "$Label workload changed while its immutable measurement snapshot was created."
    }
    return [pscustomobject][ordered]@{
        source_path = $Source
        frozen_path = $frozenPath
        source_initial_sha256 = $initialHash
        source_after_copy_sha256 = $sourceHashAfterCopy
        frozen_sha256 = $frozenHash
    }
}

$workloadInputHashes = [ordered]@{}
$workloadTreeHashes = [ordered]@{}
$workloadCanonicalTreeHashes = [ordered]@{}
$doomSnapshot = $null
$quakeSnapshot = $null
try {
    if ($Workload -in @("Both", "Doom", "Doom586")) {
        $doomSnapshot = New-FrozenWorkload $DoomFolder "doom"
        $doomAutoexec = Join-Path $doomSnapshot.frozen_path "AUTOEXEC.BAT"
        if (-not (Select-String -LiteralPath $doomAutoexec -SimpleMatch "C:\EXITVM.COM" -Quiet)) {
            throw "The Doom fixture must run C:\EXITVM.COM after the timedemo."
        }
        $doomInputHashes = Get-WorkloadInputHashes $doomSnapshot.frozen_path $doomRequiredInputs
        $fixtureManifestMatches.doom["frozen_required_inputs"] = Assert-WorkloadInputHashes `
            $doomInputHashes $fixtureManifest.doom "Frozen Doom fixture" (-not $ReportOnly)
        $doomFrozenCanonicalHash = Get-DirectoryTreeSha256 `
            $doomSnapshot.frozen_path $doomCanonicalTreeExclusions
        $workloadCanonicalTreeHashes.doom = $doomFrozenCanonicalHash
        $fixtureManifestMatches.doom["frozen_canonical_tree"] = Assert-ExpectedSha256 `
            $doomFrozenCanonicalHash $fixtureManifest.canonical_trees.doom `
            "Frozen Doom fixture tree" (-not $ReportOnly)
        if ($Workload -eq "Doom586") {
            $workloadInputHashes.doom_586 = $doomInputHashes
        } elseif ($Workload -eq "Both") {
            $workloadInputHashes.doom_486 = $doomInputHashes
            $workloadInputHashes.doom_586 = $doomInputHashes
        } else {
            $workloadInputHashes.doom_486 = $doomInputHashes
        }
        $workloadTreeHashes.doom = $doomSnapshot
    }
    if ($Workload -in @("Both", "Quake")) {
        $quakeSnapshot = New-FrozenWorkload $QuakeFolder "quake"
        Assert-QuakeAutoexecText (Get-Content -LiteralPath (
            Join-Path $quakeSnapshot.frozen_path "AUTOEXEC.BAT"
        ) -Raw)
        $workloadInputHashes.quake_586 = Get-WorkloadInputHashes `
            $quakeSnapshot.frozen_path $quakeRequiredInputs
        $fixtureManifestMatches.quake["frozen_required_inputs"] = Assert-WorkloadInputHashes `
            $workloadInputHashes.quake_586 $fixtureManifest.quake "Frozen Quake fixture" (-not $ReportOnly)
        $quakeFrozenCanonicalHash = Get-DirectoryTreeSha256 `
            $quakeSnapshot.frozen_path $quakeCanonicalTreeExclusions
        $workloadCanonicalTreeHashes.quake = $quakeFrozenCanonicalHash
        $fixtureManifestMatches.quake["frozen_canonical_tree"] = Assert-ExpectedSha256 `
            $quakeFrozenCanonicalHash $fixtureManifest.canonical_trees.quake `
            "Frozen Quake fixture tree" (-not $ReportOnly)
        $workloadTreeHashes.quake = $quakeSnapshot
    }
} catch {
    Remove-GateTemporaryRoot $temporaryRoot
    throw
}

function Invoke-Observation(
    $Policy,
    [string]$SourceFolder,
    [string]$Role,
    [string]$ObservationId,
    [string]$ExecutablePath
) {
    $context = "$($Policy.name) $Role $ObservationId"
    $fixture = Join-Path $temporaryRoot "$($Policy.name)-$Role-$ObservationId"
    $observationHome = Join-Path $temporaryRoot "home-$($Policy.name)-$Role-$ObservationId"
    Copy-Item -LiteralPath $SourceFolder -Destination $fixture -Recurse
    New-Item -ItemType Directory -Path $observationHome | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $fixture "EXITVM.COM"), $exitVmBytes)
    $qconsole = Join-Path $fixture "QUAKE/ID1/QCONSOLE.LOG"
    if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
        Remove-Item -LiteralPath $qconsole
    }

    $fileStem = "$($Policy.name)-$Role-$ObservationId"
    $jsonPath = Join-Path $ResultsDirectory "$fileStem.json"
    $stdoutPath = Join-Path $ResultsDirectory "$fileStem.stdout.log"
    $stderrPath = Join-Path $ResultsDirectory "$fileStem.stderr.log"
    Remove-Item -LiteralPath $jsonPath -Force -ErrorAction SilentlyContinue
    $processArguments = @(
        "--cpu", $Policy.mode,
        "--memory-mib", "24",
        "--video", "vega",
        "--hdd-folder", $fixture,
        "--cycles", $Policy.cycle_budget.ToString(),
        "--dump-result",
        "--profile-json", $jsonPath
    )
    if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        $processArguments += "--expect-test-exit"
    }
    if ($BackendBakeoff -and $Role -eq "interpreter") {
        $processArguments += "--interpreter"
    }
    $processResult = Invoke-IzarraProcess `
        $ExecutablePath $processArguments $stdoutPath $stderrPath $observationHome $Role
    if ($processResult.exit_code -ne 0 -and -not $BackendBakeoff) {
        throw "$context failed with exit code $($processResult.exit_code). See $stdoutPath and $stderrPath."
    }
    if (-not (Test-Path -LiteralPath $jsonPath -PathType Leaf)) {
        throw "$context did not produce its profile JSON."
    }
    $profileHash = if ($BackendBakeoff) { Get-FileSha256 $jsonPath } else { $null }
    $sample = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json
    if ($sample.schema -ne "izarravm-hdd-profile-v1" -or $sample.mode -ne $Policy.mode) {
        throw "$context produced an unexpected schema or CPU mode."
    }
    Assert-UninstrumentedProfileSample $sample $context
    if (-not $BackendBakeoff -and
        $Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if ($sample.stop.kind -ne "test_exit" -or $sample.stop.code -ne 0) {
            throw "$context did not reach TestExit code 0."
        }
    } elseif (-not $BackendBakeoff -and
        ($sample.stop.kind -ne "cycle_limit" -or
         [uint64]$sample.stop.requested -ne $Policy.cycle_budget)) {
        throw "$context did not reach its fixed cycle limit."
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
        throw "$context reported a non-positive timing or instruction metric."
    }
    if ($directCoverage -lt 0 -or $directCoverage -gt 1 -or $directExitsPer100 -lt 0 -or
        $directEntries -lt 0 -or $directInstructions -lt 0 -or $directSideExits -lt 0) {
        throw "$context reported an out-of-range direct JIT metric."
    }
    if ($directInstructions -gt $instructions) {
        throw "$context retired more direct instructions than total instructions."
    }
    if ($directSideExits -gt $directEntries) {
        throw "$context reported more direct side exits than direct entries."
    }
    Assert-NearlyEqual $realTimeFactor ($guestSeconds / $wallSeconds) "real_time_factor"
    Assert-NearlyEqual $instructionsPerSecond ($instructions / $wallSeconds) "instructions_per_host_second"
    Assert-NearlyEqual $directCoverage ($directInstructions / $instructions) "direct_native_coverage"
    Assert-NearlyEqual $directExitsPer100 (100.0 * $directSideExits / $instructions) "direct_slow_exits_per_100_instructions"
    $preservedQconsole = $null
    $qconsoleHash = $null
    if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if (-not $BackendBakeoff -and
            ($null -eq $sample.timedemo -or $sample.timedemo.gametics -ne 2134 -or
             $sample.timedemo.realtics -lt $Policy.minimum_realtics -or
             $sample.timedemo.realtics -gt $Policy.maximum_realtics)) {
            throw "$context failed its 2134-gametic timing identity check."
        }
        if ($null -ne $sample.timedemo -and $sample.timedemo.realtics -gt 0) {
            $doomFps = 35.0 * $sample.timedemo.gametics / $sample.timedemo.realtics
            $sample | Add-Member -NotePropertyName doom_fps -NotePropertyValue $doomFps
        }
    } else {
        $preservedQconsole = Join-Path $ResultsDirectory "$fileStem-qconsole.log"
        Remove-Item -LiteralPath $preservedQconsole -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
            Copy-Item -LiteralPath $qconsole -Destination $preservedQconsole
            if ($BackendBakeoff) {
                $qconsoleHash = Get-FileSha256 $preservedQconsole
            }
        }
        if ($BackendBakeoff) {
            $quakeIdentities = @(Read-QuakeTimedemoIdentities $preservedQconsole)
            $quakeIdentity = if ($quakeIdentities.Count -eq 1) {
                $quakeIdentities[0]
            } else {
                $null
            }
            $sample | Add-Member `
                -NotePropertyName quake_timedemo_identity_count `
                -NotePropertyValue $quakeIdentities.Count
            $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
        } else {
            $quakeIdentity = Read-QuakeTimedemoIdentity $preservedQconsole
            $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
        }
    }
    if ($BackendBakeoff) {
        $resultBlock = Get-NormalizedResultBlock $stdoutPath
        $sample | Add-Member `
            -NotePropertyName gate_process_exit_code `
            -NotePropertyValue $processResult.exit_code
        $sample | Add-Member -NotePropertyName gate_backend_policy -NotePropertyValue $Role
        $sample | Add-Member -NotePropertyName gate_termination_policy -NotePropertyValue $(
            if ($Policy.name -eq "quake-586") { "fixed_cycle_limit" } else { "lotura_test_exit" }
        )
        $sample | Add-Member -NotePropertyName gate_artifacts -NotePropertyValue ([pscustomobject][ordered]@{
            profile_json_file = [IO.Path]::GetFileName($jsonPath)
            profile_json_sha256 = $profileHash
            stdout_file = [IO.Path]::GetFileName($stdoutPath)
            stdout_sha256 = Get-FileSha256 $stdoutPath
            stderr_file = [IO.Path]::GetFileName($stderrPath)
            stderr_sha256 = Get-FileSha256 $stderrPath
            qconsole_file = if ($null -ne $preservedQconsole) {
                [IO.Path]::GetFileName($preservedQconsole)
            } else {
                $null
            }
            qconsole_sha256 = $qconsoleHash
            result_block_status = $resultBlock.status
            result_block_count = $resultBlock.block_count
            result_block_sha256 = $resultBlock.sha256
            result_block_normalized_bytes = $resultBlock.normalized_bytes
        })
    }
    $sample | Add-Member -NotePropertyName gate_role -NotePropertyValue $Role
    $sample | Add-Member -NotePropertyName gate_observation -NotePropertyValue $ObservationId
    $sample | Add-Member -NotePropertyName gate_processor_index -NotePropertyValue $processResult.processor_index
    $sample | Add-Member -NotePropertyName gate_processor_affinity_mask -NotePropertyValue $processResult.processor_affinity_mask
    $sample | Add-Member -NotePropertyName gate_processor_affinity_verified -NotePropertyValue $processResult.processor_affinity_verified
    return $sample
}

function Get-RoleSummary(
    [string]$Name,
    [string]$Mode,
    [object[]]$Samples,
    [bool]$EnforceDeterminism = $true
) {
    if ($EnforceDeterminism) {
        Assert-RoleDeterminism $Name $Samples
    }
    return [ordered]@{
        name = $Name
        mode = $Mode
        runs = $Samples
        median = [ordered]@{
            wall_seconds = Get-Median ([double[]]$Samples.wall_seconds)
            guest_seconds = Get-Median ([double[]]$Samples.guest_seconds)
            real_time_factor = Get-Median ([double[]]$Samples.real_time_factor)
            instructions_per_host_second = Get-Median ([double[]]$Samples.instructions_per_host_second)
            direct_native_coverage = Get-Median ([double[]]$Samples.direct_native_coverage)
            direct_slow_exits_per_100_instructions = Get-Median ([double[]]$Samples.direct_slow_exits_per_100_instructions)
        }
    }
}

function Get-PairedWorkloadSummary($Policy, [object[]]$Candidate, [object[]]$Baseline) {
    if ($Candidate.Count -ne $Baseline.Count) {
        throw "$($Policy.name) has incomplete pairs."
    }
    $ipsRatios = @()
    $rtfRatios = @()
    $pairs = for ($index = 0; $index -lt $Candidate.Count; $index++) {
        $ipsRatio = $Candidate[$index].instructions_per_host_second / $Baseline[$index].instructions_per_host_second
        $rtfRatio = $Candidate[$index].real_time_factor / $Baseline[$index].real_time_factor
        $ipsRatios += $ipsRatio
        $rtfRatios += $rtfRatio
        [ordered]@{
            pair = $index + 1
            ips_ratio = $ipsRatio
            real_time_factor_ratio = $rtfRatio
        }
    }
    $candidateChecks = Get-CandidateSampleChecks $Policy $Candidate
    return [ordered]@{
        name = $Policy.name
        mode = $Policy.mode
        minimum_real_time_factor = $Policy.minimum_real_time_factor
        candidate = Get-RoleSummary $Policy.name $Policy.mode $Candidate
        baseline = Get-RoleSummary $Policy.name $Policy.mode $Baseline
        pairs = $pairs
        paired_metrics = [ordered]@{
            instructions_per_host_second = Get-PairedMetric ([double[]]$ipsRatios)
            real_time_factor = Get-PairedMetric ([double[]]$rtfRatios)
        }
        candidate_sample_checks = $candidateChecks
        candidate_floor_passes = $candidateChecks.real_time_floor_passes
    }
}

function Get-BackendCompatibilityReasons($Policy, [string]$Role, [object[]]$Samples) {
    $reasons = @()
    foreach ($sample in $Samples) {
        $label = "$Role $($sample.gate_observation)"
        if ($sample.gate_process_exit_code -ne 0) {
            $reasons += "$label host exit code is $($sample.gate_process_exit_code)"
        }
        if ($sample.gate_artifacts.result_block_status -ne "valid" -or
            $sample.gate_artifacts.result_block_count -ne 1 -or
            [string]$sample.gate_artifacts.result_block_sha256 -notmatch '^[0-9a-f]{64}$') {
            $reasons += "$label did not produce exactly one hashable semantic result block"
        }
        if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
            if ((Get-StopIdentityKey $sample) -cne "test_exit|code=0|requested=|message=") {
                $reasons += "$label did not reach Lotura TestExit code 0"
            }
            if ($null -eq $sample.timedemo -or $sample.timedemo.gametics -ne 2134 -or
                $sample.timedemo.realtics -le 0) {
                $reasons += "$label did not report the 2134-gametic Doom demo"
            }
        } else {
            $expectedStop = "cycle_limit|code=|requested=$($Policy.cycle_budget)|message="
            if ((Get-StopIdentityKey $sample) -cne $expectedStop) {
                $reasons += "$label did not reach the fixed $($Policy.cycle_budget)-clock limit"
            }
            if ($sample.quake_timedemo_identity_count -ne 1 -or
                $null -eq $sample.quake_timedemo -or
                $sample.quake_timedemo.frames -ne 969 -or
                $sample.quake_timedemo.seconds -le 0 -or
                $sample.quake_timedemo.fps -le 0) {
                $reasons += "$label did not produce exactly one valid 969-frame Quake identity"
            } elseif ([Math]::Abs(
                $sample.quake_timedemo.frames / $sample.quake_timedemo.seconds -
                    $sample.quake_timedemo.fps
            ) -gt 0.2) {
                $reasons += "$label reported inconsistent Quake seconds and fps"
            }
        }
    }
    return @($reasons)
}

function Get-BackendCalibrationReasons($Policy, [string]$Role, [object[]]$Samples) {
    $reasons = @()
    foreach ($sample in $Samples) {
        $label = "$Role $($sample.gate_observation)"
        if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
            if ($null -eq $sample.timedemo -or
                $sample.timedemo.realtics -lt $Policy.minimum_realtics -or
                $sample.timedemo.realtics -gt $Policy.maximum_realtics) {
                $reasons += "$label is outside the Doom realtics calibration band"
            }
        } elseif ($null -eq $sample.quake_timedemo -or
            $sample.quake_timedemo.fps -lt 41.0 -or
            $sample.quake_timedemo.fps -gt 44.0) {
            $reasons += "$label is outside the Quake 41-44 fps calibration band"
        }
    }
    return @($reasons)
}

function Get-BackendSelectionReasons(
    [object[]]$Automatic,
    [object[]]$Interpreter
) {
    $reasons = @()
    foreach ($sample in $Automatic) {
        if ($sample.perf.jit_direct_entries -le 0 -or $sample.perf.jit_direct_insns -le 0) {
            $reasons += "automatic $($sample.gate_observation) did not execute the direct backend"
        }
    }
    $zeroFields = @(
        "jit_region_entries", "jit_region_insns", "jit_native_insns",
        "jit_direct_entries", "jit_direct_insns"
    )
    foreach ($sample in $Interpreter) {
        foreach ($field in $zeroFields) {
            if ($sample.perf.$field -ne 0) {
                $reasons += "interpreter $($sample.gate_observation) reported nonzero $field"
            }
        }
    }
    return @($reasons)
}

function Get-BackendWorkloadSummary(
    $Policy,
    [object[]]$Automatic,
    [object[]]$Interpreter,
    [bool]$IsScreening
) {
    if ($Automatic.Count -ne $Interpreter.Count) {
        throw "$($Policy.name) has incomplete backend pairs."
    }
    $ipsRatios = @()
    $rtfRatios = @()
    $equalWorkFailures = @()
    $pairs = for ($index = 0; $index -lt $Automatic.Count; $index++) {
        $ipsRatio = $Automatic[$index].instructions_per_host_second /
            $Interpreter[$index].instructions_per_host_second
        $rtfRatio = $Automatic[$index].real_time_factor /
            $Interpreter[$index].real_time_factor
        $ipsRatios += $ipsRatio
        $rtfRatios += $rtfRatio
        $automaticWork = Get-EqualWorkRecord $Policy.name $Automatic[$index]
        $interpreterWork = Get-EqualWorkRecord $Policy.name $Interpreter[$index]
        $equalWork = Compare-EqualWorkRecords $automaticWork $interpreterWork
        if ($Automatic[$index].gate_artifacts.result_block_status -ne "valid" -or
            $Interpreter[$index].gate_artifacts.result_block_status -ne "valid") {
            $equalWork = [pscustomobject][ordered]@{
                matches = $false
                mismatched_fields = @(
                    @($equalWork.mismatched_fields) + "result_block_identity" |
                        Sort-Object -Unique
                )
            }
            $equalWorkFailures += "pair $($index + 1): semantic result block is invalid"
        }
        if (-not $equalWork.matches) {
            $equalWorkFailures += "pair $($index + 1): $($equalWork.mismatched_fields -join ', ')"
        }
        [ordered]@{
            pair = $index + 1
            automatic_observation = $Automatic[$index].gate_observation
            interpreter_observation = $Interpreter[$index].gate_observation
            ips_ratio = $ipsRatio
            real_time_factor_ratio = $rtfRatio
            equal_work = $equalWork
        }
    }

    $automaticDeterminism = Get-RoleExactDeterminism $Policy.name $Automatic
    $interpreterDeterminism = Get-RoleExactDeterminism $Policy.name $Interpreter
    if (-not $automaticDeterminism.deterministic) {
        $equalWorkFailures += "automatic role: $($automaticDeterminism.mismatched_fields -join ', ')"
    }
    if (-not $interpreterDeterminism.deterministic) {
        $equalWorkFailures += "interpreter role: $($interpreterDeterminism.mismatched_fields -join ', ')"
    }

    $compatibilityReasons = @(
        @(Get-BackendCompatibilityReasons $Policy "automatic" $Automatic) +
        @(Get-BackendCompatibilityReasons $Policy "interpreter" $Interpreter)
    )
    $calibrationReasons = @(
        @(Get-BackendCalibrationReasons $Policy "automatic" $Automatic) +
        @(Get-BackendCalibrationReasons $Policy "interpreter" $Interpreter)
    )
    $backendReasons = @(Get-BackendSelectionReasons $Automatic $Interpreter)
    $ipsMetric = Get-BackendPairedMetric ([double[]]$ipsRatios)
    $rtfMetric = Get-BackendPairedMetric ([double[]]$rtfRatios)
    $survivalReasons = @()
    if ($ipsMetric.verdict -ne "pass") {
        $survivalReasons += "IPS survival threshold failed"
    }
    if ($rtfMetric.verdict -ne "pass") {
        $survivalReasons += "real-time-factor survival threshold failed"
    }

    $requiredFloorPasses = if ($IsScreening) { 2 } else { $minimumFloorPasses }
    $productFloorPasses = @($Automatic | Where-Object {
        $_.real_time_factor -ge $Policy.minimum_real_time_factor
    }).Count
    $productReasons = @()
    if ($productFloorPasses -lt $requiredFloorPasses) {
        $productReasons += "$productFloorPasses of $($Automatic.Count) automatic samples meet the product floor"
    }

    return [ordered]@{
        name = $Policy.name
        mode = $Policy.mode
        configuration = [ordered]@{
            cpu_mode = $Policy.mode
            memory_mib = 24
            video = "vega"
            cycle_budget = $Policy.cycle_budget
        }
        termination_policy = if ($Policy.name -eq "quake-586") {
            "fixed_cycle_limit_with_post_demo_tail"
        } else {
            "lotura_test_exit_after_timedemo"
        }
        minimum_real_time_factor = $Policy.minimum_real_time_factor
        automatic = Get-RoleSummary "automatic" $Policy.mode $Automatic $false
        interpreter = Get-RoleSummary "interpreter" $Policy.mode $Interpreter $false
        pairs = $pairs
        paired_metrics = [ordered]@{
            instructions_per_host_second = $ipsMetric
            real_time_factor = $rtfMetric
        }
        survival = [ordered]@{
            verdict = if ($survivalReasons.Count -eq 0) { "pass" } else { "fail" }
            failure_reasons = $survivalReasons
        }
        equal_work = [ordered]@{
            verdict = if ($equalWorkFailures.Count -eq 0) { "pass" } else { "fail" }
            automatic_role_determinism = $automaticDeterminism
            interpreter_role_determinism = $interpreterDeterminism
            failure_reasons = $equalWorkFailures
        }
        verdicts = [ordered]@{
            product = if ($productReasons.Count -eq 0) { "pass" } else { "fail" }
            equal_work = if ($equalWorkFailures.Count -eq 0) { "pass" } else { "fail" }
            calibration = if ($calibrationReasons.Count -eq 0) { "pass" } else { "fail" }
            backend_health = if ($backendReasons.Count -eq 0) { "pass" } else { "fail" }
            compatibility = if ($compatibilityReasons.Count -eq 0) { "pass" } else { "fail" }
        }
        checks = [ordered]@{
            product = [ordered]@{
                required_floor_passes = $requiredFloorPasses
                actual_floor_passes = $productFloorPasses
                failure_reasons = $productReasons
            }
            calibration = [ordered]@{ failure_reasons = $calibrationReasons }
            backend_health = [ordered]@{ failure_reasons = $backendReasons }
            compatibility = [ordered]@{ failure_reasons = $compatibilityReasons }
        }
    }
}

$knownDiagnosticVariables = @(
    "IZARRAVM_AUDIO_DEBUG", "IZARRAVM_CPU_PROFILE", "IZARRAVM_DECODE_CACHE_LINES",
    "IZARRAVM_DIFF_TRACE", "IZARRAVM_DUMP_LINEAR", "IZARRAVM_FAULT_TRACE",
    "IZARRAVM_JIT_FOLD", "IZARRAVM_JIT_REGION", "IZARRAVM_MACHINE_PROFILE",
    "IZARRAVM_RUNTIME_PROFILE", "RUST_LOG"
)
$inheritedIzarraVariables = @(Get-ChildItem Env: | Where-Object {
    $_.Name.StartsWith("IZARRAVM_", [StringComparison]::OrdinalIgnoreCase)
} | ForEach-Object { $_.Name })
$diagnosticVariables = @(($knownDiagnosticVariables + $inheritedIzarraVariables) |
    Where-Object { $_ -ne "IZARRAVM_JIT" } | Sort-Object -Unique)
$savedEnvironment = @{}
$candidateExecutableLock = $null
$baselineExecutableLock = $null
$candidateHashAfter = $null
$baselineHashAfter = $null
$measurementFailure = $null
$outerAffinityRestoreFailure = $null
try {
    $savedEnvironment["IZARRAVM_JIT"] = [Environment]::GetEnvironmentVariable("IZARRAVM_JIT", "Process")
    foreach ($name in $diagnosticVariables) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        Set-GateProcessEnvironment $name $null
    }
    $gateJitEnvironment = if ($BackendBakeoff) { $null } else { $Jit }
    Set-GateProcessEnvironment "IZARRAVM_JIT" $gateJitEnvironment
    $candidateExecutableLock = [IO.File]::Open(
        $candidateArtifact.executed_copy_path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    if ($revisionPairedRun) {
        $baselineExecutableLock = [IO.File]::Open(
            $baselineArtifact.executed_copy_path,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
    }
    $policies = Get-WorkloadPolicies $Workload
    $pairRoles = if ($BackendBakeoff) {
        @("automatic", "interpreter")
    } else {
        @("candidate", "baseline")
    }
    $observations = [ordered]@{}
    foreach ($policy in $policies) {
        $roleBuckets = [ordered]@{}
        foreach ($role in $pairRoles) {
            $roleBuckets[$role] = @()
        }
        $observations[$policy.name] = $roleBuckets
    }

    if ($pairedRun) {
        foreach ($policy in $policies) {
            $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                $doomSnapshot.frozen_path
            } else {
                $quakeSnapshot.frozen_path
            }
            foreach ($role in (Get-PairOrder 1 $PairSeed $pairRoles)) {
                $artifact = if ($BackendBakeoff -or $role -eq "candidate") {
                    $candidateArtifact
                } else {
                    $baselineArtifact
                }
                $null = Invoke-Observation $policy $sourceFolder $role "warmup" $artifact.executed_copy_path
            }
        }
        for ($pair = 1; $pair -le $Runs; $pair++) {
            $roleOrder = Get-PairOrder $pair $PairSeed $pairRoles
            for ($workloadOffset = 0; $workloadOffset -lt $policies.Count; $workloadOffset++) {
                $policy = $policies[($workloadOffset + $pair - 1) % $policies.Count]
                $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                    $doomSnapshot.frozen_path
                } else {
                    $quakeSnapshot.frozen_path
                }
                foreach ($role in $roleOrder) {
                    $artifact = if ($BackendBakeoff -or $role -eq "candidate") {
                        $candidateArtifact
                    } else {
                        $baselineArtifact
                    }
                    $sample = Invoke-Observation $policy $sourceFolder $role "pair$pair" $artifact.executed_copy_path
                    $bucket = $observations[$policy.name]
                    $bucket[$role] += $sample
                }
            }
        }
    } else {
        for ($run = 1; $run -le $Runs; $run++) {
            foreach ($policy in $policies) {
                $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                    $doomSnapshot.frozen_path
                } else {
                    $quakeSnapshot.frozen_path
                }
                $sample = Invoke-Observation $policy $sourceFolder "candidate" "run$run" $candidateArtifact.executed_copy_path
                $observations[$policy.name]["candidate"] += $sample
            }
        }
    }

    $workloads = @()
    foreach ($policy in $policies) {
        $bucket = $observations[$policy.name]
        if ($BackendBakeoff) {
            $workloads += Get-BackendWorkloadSummary `
                $policy $bucket.automatic $bucket.interpreter ([bool]$Screening)
        } elseif ($pairedRun) {
            $workloads += Get-PairedWorkloadSummary $policy $bucket.candidate $bucket.baseline
        } else {
            $candidateSummary = Get-RoleSummary $policy.name $policy.mode $bucket.candidate
            $candidateChecks = Get-CandidateSampleChecks $policy $bucket.candidate
            $workloads += [ordered]@{
                name = $policy.name
                mode = $policy.mode
                minimum_real_time_factor = $policy.minimum_real_time_factor
                candidate = $candidateSummary
                baseline = $null
                pairs = @()
                paired_metrics = $null
                candidate_sample_checks = $candidateChecks
                candidate_floor_passes = $candidateChecks.real_time_floor_passes
            }
        }
    }
    $candidateHashAfter = (Get-FileHash -LiteralPath $candidateArtifact.executed_copy_path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($revisionPairedRun) {
        $baselineHashAfter = (Get-FileHash -LiteralPath $baselineArtifact.executed_copy_path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    if ($null -ne $doomSnapshot -and
        (Get-DirectoryTreeSha256 $doomSnapshot.frozen_path) -ne $doomSnapshot.frozen_sha256) {
        throw "The frozen Doom workload changed during the gate."
    }
    if ($null -ne $quakeSnapshot -and
        (Get-DirectoryTreeSha256 $quakeSnapshot.frozen_path) -ne $quakeSnapshot.frozen_sha256) {
        throw "The frozen Quake workload changed during the gate."
    }
} catch {
    $measurementFailure = $_
} finally {
    if ($ProcessorIndex -ge 0) {
        try {
            $gateProcess.ProcessorAffinity = [IntPtr]$originalGateAffinity
            $gateProcess.Refresh()
            if ($gateProcess.ProcessorAffinity.ToInt64() -ne $originalGateAffinity) {
                throw "The gate process affinity did not restore to its entry mask."
            }
        } catch {
            $outerAffinityRestoreFailure = $_
        }
    }
    if ($null -ne $baselineExecutableLock) {
        $baselineExecutableLock.Dispose()
    }
    if ($null -ne $candidateExecutableLock) {
        $candidateExecutableLock.Dispose()
    }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        Set-GateProcessEnvironment $entry.Key $entry.Value
    }
    Remove-GateTemporaryRoot $temporaryRoot
}
if ($null -ne $measurementFailure) {
    if ($null -ne $outerAffinityRestoreFailure) {
        throw [InvalidOperationException]::new(
            "$($measurementFailure.Exception.Message) Gate affinity restoration also failed: $($outerAffinityRestoreFailure.Exception.Message)",
            $measurementFailure.Exception
        )
    }
    throw $measurementFailure
}
if ($null -ne $outerAffinityRestoreFailure) {
    throw $outerAffinityRestoreFailure
}

if ($ProcessorIndex -ge 0) {
    $expectedVerifiedChildren = if ($pairedRun) {
        $policies.Count * (2 + 2 * $Runs)
    } else {
        $policies.Count * $Runs
    }
    if ($verifiedChildAffinityMasks.Count -ne $expectedVerifiedChildren) {
        throw "Not every warmup and measured child received a verified processor affinity."
    }
}

if ($candidateHashAfter -ne $candidateArtifact.sha256) {
    throw "The frozen candidate executable changed during the gate."
}
if ($revisionPairedRun) {
    if ($baselineHashAfter -ne $baselineArtifact.sha256) {
        throw "The frozen baseline executable changed during the gate."
    }
}
if ($null -ne $doomSnapshot -and
    (Get-DirectoryTreeSha256 $DoomFolder) -ne $doomSnapshot.source_initial_sha256) {
    throw "The Doom workload tree changed during the gate."
}
if ($null -ne $quakeSnapshot -and
    (Get-DirectoryTreeSha256 $QuakeFolder) -ne $quakeSnapshot.source_initial_sha256) {
    throw "The Quake workload tree changed during the gate."
}
$gateScriptHashAfter = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($gateScriptHashAfter -ne $gateScriptHash) {
    throw "The throughput gate script changed during the measurement."
}
$fixtureManifestHashAfter = (Get-FileHash -LiteralPath $fixtureManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($fixtureManifestHashAfter -ne $fixtureManifestHash) {
    throw "The accepted workload manifest changed during the measurement."
}
$repositoryAtCompletion = Get-RepositoryState $repositoryRoot
if (-not $ReportOnly -and
    ($repositoryAtCompletion.head_commit -ne $repositoryAtSelection.head_commit -or
     ($repositoryAtCompletion.status -join "`n") -ne ($repositoryAtSelection.status -join "`n"))) {
    throw "The candidate repository changed during the formal gate."
}
$activePowerSchemeAtCompletion = if ($isWindowsHost) {
    Get-ActivePowerScheme
} else {
    $null
}
$powerSchemeRecorded = -not [string]::IsNullOrWhiteSpace([string]$activePowerScheme) -and
    -not [string]::IsNullOrWhiteSpace([string]$activePowerSchemeAtCompletion)
$powerSchemeStable = $powerSchemeRecorded -and
    $activePowerScheme -eq $activePowerSchemeAtCompletion

if ($BackendBakeoff) {
    $evidencePolicy = Get-BackendEvidencePolicy ([bool]$Screening)
    $aggregateVerdicts = [ordered]@{}
    foreach ($component in @(
        "product", "equal_work", "calibration", "backend_health", "compatibility"
    )) {
        $failedWorkloads = @($workloads | Where-Object {
            $_.verdicts.$component -ne "pass"
        })
        $aggregateVerdicts[$component] = if ($failedWorkloads.Count -eq 0) {
            "pass"
        } else {
            "fail"
        }
    }
    $finalEligible = $evidencePolicy.final_eligible -and
        $candidateArtifact.verified -and
        $candidateArtifact.built_this_invocation -and
        -not $repositoryAtSelection.dirty -and
        $Runs -eq 6 -and
        $ProcessorIndex -eq 8 -and
        $verifiedChildAffinityMasks.Count -eq $policies.Count * (2 + 2 * $Runs) -and
        $powerSchemeStable
    $survivalFailures = @($workloads | Where-Object { $_.survival.verdict -ne "pass" })
    $survivalComponentsPass = @(
        "equal_work", "calibration", "backend_health", "compatibility"
    ) | Where-Object { $aggregateVerdicts[$_] -ne "pass" }
    $trackASurvival = if ($Screening) {
        "not_evaluated"
    } elseif (-not $finalEligible) {
        "ineligible"
    } elseif ($survivalComponentsPass.Count -eq 0 -and $survivalFailures.Count -eq 0) {
        "pass"
    } else {
        "fail"
    }
    $verdict = if ($Screening) {
        "screening"
    } elseif ($trackASurvival -eq "pass") {
        "survived"
    } elseif ($trackASurvival -eq "ineligible") {
        "ineligible"
    } else {
        "failed"
    }
    $backendFailures = @()
    if (-not $powerSchemeStable) {
        $backendFailures += if ($powerSchemeRecorded) {
            "the active power scheme changed during the bakeoff"
        } else {
            "the active power scheme could not be recorded"
        }
    }
    foreach ($workloadResult in $workloads) {
        if ($workloadResult.survival.verdict -ne "pass") {
            $backendFailures += "$($workloadResult.name): survival threshold failed"
        }
        foreach ($component in @(
            "product", "equal_work", "calibration", "backend_health", "compatibility"
        )) {
            if ($workloadResult.verdicts.$component -ne "pass") {
                $backendFailures += "$($workloadResult.name): $component failed"
            }
        }
    }
    $summary = [ordered]@{
        schema = "izarravm-cpu-bakeoff-v1"
        comparison_class = "same_executable_backend"
        evidence_grade = $evidencePolicy.evidence_grade
        final_eligible = $finalEligible
        verdict = $verdict
        track_a_survival = $trackASurvival
        verdicts = $aggregateVerdicts
        fresh_build = $candidateArtifact.built_this_invocation
        revision = $candidateArtifact.artifact_source.head_commit
        repository_at_selection = $repositoryAtSelection
        repository_at_completion = $repositoryAtCompletion
        executable = $candidateArtifact
        roles = [ordered]@{
            automatic = [ordered]@{
                executable_sha256 = $candidateArtifact.sha256
                cli = "default automatic backend"
                environment = [ordered]@{ IZARRAVM_JIT = "1" }
            }
            interpreter = [ordered]@{
                executable_sha256 = $candidateArtifact.sha256
                cli = "--interpreter"
                environment = [ordered]@{ IZARRAVM_JIT = "0" }
            }
            same_frozen_executable = $true
        }
        verification = [ordered]@{
            executable_status = "built_and_verified"
            workload_manifest_matches = $fixtureManifestMatches
            build_environment_override_names = @($detectedBuildEnvironmentOverrides.Keys | Sort-Object)
        }
        measurement_lock = $measurementLockEvidence
        gate_script_sha256 = [ordered]@{
            at_entry = $gateScriptHash
            at_completion = $gateScriptHashAfter
        }
        workload_manifest_sha256 = [ordered]@{
            at_entry = $fixtureManifestHash
            at_completion = $fixtureManifestHashAfter
        }
        injected_exitvm_sha256 = $exitVmHash
        workload_inputs_sha256 = $workloadInputHashes
        workload_trees_sha256 = $workloadTreeHashes
        workload_canonical_trees_sha256 = $workloadCanonicalTreeHashes
        generated_utc = [DateTime]::UtcNow.ToString("o")
        host = [ordered]@{
            identity = $hostIdentity
            active_power_scheme_at_completion = $activePowerSchemeAtCompletion
            active_power_scheme_recorded = $powerSchemeRecorded
            active_power_scheme_stable = $powerSchemeStable
        }
        processor_affinity = [ordered]@{
            policy = "one inherited processor per child"
            requested_processor_index = $ProcessorIndex
            effective_processor_index = $ProcessorIndex
            requested_mask = Format-AffinityMask $requestedProcessorMask
            original_gate_process_mask = Format-AffinityMask $originalGateAffinity
            verified_child_masks = @($verifiedChildAffinityMasks | Sort-Object -Unique)
            verified_child_processes = $verifiedChildAffinityMasks.Count
            child_inheritance_verified = $true
            parent_restore_policy = "restore the exact pre-launch mask immediately after each child readback"
            processor_group_policy = "single ProcessorAffinity group, at most 64 logical processors"
        }
        measured_pairs_per_workload = $Runs
        measured_runs_per_role_and_workload = $Runs
        discarded_warmups_per_role_and_workload = 1
        pair_seed = $PairSeed
        pair_order = @(1..$Runs | ForEach-Object {
            [ordered]@{
                pair = $_
                roles = @(Get-PairOrder $_ $PairSeed $pairRoles)
            }
        })
        termination_policies = [ordered]@{
            doom = "Lotura TestExit code 0 after the 2134-gametic timedemo"
            quake = "fixed 6.2G cycle limit with exactly one 969-frame identity; wall includes the minimal post-demo tail"
            quake_test_exit_gate = "blocked: +exec bench.cfg corrupts playback before the identity"
        }
        acceptance = [ordered]@{
            workload_real_time_factor_floors = [ordered]@{
                doom_486 = 3.5
                doom_586 = 1.4
                quake_586 = 1.4
            }
            minimum_backend_median_ratio = 1.05
            minimum_backend_lower_95_ratio_exclusive = 1.0
            paired_lower_bound = "one-sided 95% Student-t"
            exact_work_fields = @(
                "perf.instructions", "master_ticks", "elapsed_budget_clocks",
                "executed_cpu_core_clocks", "raw_bus_clocks", "stop",
                "timedemo_identity", "result_block_sha256"
            )
        }
        scope = "Headless same-executable CPU backend comparison. GUI pacing and audio require separate validation."
        workloads = $workloads
        failure_reasons = $backendFailures
    }
} else {
$formalGateEligible = -not $ReportOnly -and $candidateArtifact.verified -and
    $pairedRun -and $baselineArtifact.verified -and -not $repositoryAtSelection.dirty -and
    $ProcessorIndex -ge 0
$formalFailures = @()
foreach ($workloadResult in $workloads) {
    $reasons = @()
    $candidateMedian = $workloadResult.candidate.median
    if (-not $ReportOnly) {
        $candidateChecks = $workloadResult.candidate_sample_checks
        if ($candidateChecks.coverage_passes -ne $candidateChecks.samples) {
            $reasons += "one or more candidate samples have direct-native coverage below 90%"
        }
        if ($candidateChecks.exit_rate_passes -ne $candidateChecks.samples) {
            $reasons += "one or more candidate samples have at least 5 direct slow exits per 100 instructions"
        }
        if ($pairedRun) {
            if ($candidateChecks.real_time_floor_passes -lt $minimumFloorPasses) {
                $reasons += "fewer than four candidate samples meet the workload real-time floor"
            }
            foreach ($metricName in @("instructions_per_host_second", "real_time_factor")) {
                if ($workloadResult.paired_metrics[$metricName].verdict -ne "pass") {
                    $reasons += "$metricName paired result is $($workloadResult.paired_metrics[$metricName].verdict)"
                }
            }
            $baselineMedian = $workloadResult.baseline.median
            if ($candidateMedian.direct_native_coverage -lt $baselineMedian.direct_native_coverage - 0.005) {
                $reasons += "candidate direct-native coverage regressed by more than 0.5 points"
            }
            $exitAllowance = [Math]::Max(0.02, 0.05 * $baselineMedian.direct_slow_exits_per_100_instructions)
            if ($candidateMedian.direct_slow_exits_per_100_instructions -gt
                $baselineMedian.direct_slow_exits_per_100_instructions + $exitAllowance) {
                $reasons += "candidate direct slow exits regressed beyond the paired allowance"
            }
        }
    }
    $workloadResult["failure_reasons"] = $reasons
    if ($reasons.Count -gt 0) {
        $formalFailures += "$($workloadResult.name): $($reasons -join '; ')"
    }
}
$verdict = if ($ReportOnly) {
    "diagnostic"
} elseif (-not $formalGateEligible) {
    "ineligible"
} elseif ($formalFailures.Count -gt 0) {
    "failed"
} else {
    "passed"
}

$summary = [ordered]@{
    schema = "izarravm-throughput-gate-v2"
    formal = -not $ReportOnly
    verdict = $verdict
    formal_gate_eligible = $formalGateEligible
    jit = $Jit
    fresh_build = $candidateArtifact.built_this_invocation
    revision = if ($candidateArtifact.verified) { $candidateArtifact.artifact_source.head_commit } else { $null }
    repository_at_selection = $repositoryAtSelection
    repository_at_completion = $repositoryAtCompletion
    executable = $candidateArtifact
    baseline_executable = $baselineArtifact
    verification = [ordered]@{
        candidate_status = if ($candidateArtifact.verified) { "built_and_verified" } else { "unverified" }
        baseline_status = if ($pairedRun -and $baselineArtifact.verified) { "built_and_verified" } else { "absent_or_unverified" }
        override_used = $artifactSelection -eq "unverified_prebuilt"
        workload_manifest_matches = $fixtureManifestMatches
        build_environment_override_names = @($detectedBuildEnvironmentOverrides.Keys | Sort-Object)
    }
    gate_script_sha256 = [ordered]@{
        at_entry = $gateScriptHash
        at_completion = $gateScriptHashAfter
    }
    workload_manifest_sha256 = [ordered]@{
        at_entry = $fixtureManifestHash
        at_completion = $fixtureManifestHashAfter
    }
    injected_exitvm_sha256 = $exitVmHash
    workload_inputs_sha256 = $workloadInputHashes
    workload_trees_sha256 = $workloadTreeHashes
    workload_canonical_trees_sha256 = $workloadCanonicalTreeHashes
    generated_utc = [DateTime]::UtcNow.ToString("o")
    host = $hostIdentity
    processor_affinity = [ordered]@{
        policy = if ($ProcessorIndex -ge 0) { "one inherited processor per child" } else { "unpinned" }
        requested_processor_index = if ($ProcessorIndex -ge 0) { $ProcessorIndex } else { $null }
        effective_processor_index = if ($verifiedChildAffinityMasks.Count -gt 0) { $ProcessorIndex } else { $null }
        requested_mask = if ($null -ne $requestedProcessorMask) {
            Format-AffinityMask $requestedProcessorMask
        } else {
            $null
        }
        original_gate_process_mask = if ($null -ne $originalGateAffinity) {
            Format-AffinityMask $originalGateAffinity
        } else {
            $null
        }
        verified_child_masks = @($verifiedChildAffinityMasks | Sort-Object -Unique)
        verified_child_processes = $verifiedChildAffinityMasks.Count
        child_inheritance_verified = $ProcessorIndex -ge 0 -and
            $verifiedChildAffinityMasks.Count -gt 0
        parent_restore_policy = "restore the exact pre-launch mask immediately after each child readback"
        processor_group_policy = "single ProcessorAffinity group, at most 64 logical processors"
    }
    measured_pairs_per_workload = if ($pairedRun) { $Runs } else { 0 }
    measured_candidate_runs_per_workload = $Runs
    discarded_warmups_per_executable_and_workload = if ($pairedRun) { 1 } else { 0 }
    pair_seed = if ($pairedRun) { $PairSeed } else { $null }
    pair_order = if ($pairedRun) {
        @(1..$Runs | ForEach-Object {
            [ordered]@{
                pair = $_
                roles = @(Get-PairOrder $_ $PairSeed)
            }
        })
    } else { @() }
    scope = "Headless Doom and Quake throughput only. GUI pacing and audio require separate validation."
    acceptance = [ordered]@{
        accepted_baseline_tree = $acceptedBaselineTree
        workload_real_time_factor_floors = [ordered]@{
            doom_486 = 3.5
            doom_586 = 1.4
            quake_586 = 1.4
        }
        minimum_direct_native_coverage = $minimumDirectCoverage
        maximum_direct_slow_exits_per_100_instructions = $maximumDirectExitsPer100
        minimum_paired_median_ratio = 0.98
        minimum_paired_lower_95_ratio = 0.97
        paired_lower_bound = "one-sided 95% Student-t"
        required_processor_index = if (-not $ReportOnly) { $ProcessorIndex } else { $null }
        minimum_samples_meeting_real_time_floor = $minimumFloorPasses
    }
    workloads = $workloads
    failure_reasons = $formalFailures
}
}

$summaryPath = Join-Path $ResultsDirectory "summary.json"
$summary | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $summaryPath -Encoding utf8

foreach ($workloadResult in $summary.workloads) {
    if ($BackendBakeoff) {
        $median = $workloadResult.automatic.median
        Write-Host ("{0}: automatic rt={1:N3} direct-native={2:P2} verdicts={3}" -f `
            $workloadResult.name, $median.real_time_factor, $median.direct_native_coverage, `
            (($workloadResult.verdicts.GetEnumerator() | ForEach-Object {
                "$($_.Key)=$($_.Value)"
            }) -join ","))
    } else {
        $median = $workloadResult.candidate.median
        Write-Host ("{0}: candidate rt={1:N3} direct-native={2:P2} direct-exits/100={3:N3}" -f `
            $workloadResult.name, $median.real_time_factor, $median.direct_native_coverage, `
            $median.direct_slow_exits_per_100_instructions)
    }
    if ($null -ne $workloadResult.paired_metrics) {
        Write-Host ("  paired IPS={0:N3} (lower95={1:N3}) RTF={2:N3} (lower95={3:N3})" -f `
            $workloadResult.paired_metrics.instructions_per_host_second.median_ratio,
            $workloadResult.paired_metrics.instructions_per_host_second.lower_95_ratio,
            $workloadResult.paired_metrics.real_time_factor.median_ratio,
            $workloadResult.paired_metrics.real_time_factor.lower_95_ratio)
    }
}
Write-Host "Summary: $summaryPath"

if ($BackendBakeoff -and -not $Screening -and $summary.track_a_survival -ne "pass") {
    throw "The backend bakeoff did not meet its survival gate: $($summary.failure_reasons -join ' | ')."
}
if (-not $BackendBakeoff -and -not $ReportOnly -and $verdict -ne "passed") {
    throw "The paired throughput gate did not pass: $($formalFailures -join ' | ')."
}
} finally {
    if ($null -ne $measurementLockHandle) {
        $measurementLockHandle.Dispose()
    }
}
