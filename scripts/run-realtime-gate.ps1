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
    [ValidateSet("Both", "Doom", "Doom586", "Quake")]
    [string]$Workload = "Both",
    [ValidateSet("0", "1")]
    [string]$Jit = "1",
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

function Get-PairOrder([int]$PairNumber, [int]$Seed) {
    $candidateFirstOnOddPairs = ($Seed -band 1) -eq 0
    $candidateFirst = if ($PairNumber % 2 -eq 1) {
        $candidateFirstOnOddPairs
    } else {
        -not $candidateFirstOnOddPairs
    }
    if ($candidateFirst) {
        return @("candidate", "baseline")
    }
    return @("baseline", "candidate")
}

function Get-Median([double[]]$Values) {
    $ordered = @($Values | Sort-Object)
    $middle = [Math]::Floor($ordered.Count / 2)
    if ($ordered.Count % 2 -eq 1) {
        return $ordered[$middle]
    }
    return ($ordered[$middle - 1] + $ordered[$middle]) / 2.0
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
    $critical = if ($logs.Count -eq 6) { 2.015048 } else { 1.96 }
    $lower95 = [Math]::Exp($mean - $critical * $sampleDeviation / [Math]::Sqrt($logs.Count))
    $median = Get-Median $Ratios
    $verdict = Get-PairedMetricVerdict $median $lower95
    return [pscustomobject][ordered]@{
        median_ratio = $median
        lower_95_ratio = $lower95
        verdict = $verdict
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
    if ((Get-PairedMetric ([double[]](1, 1, 1, 1, 1, 1))).verdict -ne "pass" -or
        (Get-PairedMetric ([double[]](0.97, 0.97, 0.97, 0.97, 0.97, 0.97))).verdict -ne "regression" -or
        (Get-PairedMetric ([double[]](0.90, 0.91, 1.00, 1.01, 1.10, 1.11))).verdict -ne "inconclusive") {
        throw "Paired metric verdict boundaries are wrong."
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
    Write-Host "run-realtime-gate self-test passed"
    return
}

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
$explicitExecutable = $PSBoundParameters.ContainsKey("Executable")
$artifactSelection = Get-ArtifactSelectionPolicy ([bool]$ReportOnly) $explicitExecutable ([bool]$SkipBuild)
if ($PairSeed -eq 0) {
    $PairSeed = [Security.Cryptography.RandomNumberGenerator]::GetInt32(1, [int]::MaxValue)
}
if ($HostTimeoutSeconds -lt 1) {
    throw "HostTimeoutSeconds must be positive."
}

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

function Get-BytesSha256([byte[]]$Bytes) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
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
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-build-" + [guid]::NewGuid())
    $source = Join-Path $scratch "source"
    $target = Join-Path $scratch "target"
    $isolatedCargoHome = Join-Path $scratch "cargo-home"
    $archive = Join-Path $scratch "source.tar"
    New-Item -ItemType Directory -Path $source | Out-Null
    New-Item -ItemType Directory -Path $isolatedCargoHome | Out-Null
    $started = [DateTime]::UtcNow
    try {
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
                [Environment]::SetEnvironmentVariable($name, $null, "Process")
            }
            [Environment]::SetEnvironmentVariable("CARGO_HOME", $isolatedCargoHome, "Process")
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
                [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
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
$pairedRun = $null -ne $baselineArtifact
if (-not $ReportOnly -and -not $pairedRun) {
    throw "The formal gate requires a freshly built baseline artifact."
}
if ($pairedRun -and $candidateArtifact.verified -and $baselineArtifact.verified -and
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
    [string]$HomePath
) {
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
    $childEnvironment["IZARRAVM_JIT"] = $Jit
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
    $process = Start-Process @start
    # Keep the native handle alive so ExitCode remains available after a fast child exit.
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
    foreach ($path in @($StdoutPath, $StderrPath)) {
        if ((Test-Path -LiteralPath $path -PathType Leaf) -and
            (Get-Item -LiteralPath $path).Length -gt 64MB) {
            throw "IzarraVM produced more than 64 MiB of diagnostic output."
        }
    }
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
    $exitCode = Invoke-IzarraProcess $ExecutablePath $processArguments $stdoutPath $stderrPath $observationHome
    if ($exitCode -ne 0) {
        throw "$context failed with exit code $exitCode. See $stdoutPath and $stderrPath."
    }
    if (-not (Test-Path -LiteralPath $jsonPath -PathType Leaf)) {
        throw "$context did not produce its profile JSON."
    }
    $sample = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json
    if ($sample.schema -ne "izarravm-hdd-profile-v1" -or $sample.mode -ne $Policy.mode) {
        throw "$context produced an unexpected schema or CPU mode."
    }
    Assert-UninstrumentedProfileSample $sample $context
    if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if ($sample.stop.kind -ne "test_exit" -or $sample.stop.code -ne 0) {
            throw "$context did not reach TestExit code 0."
        }
    } elseif ($sample.stop.kind -ne "cycle_limit" -or
        [uint64]$sample.stop.requested -ne $Policy.cycle_budget) {
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
    if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if ($null -eq $sample.timedemo -or $sample.timedemo.gametics -ne 2134 -or
            $sample.timedemo.realtics -lt $Policy.minimum_realtics -or
            $sample.timedemo.realtics -gt $Policy.maximum_realtics) {
            throw "$context failed its 2134-gametic timing identity check."
        }
        $doomFps = 35.0 * $sample.timedemo.gametics / $sample.timedemo.realtics
        $sample | Add-Member -NotePropertyName doom_fps -NotePropertyValue $doomFps
    } else {
        $preservedQconsole = Join-Path $ResultsDirectory "$fileStem-qconsole.log"
        Remove-Item -LiteralPath $preservedQconsole -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
            Copy-Item -LiteralPath $qconsole -Destination $preservedQconsole
        }
        $quakeIdentity = Read-QuakeTimedemoIdentity $preservedQconsole
        $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
    }
    $sample | Add-Member -NotePropertyName gate_role -NotePropertyValue $Role
    $sample | Add-Member -NotePropertyName gate_observation -NotePropertyValue $ObservationId
    return $sample
}

function Get-RoleSummary([string]$Name, [string]$Mode, [object[]]$Samples) {
    Assert-RoleDeterminism $Name $Samples
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
try {
    $savedEnvironment["IZARRAVM_JIT"] = [Environment]::GetEnvironmentVariable("IZARRAVM_JIT", "Process")
    foreach ($name in $diagnosticVariables) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
    [Environment]::SetEnvironmentVariable("IZARRAVM_JIT", $Jit, "Process")
    $candidateExecutableLock = [IO.File]::Open(
        $candidateArtifact.executed_copy_path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    if ($pairedRun) {
        $baselineExecutableLock = [IO.File]::Open(
            $baselineArtifact.executed_copy_path,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
    }
    $policies = Get-WorkloadPolicies $Workload
    $observations = [ordered]@{}
    foreach ($policy in $policies) {
        $observations[$policy.name] = [ordered]@{ candidate = @(); baseline = @() }
    }

    if ($pairedRun) {
        foreach ($policy in $policies) {
            $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                $doomSnapshot.frozen_path
            } else {
                $quakeSnapshot.frozen_path
            }
            foreach ($role in (Get-PairOrder 1 $PairSeed)) {
                $artifact = if ($role -eq "candidate") { $candidateArtifact } else { $baselineArtifact }
                $null = Invoke-Observation $policy $sourceFolder $role "warmup" $artifact.executed_copy_path
            }
        }
        for ($pair = 1; $pair -le $Runs; $pair++) {
            $roleOrder = Get-PairOrder $pair $PairSeed
            for ($workloadOffset = 0; $workloadOffset -lt $policies.Count; $workloadOffset++) {
                $policy = $policies[($workloadOffset + $pair - 1) % $policies.Count]
                $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                    $doomSnapshot.frozen_path
                } else {
                    $quakeSnapshot.frozen_path
                }
                foreach ($role in $roleOrder) {
                    $artifact = if ($role -eq "candidate") { $candidateArtifact } else { $baselineArtifact }
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
        if ($pairedRun) {
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
    if ($pairedRun) {
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
} finally {
    if ($null -ne $baselineExecutableLock) {
        $baselineExecutableLock.Dispose()
    }
    if ($null -ne $candidateExecutableLock) {
        $candidateExecutableLock.Dispose()
    }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
    Remove-GateTemporaryRoot $temporaryRoot
}

if ($candidateHashAfter -ne $candidateArtifact.sha256) {
    throw "The frozen candidate executable changed during the gate."
}
if ($pairedRun) {
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

$formalGateEligible = -not $ReportOnly -and $candidateArtifact.verified -and
    $pairedRun -and $baselineArtifact.verified -and -not $repositoryAtSelection.dirty
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
        minimum_samples_meeting_real_time_floor = $minimumFloorPasses
    }
    workloads = $workloads
    failure_reasons = $formalFailures
}

$summaryPath = Join-Path $ResultsDirectory "summary.json"
$summary | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $summaryPath -Encoding utf8

foreach ($workloadResult in $summary.workloads) {
    $median = $workloadResult.candidate.median
    Write-Host ("{0}: candidate rt={1:N3} direct-native={2:P2} direct-exits/100={3:N3}" -f `
        $workloadResult.name, $median.real_time_factor, $median.direct_native_coverage, `
        $median.direct_slow_exits_per_100_instructions)
    if ($null -ne $workloadResult.paired_metrics) {
        Write-Host ("  paired IPS={0:N3} (lower95={1:N3}) RTF={2:N3} (lower95={3:N3})" -f `
            $workloadResult.paired_metrics.instructions_per_host_second.median_ratio,
            $workloadResult.paired_metrics.instructions_per_host_second.lower_95_ratio,
            $workloadResult.paired_metrics.real_time_factor.median_ratio,
            $workloadResult.paired_metrics.real_time_factor.lower_95_ratio)
    }
}
Write-Host "Summary: $summaryPath"

if (-not $ReportOnly -and $verdict -ne "passed") {
    throw "The paired throughput gate did not pass: $($formalFailures -join ' | ')."
}
