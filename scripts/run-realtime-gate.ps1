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
    [switch]$TrackMComparison,
    [switch]$PollSkipComparison,
    [string]$ExecutionRole = "",
    [switch]$Screening,
    [string]$MeasurementLockPath = "",
    [switch]$SkipBuild,
    [switch]$ReportOnly,
    [switch]$SelfTest,
    [switch]$DirectQuakeCampaign,
    [ValidateSet("Noise", "Screen", "Proof")]
    [string]$CampaignStage = "Proof"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# The one revision every formal candidate is measured against. Re-pinned from
# 8e238b06 (5817fb58, 2026-08-04) to cbd650de (5e821a32, 2026-08-07): merges
# #706-#713 deliberately changed the JIT admission mix (79f00626 defaulted the
# 16-bit and 486-Word paths on), which doubles direct entries and side exits
# per instruction while RAISING coverage (doom-486 0.78 -> 0.91) and doom
# throughput (+16.8% / +7.0% paired). The old pin's paired slow-exits ratchet
# therefore failed every candidate on an intended, kept change -- the gate had
# stopped being protective, the same state that forced the 2026-08-04 re-pin.
# quake-586 carries a real, documented -3.5% paired against the OLD pin
# (dev_docs/2026-08-07-gate-red-diagnosis.md), accepted with the R15-offsets
# slice named as its recovery; the floors below ratchet from the new baseline
# so it cannot slide further unnoticed. Whoever re-pins this next must
# recalibrate the per-workload floors in Get-WorkloadPolicy in the same commit
# -- they are ratchets derived from what the pinned tree measures, and a pin
# moved without them silently stops asserting anything.
#
# The floors below were measured on 5e821a32 itself: gate run
# 5e821a32d463-20260807-021454-57b54847, six pairs, quiet box, processor 8,
# candidate role. The gate run that accepted this pin confirms them rather
# than assuming it.
$acceptedBaselineTree = "cbd650def00eeb076226162128c2cd9160b98a80"
$highPerformancePowerSchemeGuid = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"
$minimumDirectCoverage = 0.90
$maximumDirectExitsPer100 = 5.0
$minimumFloorPasses = 4
$gateMainScriptPath = [IO.Path]::GetFullPath($PSCommandPath)
$gateScriptsRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$gateSelfTestScriptPath = [IO.Path]::GetFullPath(
    (Join-Path $gateScriptsRoot "run-realtime-gate-self-test.ps1")
)
$gateSummaryScriptPath = [IO.Path]::GetFullPath(
    (Join-Path $gateScriptsRoot "run-realtime-gate-summary.ps1")
)

function Get-GateSha256Hex([byte[]]$Bytes) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).
            Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-GateSourceMemberIdentity([string]$Label, [string]$Path) {
    $orderedLabels = @(
        "scripts/run-realtime-gate.ps1",
        "scripts/run-realtime-gate-self-test.ps1",
        "scripts/run-realtime-gate-summary.ps1"
    )
    if ($orderedLabels -cnotcontains $Label -or
        $Label.Contains([char]0) -or $Label.Contains("`n") -or $Label.Contains("`r")) {
        throw "Invalid gate source label '$Label'."
    }
    if ([string]::IsNullOrWhiteSpace($Path) -or
        -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing gate source member '$Label': $Path"
    }
    $bytes = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Path))
    return [pscustomobject][ordered]@{
        label = $Label
        byte_length = $bytes.LongLength
        sha256 = Get-GateSha256Hex $bytes
    }
}

function Get-GateSourceClosureIdentity(
    [string]$MainPath,
    [string]$SelfTestPath,
    [string]$SummaryPath
) {
    $members = [object[]]@(
        Get-GateSourceMemberIdentity "scripts/run-realtime-gate.ps1" $MainPath
        Get-GateSourceMemberIdentity `
            "scripts/run-realtime-gate-self-test.ps1" $SelfTestPath
        Get-GateSourceMemberIdentity `
            "scripts/run-realtime-gate-summary.ps1" $SummaryPath
    )
    $manifest = [Text.StringBuilder]::new("izarravm-gate-source-closure-v1`n")
    foreach ($member in $members) {
        [void]$manifest.Append($member.label)
        [void]$manifest.Append([char]0)
        [void]$manifest.Append(
            $member.byte_length.ToString([Globalization.CultureInfo]::InvariantCulture)
        )
        [void]$manifest.Append([char]0)
        [void]$manifest.Append($member.sha256)
        [void]$manifest.Append("`n")
    }
    $manifestBytes = [Text.UTF8Encoding]::new($false).GetBytes($manifest.ToString())
    return [pscustomobject][ordered]@{
        schema = "izarravm-gate-source-closure-v1"
        closure_sha256 = Get-GateSha256Hex $manifestBytes
        members = $members
    }
}

function Get-GateSourceClosureMismatches($Expected, $Actual) {
    $mismatches = @()
    $expectedMembers = @($Expected.members)
    $actualMembers = @($Actual.members)
    $memberCount = [Math]::Min($expectedMembers.Count, $actualMembers.Count)
    for ($index = 0; $index -lt $memberCount; $index++) {
        $expectedMember = $expectedMembers[$index]
        $actualMember = $actualMembers[$index]
        if ($expectedMember.label -cne $actualMember.label) {
            $mismatches += $expectedMember.label
            $mismatches += $actualMember.label
        } elseif ($expectedMember.byte_length -ne $actualMember.byte_length -or
            $expectedMember.sha256 -cne $actualMember.sha256) {
            $mismatches += $expectedMember.label
        }
    }
    if ($expectedMembers.Count -ne $actualMembers.Count) {
        $mismatches += @($expectedMembers[$memberCount..($expectedMembers.Count - 1)].label)
        $mismatches += @($actualMembers[$memberCount..($actualMembers.Count - 1)].label)
    }
    if ($Expected.closure_sha256 -cne $Actual.closure_sha256 -and
        $mismatches.Count -eq 0) {
        $mismatches += "source-closure manifest"
    }
    return @($mismatches | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        Sort-Object -Unique)
}

function Assert-GateSourceClosureUnchanged($Expected, $Actual, [string]$Context) {
    $mismatches = @(Get-GateSourceClosureMismatches $Expected $Actual)
    if ($mismatches.Count -ne 0) {
        throw "The gate source closure changed $Context`: $($mismatches -join ', ')."
    }
}

function ConvertTo-GateSourceClosureEvidence($Identity) {
    return [ordered]@{
        closure_sha256 = $Identity.closure_sha256
        members = [object[]]@($Identity.members | ForEach-Object {
            [ordered]@{
                label = $_.label
                byte_length = $_.byte_length
                sha256 = $_.sha256
            }
        })
    }
}

$gateSourceClosureAtEntry = Get-GateSourceClosureIdentity `
    $gateMainScriptPath $gateSelfTestScriptPath $gateSummaryScriptPath
$gateScriptHash = $gateSourceClosureAtEntry.members[0].sha256

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
        $acquiredUtc = [DateTime]::UtcNow.ToString("o")
        $metadata = [ordered]@{
            pid = $PID
            acquired_utc = $acquiredUtc
        } | ConvertTo-Json -Compress
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($metadata + "`n")
        $stream.SetLength(0)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
        return [pscustomobject][ordered]@{
            handle = $stream
            path = $absolutePath
            pid = $PID
            acquired_utc = $acquiredUtc
        }
    } catch {
        $stream.Dispose()
        throw
    }
}

function Get-MeasurementLockEvidence($Lease) {
    return [ordered]@{
        path = $Lease.path
        acquired_utc = $Lease.acquired_utc
        pid = $Lease.pid
        share_mode = "FileShare.None"
        scope = "cooperating campaign tools"
        held_through_summary_write = $true
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

$backendQuakeWaitMarker = "IZARRA-QEMU-QUAKE-WAIT-DONE-20260713C001"
$backendQuakeAutoexecSha256 = "c72b5c0e66ffd1743c857e430ade756b63bbf16e08d79f9c58d53108ba0f85fc"
$backendQuakeBenchCfgSha256 = "c20a7cef0f12c7a4422781fe2cff06dbfb9e11de4fe7483333a36b3c95c7537a"

function Test-BackendQuakeCompletionOverride([bool]$IsBackendBakeoff, [string]$PolicyName) {
    return $IsBackendBakeoff -and $PolicyName -ceq "quake-586"
}

function Test-ObservationRequiresTestExit([bool]$IsBackendBakeoff, [string]$PolicyName) {
    return $PolicyName.StartsWith("doom-", [StringComparison]::Ordinal) -or
        (Test-BackendQuakeCompletionOverride $IsBackendBakeoff $PolicyName)
}

function Get-BackendQuakeCompletionOverrides {
    $autoexecText = (@(
        "@echo off",
        "cd \QUAKE",
        "quake.exe -nosound -nocdaudio -nojoy -condebug +timedemo demo1 +startdemos +exec bench.cfg",
        "C:\EXITVM.COM"
    ) -join "`n") + "`n"
    $benchCfgText = (@(
        'alias w10 "wait;wait;wait;wait;wait;wait;wait;wait;wait;wait"',
        'alias w100 "w10;w10;w10;w10;w10;w10;w10;w10;w10;w10"',
        'alias w1000 "w100;w100;w100;w100;w100;w100;w100;w100;w100;w100"',
        "",
        "w1000",
        "echo $backendQuakeWaitMarker",
        "toggleconsole",
        "quit"
    ) -join "`n") + "`n"
    $ascii = [Text.Encoding]::ASCII
    $autoexecBytes = $ascii.GetBytes($autoexecText)
    $benchCfgBytes = $ascii.GetBytes($benchCfgText)
    $autoexecHash = Get-BytesSha256 $autoexecBytes
    $benchCfgHash = Get-BytesSha256 $benchCfgBytes
    if ($autoexecHash -cne $backendQuakeAutoexecSha256 -or
        $benchCfgHash -cne $backendQuakeBenchCfgSha256) {
        throw "The BackendBakeoff Quake completion override bytes changed."
    }
    return [pscustomobject][ordered]@{
        autoexec_bytes = $autoexecBytes
        autoexec_sha256 = $autoexecHash
        bench_cfg_bytes = $benchCfgBytes
        bench_cfg_sha256 = $benchCfgHash
        wait_marker = $backendQuakeWaitMarker
    }
}

function Find-BackendQuakeFatalText([string[]]$Paths) {
    $fatalMatches = [Collections.Generic.List[string]]::new()
    foreach ($path in $Paths) {
        if ([string]::IsNullOrWhiteSpace($path) -or
            -not (Test-Path -LiteralPath $path -PathType Leaf)) {
            continue
        }
        $lineNumber = 0
        foreach ($line in [IO.File]::ReadAllLines($path)) {
            $lineNumber++
            if ($line -match '(?i)No demos listed with startdemos|Host_Error|Sys_Error|Demo message\s*>\s*MAX_MSGLEN|\bfatal\b') {
                $fatalMatches.Add("$([IO.Path]::GetFileName($path)):${lineNumber}:$($line.Trim())")
            }
        }
    }
    return [object[]]$fatalMatches
}

function Read-BackendQuakeCompletion(
    [string]$QconsolePath,
    [string[]]$AdditionalDiagnosticPaths = @()
) {
    $identities = [Collections.Generic.List[object]]::new()
    $identityLines = [Collections.Generic.List[int]]::new()
    $markerLines = [Collections.Generic.List[int]]::new()
    if (Test-Path -LiteralPath $QconsolePath -PathType Leaf) {
        $lineNumber = 0
        foreach ($line in [IO.File]::ReadAllLines($QconsolePath)) {
            $lineNumber++
            $identity = ConvertFrom-QuakeTimedemoLine $line
            if ($null -ne $identity) {
                $identities.Add($identity)
                $identityLines.Add($lineNumber)
            }
            if ($line.Trim() -ceq $backendQuakeWaitMarker) {
                $markerLines.Add($lineNumber)
            }
        }
    }
    $allDiagnosticPaths = @($QconsolePath) + @($AdditionalDiagnosticPaths)
    $fatalMatches = @(Find-BackendQuakeFatalText $allDiagnosticPaths)
    $identity = if ($identities.Count -eq 1) { $identities[0] } else { $null }
    $identityLine = if ($identityLines.Count -eq 1) { $identityLines[0] } else { $null }
    $markerLine = if ($markerLines.Count -eq 1) { $markerLines[0] } else { $null }
    $resultBeforeMarker = $null -ne $identityLine -and $null -ne $markerLine -and
        $identityLine -lt $markerLine
    $reportedValuesConsistent = $null -ne $identity -and $identity.seconds -gt 0 -and
        [Math]::Abs($identity.frames / $identity.seconds - $identity.fps) -le 0.2
    return [pscustomobject][ordered]@{
        identity_count = $identities.Count
        timedemo = $identity
        timedemo_line_number = $identityLine
        wait_marker = $backendQuakeWaitMarker
        wait_marker_count = $markerLines.Count
        wait_marker_line_number = $markerLine
        result_before_wait_marker = $resultBeforeMarker
        reported_values_consistent = $reportedValuesConsistent
        fatal_match_count = $fatalMatches.Count
        fatal_matches = [object[]]$fatalMatches
    }
}

function Get-BackendQuakeCompletionReasons($Completion, [string]$Label) {
    $reasons = @()
    if ($null -eq $Completion) {
        return @("$Label is missing its Quake completion evidence")
    }
    if ($Completion.identity_count -ne 1 -or $null -eq $Completion.timedemo -or
        $Completion.timedemo.frames -ne 969 -or $Completion.timedemo.seconds -le 0 -or
        $Completion.timedemo.fps -le 0) {
        $reasons += "$Label did not produce exactly one valid 969-frame Quake identity"
    } elseif (-not $Completion.reported_values_consistent) {
        $reasons += "$Label reported inconsistent Quake seconds and fps"
    }
    if ($Completion.wait_marker_count -ne 1) {
        $reasons += "$Label did not produce exactly one post-timedemo wait marker"
    } elseif (-not $Completion.result_before_wait_marker) {
        $reasons += "$Label did not report the timedemo before the post-demo wait completed"
    }
    if ($Completion.fatal_match_count -ne 0) {
        $reasons += "$Label contains fatal Quake text: $($Completion.fatal_matches -join '; ')"
    }
    return @($reasons)
}

function Get-BackendQuakeFixtureReasons($Fixture, [string]$Label) {
    $reasons = @()
    if ($null -eq $Fixture) {
        return @("$Label is missing its disposable Quake fixture evidence")
    }
    foreach ($property in @(
        "canonical_tree_sha256", "autoexec_before_sha256", "bench_cfg_before_sha256",
        "autoexec_override_sha256", "bench_cfg_override_sha256", "exitvm_sha256",
        "prelaunch_overridden_tree_sha256"
    )) {
        if ($null -eq $Fixture.PSObject.Properties[$property] -or
            [string]$Fixture.$property -notmatch '^[0-9a-f]{64}$') {
            $reasons += "$Label fixture evidence has an invalid $property"
        }
    }
    if ($null -eq $Fixture.PSObject.Properties["autoexec_override_sha256"] -or
        $Fixture.autoexec_override_sha256 -cne $backendQuakeAutoexecSha256) {
        $reasons += "$Label used the wrong BackendBakeoff Quake AUTOEXEC override"
    }
    if ($null -eq $Fixture.PSObject.Properties["bench_cfg_override_sha256"] -or
        $Fixture.bench_cfg_override_sha256 -cne $backendQuakeBenchCfgSha256) {
        $reasons += "$Label used the wrong BackendBakeoff Quake bench.cfg override"
    }
    if ($null -eq $Fixture.PSObject.Properties["stale_qconsole_absent_before_launch"] -or
        -not $Fixture.stale_qconsole_absent_before_launch) {
        $reasons += "$Label launched with a stale QCONSOLE.LOG"
    }
    return @($reasons)
}

function Assert-BackendQuakeFixtureSet([object[]]$Samples) {
    if ($Samples.Count -eq 0) {
        throw "The BackendBakeoff Quake fixture set is empty."
    }
    $identities = @()
    foreach ($sample in $Samples) {
        $label = "$($sample.gate_role) $($sample.gate_observation)"
        $fixture = if ($null -ne $sample.PSObject.Properties["gate_fixture"]) {
            $sample.gate_fixture
        } else {
            $null
        }
        $reasons = @(Get-BackendQuakeFixtureReasons $fixture $label)
        if ($reasons.Count -ne 0) {
            throw "The BackendBakeoff Quake fixture set is invalid: $($reasons -join '; ')"
        }
        $identities += Get-MeasurementFixtureIdentityKey $sample
    }
    $uniqueIdentities = @($identities | Sort-Object -Unique)
    if ($uniqueIdentities.Count -ne 1) {
        throw "BackendBakeoff Quake did not use one identical prelaunch fixture across every observation."
    }
    return $uniqueIdentities[0]
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
                # Ratchets, set just under what the accepted baseline measures
                # (rt 2.762-2.922, coverage 91.33%, realtics 2969 all six
                # samples; the realtics band is wider because realtics is
                # session-local).
                minimum_real_time_factor = 2.65
                minimum_direct_native_coverage = 0.90
                minimum_realtics = 2900
                maximum_realtics = 3050
            }
        }
        "doom-586" {
            return [pscustomobject][ordered]@{
                name = $Name
                mode = "586"
                cycle_budget = [uint64]6640000000
                # PROVISIONAL bands for the 166 MHz / 64 MB spec change: the old
                # 200 MHz baseline measured rt 0.911-0.961, realtics 826. Wall
                # shrinks ~17% at the same guest span, so rt rises ~x1.2, and
                # realtics scale by 200/166. Re-derive from the first accepted
                # baseline on the new spec.
                minimum_real_time_factor = 0.95
                minimum_direct_native_coverage = 0.92
                minimum_realtics = 970
                maximum_realtics = 1040
            }
        }
        "quake-586" {
            return [pscustomobject][ordered]@{
                name = $Name
                mode = "586"
                cycle_budget = [uint64]5146000000
                # Baseline measures rt 1.470-1.567, coverage 96.26%. The rt
                # floor stays 1.4 (same ~4% under the measured minimum as
                # before); the paired checks against the new baseline are the
                # tight layer, this absolute is the backstop.
                minimum_real_time_factor = 1.4
                minimum_direct_native_coverage = 0.95
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

function Get-DirectQuakePairOrder(
    [int]$PairNumber,
    [string[]]$Roles = @("candidate", "parent")
) {
    if ($Roles.Count -ne 2 -or $Roles[0] -eq $Roles[1]) {
        throw "Direct Quake campaign measurements require two distinct role names."
    }
    if ($PairNumber -lt 1 -or $PairNumber -gt 12) {
        throw "Direct Quake campaign pair numbers must be from 1 through 12."
    }
    $candidateFirst = @($true, $false, $false, $true, $true, $false)[
        ($PairNumber - 1) % 6
    ]
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

function Get-HddTreeSnapshotV1([string]$Root) {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $files = @(Get-ChildItem -LiteralPath $rootPath -File -Recurse -Force | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($rootPath, $_.FullName).Replace("\", "/")
        if ($relative.Contains([char]0) -or $relative.Contains("`n") -or
            $relative.Contains("`r") -or $relative.StartsWith("../", [StringComparison]::Ordinal)) {
            throw "The final HDD tree contains an invalid relative path."
        }
        [pscustomobject][ordered]@{
            path = $relative
            byte_length = $_.Length
            sha256 = Get-FileSha256 $_.FullName
        }
    })
    [Array]::Sort($files, [Comparison[object]]{
        param($left, $right)
        [StringComparer]::Ordinal.Compare($left.path, $right.path)
    })
    $records = foreach ($file in $files) {
        "$($file.path)`0$($file.byte_length)`0$($file.sha256)`n"
    }
    return [pscustomobject][ordered]@{
        schema = "izarra-hdd-tree-snapshot-v1"
        path_order = "ordinal relative path"
        host_metadata_excluded = $true
        file_count = $files.Count
        tree_sha256 = Get-BytesSha256 ([Text.Encoding]::UTF8.GetBytes(($records -join "")))
        files = [object[]]$files
    }
}

function Set-BackendQuakeCompletionFixture(
    [string]$Fixture,
    [string]$ExpectedCanonicalTreeSha256,
    [byte[]]$ExitVmBytes,
    [string]$ExitVmSha256
) {
    $qconsolePath = Join-Path $Fixture "QUAKE/ID1/QCONSOLE.LOG"
    if (Test-Path -LiteralPath $qconsolePath -PathType Leaf) {
        Remove-Item -LiteralPath $qconsolePath
    }
    if (Test-Path -LiteralPath $qconsolePath) {
        throw "The BackendBakeoff Quake fixture contains a stale QCONSOLE.LOG."
    }
    $canonicalTreeHash = Get-DirectoryTreeSha256 $Fixture @(
        "EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG"
    )
    if ($canonicalTreeHash -cne $ExpectedCanonicalTreeSha256) {
        throw "The disposable Quake copy does not match its verified canonical tree."
    }
    $autoexecPath = Join-Path $Fixture "AUTOEXEC.BAT"
    $benchCfgPath = Join-Path $Fixture "QUAKE/ID1/bench.cfg"
    foreach ($path in @($autoexecPath, $benchCfgPath)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "The BackendBakeoff Quake override target is missing: $path"
        }
    }
    $autoexecBeforeHash = Get-FileSha256 $autoexecPath
    $benchCfgBeforeHash = Get-FileSha256 $benchCfgPath
    $overrides = Get-BackendQuakeCompletionOverrides
    [IO.File]::WriteAllBytes((Join-Path $Fixture "EXITVM.COM"), $ExitVmBytes)
    [IO.File]::WriteAllBytes($autoexecPath, $overrides.autoexec_bytes)
    [IO.File]::WriteAllBytes($benchCfgPath, $overrides.bench_cfg_bytes)
    $autoexecOverrideHash = Get-FileSha256 $autoexecPath
    $benchCfgOverrideHash = Get-FileSha256 $benchCfgPath
    $injectedExitVmHash = Get-FileSha256 (Join-Path $Fixture "EXITVM.COM")
    if ($autoexecOverrideHash -cne $overrides.autoexec_sha256 -or
        $benchCfgOverrideHash -cne $overrides.bench_cfg_sha256 -or
        $injectedExitVmHash -cne $ExitVmSha256) {
        throw "The BackendBakeoff Quake prelaunch bytes do not match their fixed identities."
    }
    if (Test-Path -LiteralPath $qconsolePath) {
        throw "The BackendBakeoff Quake fixture recreated QCONSOLE.LOG before launch."
    }
    return [pscustomobject][ordered]@{
        canonical_tree_sha256 = $canonicalTreeHash
        autoexec_before_sha256 = $autoexecBeforeHash
        bench_cfg_before_sha256 = $benchCfgBeforeHash
        autoexec_override_sha256 = $autoexecOverrideHash
        bench_cfg_override_sha256 = $benchCfgOverrideHash
        exitvm_sha256 = $injectedExitVmHash
        prelaunch_overridden_tree_sha256 = Get-DirectoryTreeSha256 `
            $Fixture @("QUAKE/ID1/QCONSOLE.LOG")
        stale_qconsole_absent_before_launch = $true
    }
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

function Get-MeasurementFixtureIdentityKey($Sample) {
    if ($null -eq $Sample.PSObject.Properties["gate_fixture"] -or
        $null -eq $Sample.gate_fixture) {
        if ($null -ne $Sample.PSObject.Properties["gate_measurement_fixture_sha256"] -and
            [string]$Sample.gate_measurement_fixture_sha256 -match '^[0-9a-f]{64}$') {
            return [string]$Sample.gate_measurement_fixture_sha256
        }
        return "not_applicable"
    }
    $fixture = $Sample.gate_fixture
    return @(
        $fixture.canonical_tree_sha256,
        $fixture.autoexec_before_sha256,
        $fixture.bench_cfg_before_sha256,
        $fixture.autoexec_override_sha256,
        $fixture.bench_cfg_override_sha256,
        $fixture.exitvm_sha256,
        $fixture.prelaunch_overridden_tree_sha256,
        $fixture.stale_qconsole_absent_before_launch
    ) -join "|"
}

function Get-QuakeCompletionIdentityKey([string]$WorkloadName, $Sample) {
    if ($WorkloadName -cne "quake-586" -or
        $null -eq $Sample.PSObject.Properties["gate_quake_completion"] -or
        $null -eq $Sample.gate_quake_completion) {
        return "not_applicable"
    }
    $completion = $Sample.gate_quake_completion
    return @(
        $completion.identity_count,
        $completion.wait_marker,
        $completion.wait_marker_count,
        $completion.result_before_wait_marker,
        $completion.reported_values_consistent,
        $completion.fatal_match_count,
        $Sample.gate_artifacts.qconsole_sha256
    ) -join "|"
}

function Get-EqualWorkRecord([string]$WorkloadName, $Sample) {
    $resultStatus = [string]$Sample.gate_artifacts.result_block_status
    $resultHash = [string]$Sample.gate_artifacts.result_block_sha256
    $scaledBusClocks = if ($null -eq $Sample.PSObject.Properties["scaled_bus_clocks"] -or
        $null -eq $Sample.scaled_bus_clocks) {
        "not_recorded"
    } else {
        [uint64]$Sample.scaled_bus_clocks
    }
    return [ordered]@{
        instructions = [uint64]$Sample.perf.instructions
        master_ticks = [uint64]$Sample.master_ticks
        elapsed_budget_clocks = [uint64]$Sample.elapsed_budget_clocks
        executed_cpu_core_clocks = [uint64]$Sample.executed_cpu_core_clocks
        raw_bus_clocks = [uint64]$Sample.raw_bus_clocks
        scaled_bus_clocks = $scaledBusClocks
        stop = Get-StopIdentityKey $Sample
        timedemo_identity = Get-TimedemoIdentityKey $WorkloadName $Sample
        result_block_identity = "$resultStatus|$resultHash"
        measurement_fixture_identity = Get-MeasurementFixtureIdentityKey $Sample
        quake_completion_identity = Get-QuakeCompletionIdentityKey $WorkloadName $Sample
        hdd_tree_identity = if ($null -ne $Sample.PSObject.Properties["gate_hdd_tree"]) {
            [string]$Sample.gate_hdd_tree.tree_sha256
        } else { "not_recorded" }
        argv_identity = if ($null -ne $Sample.PSObject.Properties["gate_argv_sha256"]) {
            [string]$Sample.gate_argv_sha256
        } else { "not_recorded" }
    }
}

function Compare-EqualWorkRecords($Left, $Right) {
    $mismatches = @()
    foreach ($field in $Left.Keys) {
        $unobservedScaledBusClocks = $field -ceq "scaled_bus_clocks" -and
            ([string]$Left[$field] -ceq "not_recorded" -or
                [string]$Right[$field] -ceq "not_recorded")
        if ($unobservedScaledBusClocks -or
            [string]$Left[$field] -cne [string]$Right[$field]) {
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


function Assert-BackendWarmupSample($Sample, [string]$Role, [string]$WorkloadName) {
    foreach ($property in @(
        "gate_role", "gate_observation", "gate_processor_index",
        "gate_processor_affinity_mask", "gate_processor_affinity_verified",
        "gate_artifacts"
    )) {
        if ($null -eq $Sample.PSObject.Properties[$property]) {
            throw "$Role warmup is missing $property."
        }
    }
    if ($Sample.gate_role -cne $Role -or $Sample.gate_observation -cne "warmup") {
        throw "$Role warmup has the wrong role or observation identity."
    }
    if ($Sample.gate_processor_index -ne 8 -or
        [string]::IsNullOrWhiteSpace([string]$Sample.gate_processor_affinity_mask) -or
        -not $Sample.gate_processor_affinity_verified) {
        throw "$Role warmup is missing verified processor 8 affinity metadata."
    }
    foreach ($property in @(
        "profile_json_file", "profile_json_sha256", "stdout_file", "stdout_sha256",
        "stderr_file", "stderr_sha256", "qconsole_file", "qconsole_sha256",
        "result_block_status", "result_block_count", "result_block_sha256",
        "result_block_normalized_bytes"
    )) {
        if ($null -eq $Sample.gate_artifacts.PSObject.Properties[$property]) {
            throw "$Role warmup artifacts are missing $property."
        }
    }
    foreach ($property in @("profile_json_sha256", "stdout_sha256", "stderr_sha256")) {
        if ([string]$Sample.gate_artifacts.$property -notmatch '^[0-9a-f]{64}$') {
            throw "$Role warmup artifact $property is not a SHA-256 value."
        }
    }
    if ($Sample.gate_artifacts.result_block_status -ne "valid" -or
        $Sample.gate_artifacts.result_block_count -ne 1 -or
        [string]$Sample.gate_artifacts.result_block_sha256 -notmatch '^[0-9a-f]{64}$' -or
        $Sample.gate_artifacts.result_block_normalized_bytes -le 0) {
        throw "$WorkloadName $Role warmup does not contain one valid semantic result block."
    }
    if ($WorkloadName -eq "quake-586") {
        if ([string]::IsNullOrWhiteSpace([string]$Sample.gate_artifacts.qconsole_file) -or
            [string]$Sample.gate_artifacts.qconsole_sha256 -notmatch '^[0-9a-f]{64}$') {
            throw "$Role Quake warmup is missing its hashed console log."
        }
        if ($null -eq $Sample.PSObject.Properties["gate_process_exit_code"] -or
            $Sample.gate_process_exit_code -ne 0 -or
            (Get-StopIdentityKey $Sample) -cne "test_exit|code=0|requested=|message=") {
            throw "$Role Quake warmup did not reach Lotura TestExit code 0."
        }
        $completion = if ($null -ne $Sample.PSObject.Properties["gate_quake_completion"]) {
            $Sample.gate_quake_completion
        } else {
            $null
        }
        $fixture = if ($null -ne $Sample.PSObject.Properties["gate_fixture"]) {
            $Sample.gate_fixture
        } else {
            $null
        }
        $completionReasons = @(Get-BackendQuakeCompletionReasons $completion "$Role warmup")
        if ($completionReasons.Count -ne 0) {
            throw "$Role Quake warmup failed its completion protocol: $($completionReasons -join '; ')"
        }
        $fixtureReasons = @(Get-BackendQuakeFixtureReasons $fixture "$Role warmup")
        if ($fixtureReasons.Count -ne 0) {
            throw "$Role Quake warmup failed its fixture contract: $($fixtureReasons -join '; ')"
        }
    } elseif ($null -ne $Sample.gate_artifacts.qconsole_file -or
        $null -ne $Sample.gate_artifacts.qconsole_sha256) {
        throw "$WorkloadName $Role warmup must keep explicit null qconsole fields."
    }
}

function Get-BackendDiscardedWarmups($Bucket, [string]$WorkloadName) {
    $packaged = [ordered]@{}
    foreach ($role in @("automatic", "interpreter")) {
        $samples = @($Bucket.$role)
        if ($samples.Count -ne 1) {
            throw "$role must have exactly one discarded warmup."
        }
        Assert-BackendWarmupSample $samples[0] $role $WorkloadName
        $packaged[$role] = [object[]]$samples
    }
    return [pscustomobject]$packaged
}

. $gateSummaryScriptPath
. $gateSelfTestScriptPath
$gateSourceClosureAfterLoad = Get-GateSourceClosureIdentity `
    $gateMainScriptPath $gateSelfTestScriptPath $gateSummaryScriptPath
Assert-GateSourceClosureUnchanged `
    $gateSourceClosureAtEntry $gateSourceClosureAfterLoad "while loading support sources"

if ($SelfTest) {
    Invoke-RealtimeGateSelfTest
    return
}

$explicitExecutable = $PSBoundParameters.ContainsKey("Executable")
$explicitJit = $PSBoundParameters.ContainsKey("Jit")
$explicitRuns = $PSBoundParameters.ContainsKey("Runs")
$explicitBaseline = $PSBoundParameters.ContainsKey("BaselineRevision")
$explicitExecutionRole = $PSBoundParameters.ContainsKey("ExecutionRole")
$explicitPairSeed = $PSBoundParameters.ContainsKey("PairSeed")
$explicitCampaignStage = $PSBoundParameters.ContainsKey("CampaignStage")
$trackMExecutionPolicy = $null
$pollSkipExecutionPolicies = $null
$directQuakeExecutionPolicy = $null
if (-not $DirectQuakeCampaign -and $explicitCampaignStage) {
    throw "CampaignStage is only valid with DirectQuakeCampaign."
}
if ($DirectQuakeCampaign) {
    $CampaignStage = Get-NormalizedDirectQuakeCampaignStage $CampaignStage
    Assert-DirectQuakeCampaignMode `
        ([bool]$BackendBakeoff) ([bool]$TrackMComparison) ([bool]$PollSkipComparison) `
        ([bool]$ReportOnly) $explicitBaseline $explicitJit $explicitExecutable `
        ([bool]$SkipBuild) $explicitExecutionRole ([bool]$Screening) `
        $explicitPairSeed $CampaignStage $Runs $Workload $ProcessorIndex `
        $MeasurementLockPath
    $directQuakeExecutionPolicy = Get-DirectQuakeExecutionPolicy
} elseif ($PollSkipComparison) {
    Assert-PollSkipComparisonMode `
        ([bool]$BackendBakeoff) ([bool]$TrackMComparison) ([bool]$ReportOnly) `
        $explicitBaseline $explicitJit $explicitExecutable ([bool]$SkipBuild) `
        $explicitExecutionRole ([bool]$Screening) $Runs $Workload `
        $ProcessorIndex $MeasurementLockPath
    $pollSkipExecutionPolicies = [ordered]@{
        skip_off = Get-PollSkipExecutionPolicy "skip_off"
        skip_on = Get-PollSkipExecutionPolicy "skip_on"
    }
} elseif ($TrackMComparison) {
    Assert-TrackMComparisonMode `
        ([bool]$BackendBakeoff) ([bool]$ReportOnly) $explicitBaseline `
        $explicitJit $explicitExecutable ([bool]$SkipBuild) `
        $ExecutionRole ([bool]$Screening) $explicitRuns $Runs `
        $Workload $ProcessorIndex $MeasurementLockPath
    $trackMExecutionPolicy = Get-TrackMExecutionPolicy $ExecutionRole
    if ($Screening) {
        $Runs = 3
    }
} elseif ($Screening -and -not $BackendBakeoff) {
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
} elseif (-not $TrackMComparison -and -not $PollSkipComparison -and
    -not $DirectQuakeCampaign) {
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
$captureProofArtifacts = [bool]($BackendBakeoff -or $TrackMComparison -or
    $PollSkipComparison -or $DirectQuakeCampaign)
$revisionProofComparison = [bool]($TrackMComparison -or $DirectQuakeCampaign)
$revisionExecutionPolicy = if ($DirectQuakeCampaign) {
    $directQuakeExecutionPolicy
} else {
    $trackMExecutionPolicy
}
$artifactSelection = Get-ArtifactSelectionPolicy ([bool]$ReportOnly) $explicitExecutable ([bool]$SkipBuild)
if (-not $DirectQuakeCampaign -and $PairSeed -eq 0) {
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
$pollSkipPowerSchemeEligible = -not $PollSkipComparison -or
    ([string]$activePowerScheme).Contains(
        $highPerformancePowerSchemeGuid,
        [StringComparison]::OrdinalIgnoreCase
    )
if ($PollSkipComparison -and -not $pollSkipPowerSchemeEligible) {
    throw "POLL-SKIP comparison requires the High Performance power scheme."
}
$directQuakePowerSchemeEligible = -not $DirectQuakeCampaign -or
    ([string]$activePowerScheme).Contains(
        $highPerformancePowerSchemeGuid,
        [StringComparison]::OrdinalIgnoreCase
    )
if ($DirectQuakeCampaign -and -not $directQuakePowerSchemeEligible) {
    throw "Direct Quake campaign mode requires the High Performance power scheme."
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
if ($revisionProofComparison) {
    $baselineCommit = Get-TrackMImmediateParent $repositoryRoot $revision
    $baselineTree = (& git -C $repositoryRoot rev-parse --verify "$baselineCommit^{tree}").Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($baselineTree)) {
        throw "Unable to resolve the Track M parent tree."
    }
    $BaselineRevision = $baselineCommit
} elseif (-not [string]::IsNullOrWhiteSpace($BaselineRevision)) {
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

$measurementLockLease = $null
$measurementLockEvidence = $null
if ($captureProofArtifacts) {
    $MeasurementLockPath = [IO.Path]::GetFullPath($MeasurementLockPath)
    $measurementLockLease = Enter-MeasurementLock $MeasurementLockPath
    $measurementLockEvidence = Get-MeasurementLockEvidence $measurementLockLease
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
    $artifactLabel = if ($revisionProofComparison) { "parent" } else { "baseline" }
    $baselineArtifact = Invoke-IsolatedRevisionBuild $repositoryRoot $baselineCommit $artifactLabel $ResultsDirectory
}
$revisionPairedRun = $null -ne $baselineArtifact
$pairedRun = $BackendBakeoff -or $PollSkipComparison -or $revisionPairedRun
if (-not $ReportOnly -and -not $pairedRun) {
    throw "The formal gate requires a freshly built baseline artifact."
}
if ($revisionPairedRun -and $candidateArtifact.verified -and $baselineArtifact.verified -and
    $candidateArtifact.build.recipe_fingerprint_sha256 -ne
        $baselineArtifact.build.recipe_fingerprint_sha256) {
    throw "Candidate and baseline were not built with the same isolated recipe and toolchain."
}
if ($DirectQuakeCampaign) {
    Assert-DirectQuakeExecutableRelation `
        $CampaignStage $candidateArtifact.sha256 $baselineArtifact.sha256
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
    if ($TrackMComparison -and $BackendRole -notin @("candidate", "parent")) {
        throw "Unknown Track M revision role '$BackendRole'."
    }
    if ($DirectQuakeCampaign -and $BackendRole -notin @("candidate", "parent")) {
        throw "Unknown Direct Quake campaign role '$BackendRole'."
    }
    if ($PollSkipComparison -and $BackendRole -notin @("skip_off", "skip_on")) {
        throw "Unknown POLL-SKIP comparison role '$BackendRole'."
    }
    $argumentLine = ($Arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
    $roleEnvironment = if ($PollSkipComparison) {
        $pollSkipExecutionPolicies[$BackendRole].environment
    } elseif ($BackendBakeoff) {
        [ordered]@{
            IZARRAVM_JIT = if ($BackendRole -eq "automatic") { "1" } else { "0" }
        }
    } elseif ($revisionProofComparison) {
        $revisionExecutionPolicy.environment
    } else {
        [ordered]@{ IZARRAVM_JIT = $Jit }
    }
    $childEnvironment = New-IzarraChildEnvironment `
        $HomePath $diagnosticVariables $roleEnvironment
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

function Copy-CampaignObservationFixture([string]$Source, [string]$Destination) {
    $sourcePath = [IO.Path]::GetFullPath($Source)
    $destinationPath = [IO.Path]::GetFullPath($Destination)
    $privateRoot = [IO.Path]::GetFullPath($temporaryRoot).
        TrimEnd([IO.Path]::DirectorySeparatorChar)
    $privatePrefix = $privateRoot + [IO.Path]::DirectorySeparatorChar
    if (-not $destinationPath.StartsWith($privatePrefix, [StringComparison]::OrdinalIgnoreCase) -or
        $destinationPath -ceq $privateRoot) {
        throw "Refusing to create a campaign fixture outside its private temporary root."
    }
    if (Test-Path -LiteralPath $destinationPath) {
        throw "A campaign observation fixture path was reused."
    }
    $robocopy = Get-Command robocopy.exe -CommandType Application -ErrorAction Stop
    $output = @(& $robocopy.Source $sourcePath $destinationPath /E /COPY:DAT /DCOPY:DAT `
        /R:1 /W:1 /NFL /NDL /NJH /NJS /NP 2>&1)
    $code = $LASTEXITCODE
    if ($code -lt 0 -or $code -gt 7) {
        throw "robocopy failed for a campaign observation fixture with code ${code}: $($output -join ' ')"
    }
    if (-not (Test-Path -LiteralPath $destinationPath -PathType Container)) {
        throw "robocopy did not create the campaign observation fixture."
    }
}

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

$observationSerial = 0

function Invoke-Observation(
    $Policy,
    [string]$SourceFolder,
    [string]$Role,
    [string]$ObservationId,
    [string]$ExecutablePath,
    [ValidateSet("production", "correctness")]
    [string]$ObservationClass = "production"
) {
    $context = "$($Policy.name) $Role $ObservationId"
    $serialName = $null
    if ($DirectQuakeCampaign) {
        $script:observationSerial++
        $serialName = "observation-{0:D4}" -f $script:observationSerial
        $fixture = Join-Path $temporaryRoot "$serialName-fixture"
        $observationHome = Join-Path $temporaryRoot "$serialName-home"
        Copy-CampaignObservationFixture $SourceFolder $fixture
    } else {
        $fixture = Join-Path $temporaryRoot "$($Policy.name)-$Role-$ObservationId"
        $observationHome = Join-Path $temporaryRoot `
            "home-$($Policy.name)-$Role-$ObservationId"
        Copy-Item -LiteralPath $SourceFolder -Destination $fixture -Recurse
    }
    New-Item -ItemType Directory -Path $observationHome | Out-Null
    $qconsole = Join-Path $fixture "QUAKE/ID1/QCONSOLE.LOG"
    if ($DirectQuakeCampaign) {
        $copiedCanonicalTree = Get-DirectoryTreeSha256 `
            $fixture $quakeCanonicalTreeExclusions
        if ($copiedCanonicalTree -cne $workloadCanonicalTreeHashes.quake) {
            throw "$context robocopy fixture does not match the frozen Quake tree."
        }
    }
    $campaignCorrectness = $DirectQuakeCampaign -and $ObservationClass -ceq "correctness"
    $campaignProduction = $DirectQuakeCampaign -and $ObservationClass -ceq "production"
    $useBackendQuakeCompletion = Test-BackendQuakeCompletionOverride `
        ($captureProofArtifacts -and -not $campaignProduction) $Policy.name
    $fixtureEvidence = $null
    if ($useBackendQuakeCompletion) {
        $fixtureEvidence = Set-BackendQuakeCompletionFixture `
            $fixture $workloadCanonicalTreeHashes.quake $exitVmBytes $exitVmHash
    } else {
        [IO.File]::WriteAllBytes((Join-Path $fixture "EXITVM.COM"), $exitVmBytes)
        if (Test-Path -LiteralPath $qconsole -PathType Leaf) {
            Remove-Item -LiteralPath $qconsole
        }
    }
    $measurementFixtureHash = if ($revisionProofComparison -or $PollSkipComparison) {
        Get-DirectoryTreeSha256 $fixture
    } else {
        $null
    }

    $fileStem = if ($DirectQuakeCampaign) { $serialName } else {
        "$($Policy.name)-$Role-$ObservationId"
    }
    $jsonPath = Join-Path $ResultsDirectory "$fileStem.json"
    $stdoutPath = Join-Path $ResultsDirectory "$fileStem.stdout.log"
    $stderrPath = Join-Path $ResultsDirectory "$fileStem.stderr.log"
    Remove-Item -LiteralPath $jsonPath -Force -ErrorAction SilentlyContinue
    $processArguments = @(
        "--cpu", $Policy.mode,
        "--memory-mib", "64",
        "--video", "vega",
        "--hdd-folder", $fixture,
        "--cycles", $Policy.cycle_budget.ToString(),
        "--dump-result",
        "--profile-json", $jsonPath
    )
    if (Test-ObservationRequiresTestExit `
        ($captureProofArtifacts -and -not $campaignProduction) $Policy.name) {
        $processArguments += "--expect-test-exit"
    }
    if ($PollSkipComparison -or
        ($BackendBakeoff -and $Role -eq "interpreter") -or
        ($TrackMComparison -and $revisionExecutionPolicy.name -eq "interpreter")) {
        $processArguments += "--interpreter"
    }
    $powerSchemeBefore = if ($DirectQuakeCampaign) { Get-ActivePowerScheme } else { $null }
    if ($DirectQuakeCampaign -and
        -not ([string]$powerSchemeBefore).Contains(
            $highPerformancePowerSchemeGuid,
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw "$context did not start under the High Performance power scheme."
    }
    $processResult = Invoke-IzarraProcess `
        $ExecutablePath $processArguments $stdoutPath $stderrPath $observationHome $Role
    $powerSchemeAfter = if ($DirectQuakeCampaign) { Get-ActivePowerScheme } else { $null }
    if ($DirectQuakeCampaign -and
        ($powerSchemeAfter -cne $powerSchemeBefore -or
         -not ([string]$powerSchemeAfter).Contains(
             $highPerformancePowerSchemeGuid,
             [StringComparison]::OrdinalIgnoreCase
         ))) {
        throw "$context did not keep the High Performance power scheme."
    }
    if ($processResult.exit_code -ne 0 -and -not $captureProofArtifacts) {
        throw "$context failed with exit code $($processResult.exit_code). See $stdoutPath and $stderrPath."
    }
    if (-not (Test-Path -LiteralPath $jsonPath -PathType Leaf)) {
        throw "$context did not produce its profile JSON."
    }
    $profileHash = if ($captureProofArtifacts) { Get-FileSha256 $jsonPath } else { $null }
    $sample = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json
    if ($sample.schema -ne "izarravm-hdd-profile-v1" -or $sample.mode -ne $Policy.mode) {
        throw "$context produced an unexpected schema or CPU mode."
    }
    Assert-UninstrumentedProfileSample $sample $context
    if (-not $captureProofArtifacts -and
        $Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
        if ($sample.stop.kind -ne "test_exit" -or $sample.stop.code -ne 0) {
            throw "$context did not reach TestExit code 0."
        }
    } elseif (-not $captureProofArtifacts -and
        ($sample.stop.kind -ne "cycle_limit" -or
         [uint64]$sample.stop.requested -ne $Policy.cycle_budget)) {
        throw "$context did not reach its fixed cycle limit."
    } elseif ($campaignCorrectness -and
        ($processResult.exit_code -ne 0 -or $sample.stop.kind -ne "test_exit" -or
         $sample.stop.code -ne 0)) {
        throw "$context did not complete through TestExit code 0."
    } elseif ($campaignProduction -and
        ($processResult.exit_code -ne 0 -or $sample.stop.kind -ne "cycle_limit" -or
         [uint64]$sample.stop.requested -ne $Policy.cycle_budget)) {
        throw "$context did not reach the production fixed-cycle limit."
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
        if (-not $captureProofArtifacts -and
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
            if ($captureProofArtifacts) {
                $qconsoleHash = Get-FileSha256 $preservedQconsole
            }
        }
        if ($captureProofArtifacts -and -not $campaignProduction) {
            $quakeCompletion = Read-BackendQuakeCompletion `
                $preservedQconsole @($stdoutPath, $stderrPath)
            $quakeIdentity = $quakeCompletion.timedemo
            $sample | Add-Member `
                -NotePropertyName quake_timedemo_identity_count `
                -NotePropertyValue $quakeCompletion.identity_count
            $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
            $sample | Add-Member `
                -NotePropertyName gate_quake_completion `
                -NotePropertyValue $quakeCompletion
        } else {
            $quakeIdentity = Read-QuakeTimedemoIdentity $preservedQconsole
            $sample | Add-Member -NotePropertyName quake_timedemo -NotePropertyValue $quakeIdentity
            if ($campaignProduction) {
                $sample | Add-Member -NotePropertyName gate_quake_completion -NotePropertyValue $null
            }
        }
    }
    if ($captureProofArtifacts) {
        $hddTree = $null
        $hddTreePath = $null
        $argvIdentity = $null
        if ($DirectQuakeCampaign) {
            $hddTree = Get-HddTreeSnapshotV1 $fixture
            $hddTreePath = Join-Path $ResultsDirectory "$fileStem-hdd-tree.json"
            $hddTree | ConvertTo-Json -Depth 6 |
                Set-Content -LiteralPath $hddTreePath -Encoding utf8
            $normalizedArguments = [string[]]@($processArguments)
            for ($argumentIndex = 0; $argumentIndex -lt $normalizedArguments.Count - 1;
                $argumentIndex++) {
                if ($normalizedArguments[$argumentIndex] -ceq "--hdd-folder") {
                    $normalizedArguments[$argumentIndex + 1] = "<fresh-hdd-fixture>"
                } elseif ($normalizedArguments[$argumentIndex] -ceq "--profile-json") {
                    $normalizedArguments[$argumentIndex + 1] = "<profile-json>"
                }
            }
            $argvIdentity = Get-BytesSha256 (
                [Text.Encoding]::UTF8.GetBytes(($normalizedArguments -join "`0"))
            )
        }
        $resultBlock = Get-NormalizedResultBlock $stdoutPath
        $sample | Add-Member `
            -NotePropertyName gate_process_exit_code `
            -NotePropertyValue $processResult.exit_code
        if ($PollSkipComparison) {
            $rolePolicy = $pollSkipExecutionPolicies[$Role]
            $sample | Add-Member -NotePropertyName gate_execution_cli `
                -NotePropertyValue $rolePolicy.cli
            $sample | Add-Member -NotePropertyName gate_execution_jit `
                -NotePropertyValue $rolePolicy.environment.IZARRAVM_JIT
            $sample | Add-Member -NotePropertyName gate_poll_skip `
                -NotePropertyValue $rolePolicy.environment.IZARRAVM_POLL_SKIP
            $sample | Add-Member -NotePropertyName gate_measurement_fixture_sha256 `
                -NotePropertyValue $measurementFixtureHash
        } elseif ($BackendBakeoff) {
            $sample | Add-Member -NotePropertyName gate_backend_policy -NotePropertyValue $Role
        } elseif ($revisionProofComparison) {
            $sample | Add-Member -NotePropertyName gate_execution_role `
                -NotePropertyValue $revisionExecutionPolicy.name
            $sample | Add-Member -NotePropertyName gate_execution_cli `
                -NotePropertyValue $revisionExecutionPolicy.cli
            $sample | Add-Member -NotePropertyName gate_execution_jit `
                -NotePropertyValue $revisionExecutionPolicy.environment.IZARRAVM_JIT
            $sample | Add-Member -NotePropertyName gate_poll_skip `
                -NotePropertyValue $revisionExecutionPolicy.environment.IZARRAVM_POLL_SKIP
            $sample | Add-Member -NotePropertyName gate_measurement_fixture_sha256 `
                -NotePropertyValue $measurementFixtureHash
        } else {
            throw "Proof artifact capture received an unknown execution policy."
        }
        $sample | Add-Member `
            -NotePropertyName gate_termination_policy `
            -NotePropertyValue $(if ($campaignProduction) {
                "fixed_cycle_production"
            } else {
                "lotura_test_exit"
            })
        if ($DirectQuakeCampaign) {
            $sample | Add-Member -NotePropertyName gate_observation_class `
                -NotePropertyValue $ObservationClass
            $sample | Add-Member -NotePropertyName gate_power_scheme_before `
                -NotePropertyValue $powerSchemeBefore
            $sample | Add-Member -NotePropertyName gate_power_scheme_after `
                -NotePropertyValue $powerSchemeAfter
            $sample | Add-Member -NotePropertyName gate_argv `
                -NotePropertyValue ([object[]]$processArguments)
            $sample | Add-Member -NotePropertyName gate_argv_sha256 `
                -NotePropertyValue $argvIdentity
            $sample | Add-Member -NotePropertyName gate_executable_sha256 `
                -NotePropertyValue (Get-FileSha256 $ExecutablePath)
            $sample | Add-Member -NotePropertyName gate_hdd_tree `
                -NotePropertyValue $hddTree
        }
        $sample | Add-Member -NotePropertyName gate_fixture -NotePropertyValue $fixtureEvidence
        $artifactEvidence = [pscustomobject][ordered]@{
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
        }
        if ($DirectQuakeCampaign) {
            $artifactEvidence | Add-Member -NotePropertyName hdd_tree_file `
                -NotePropertyValue ([IO.Path]::GetFileName($hddTreePath))
            $artifactEvidence | Add-Member -NotePropertyName hdd_tree_sha256 `
                -NotePropertyValue (Get-FileSha256 $hddTreePath)
        }
        $sample | Add-Member -NotePropertyName gate_artifacts -NotePropertyValue $artifactEvidence
    }
    $sample | Add-Member -NotePropertyName gate_role -NotePropertyValue $Role
    $sample | Add-Member -NotePropertyName gate_observation -NotePropertyValue $ObservationId
    $sample | Add-Member -NotePropertyName gate_processor_index -NotePropertyValue $processResult.processor_index
    $sample | Add-Member -NotePropertyName gate_processor_affinity_mask -NotePropertyValue $processResult.processor_affinity_mask
    $sample | Add-Member -NotePropertyName gate_processor_affinity_verified -NotePropertyValue $processResult.processor_affinity_verified
    return $sample
}


$knownDiagnosticVariables = @(Get-KnownDiagnosticVariables)
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
$doomFrozenStable = $true
$quakeFrozenStable = $true
try {
    $savedEnvironment["IZARRAVM_JIT"] = [Environment]::GetEnvironmentVariable("IZARRAVM_JIT", "Process")
    foreach ($name in $diagnosticVariables) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        Set-GateProcessEnvironment $name $null
    }
    $gateJitEnvironment = if ($captureProofArtifacts) { $null } else { $Jit }
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
    $policies = @(Get-WorkloadPolicies $Workload)
    $pairRoles = if ($PollSkipComparison) {
        @("skip_on", "skip_off")
    } elseif ($BackendBakeoff) {
        @("automatic", "interpreter")
    } elseif ($revisionProofComparison) {
        @("candidate", "parent")
    } else {
        @("candidate", "baseline")
    }
    $observations = [ordered]@{}
    $discardedWarmups = [ordered]@{}
    $correctnessObservations = [ordered]@{}
    foreach ($policy in $policies) {
        $roleBuckets = [ordered]@{}
        $warmupBuckets = [ordered]@{}
        foreach ($role in $pairRoles) {
            $roleBuckets[$role] = @()
            $warmupBuckets[$role] = @()
        }
        $observations[$policy.name] = $roleBuckets
        $discardedWarmups[$policy.name] = $warmupBuckets
        if ($DirectQuakeCampaign) {
            $correctnessBuckets = [ordered]@{}
            foreach ($role in $pairRoles) {
                $correctnessBuckets[$role] = @()
            }
            $correctnessObservations[$policy.name] = $correctnessBuckets
        }
    }

    if ($pairedRun) {
        foreach ($policy in $policies) {
            $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                $doomSnapshot.frozen_path
            } else {
                $quakeSnapshot.frozen_path
            }
            if ($DirectQuakeCampaign) {
                foreach ($role in @(Get-DirectQuakePairOrder 1 $pairRoles)) {
                    $artifact = if ($CampaignStage -ceq "Noise" -or $role -eq "parent") {
                        $baselineArtifact
                    } else {
                        $candidateArtifact
                    }
                    $correctness = Invoke-Observation `
                        $policy $sourceFolder $role "correctness" `
                        $artifact.executed_copy_path "correctness"
                    $correctnessObservations[$policy.name][$role] += $correctness
                }
            }
            $warmupOrder = if ($PollSkipComparison) {
                Get-PollSkipWarmupOrder
            } elseif ($DirectQuakeCampaign) {
                Get-DirectQuakePairOrder 1 $pairRoles
            } else {
                Get-PairOrder 1 $PairSeed $pairRoles
            }
            foreach ($role in $warmupOrder) {
                $artifact = if ($DirectQuakeCampaign -and $CampaignStage -ceq "Noise") {
                    $baselineArtifact
                } elseif ($BackendBakeoff -or $PollSkipComparison -or $role -eq "candidate") {
                    $candidateArtifact
                } else {
                    $baselineArtifact
                }
                $warmup = Invoke-Observation `
                    $policy $sourceFolder $role "warmup" $artifact.executed_copy_path
                if ($PollSkipComparison) {
                    Assert-PollSkipSample $warmup $role "warmup" $policy
                }
                $discardedWarmups[$policy.name][$role] += $warmup
            }
            if ($PollSkipComparison) {
                $null = Assert-PollSkipPair `
                    $policy.name `
                    $discardedWarmups[$policy.name].skip_on[0] `
                    $discardedWarmups[$policy.name].skip_off[0] `
                    "warmup"
            }
        }
        for ($pair = 1; $pair -le $Runs; $pair++) {
            $roleOrder = if ($DirectQuakeCampaign) {
                Get-DirectQuakePairOrder $pair $pairRoles
            } else {
                Get-PairOrder $pair $PairSeed $pairRoles
            }
            for ($workloadOffset = 0; $workloadOffset -lt $policies.Count; $workloadOffset++) {
                $policy = $policies[($workloadOffset + $pair - 1) % $policies.Count]
                $sourceFolder = if ($policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
                    $doomSnapshot.frozen_path
                } else {
                    $quakeSnapshot.frozen_path
                }
                $pollSkipPairSamples = [ordered]@{}
                foreach ($role in $roleOrder) {
                    $artifact = if ($DirectQuakeCampaign -and $CampaignStage -ceq "Noise") {
                        $baselineArtifact
                    } elseif ($BackendBakeoff -or $PollSkipComparison -or $role -eq "candidate") {
                        $candidateArtifact
                    } else {
                        $baselineArtifact
                    }
                    $sample = Invoke-Observation $policy $sourceFolder $role "pair$pair" $artifact.executed_copy_path
                    if ($PollSkipComparison) {
                        Assert-PollSkipSample $sample $role "pair$pair" $policy
                        Assert-PollSkipRoleReference `
                            $policy.name $role `
                            $discardedWarmups[$policy.name][$role][0] $sample
                        $pollSkipPairSamples[$role] = $sample
                    }
                    $bucket = $observations[$policy.name]
                    $bucket[$role] += $sample
                }
                if ($PollSkipComparison) {
                    $null = Assert-PollSkipPair `
                        $policy.name $pollSkipPairSamples.skip_on `
                        $pollSkipPairSamples.skip_off "pair $pair"
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
        if ($PollSkipComparison) {
            $workloads += Get-PollSkipWorkloadSummary `
                $policy $bucket.skip_on $bucket.skip_off `
                $discardedWarmups[$policy.name]
        } elseif ($DirectQuakeCampaign) {
            $workloads += Get-DirectQuakeCampaignWorkloadSummary `
                $policy $bucket.candidate $bucket.parent `
                $discardedWarmups[$policy.name] $correctnessObservations[$policy.name] `
                $directQuakeExecutionPolicy $CampaignStage `
                $candidateArtifact.sha256 $baselineArtifact.sha256
        } elseif ($TrackMComparison) {
            $workloads += Get-TrackMWorkloadSummary `
                $policy $bucket.candidate $bucket.parent `
                $discardedWarmups[$policy.name] $trackMExecutionPolicy
        } elseif ($BackendBakeoff) {
            $backendSummary = Get-BackendWorkloadSummary `
                $policy $bucket.automatic $bucket.interpreter ([bool]$Screening)
            $workloadWarmups = Get-BackendDiscardedWarmups `
                $discardedWarmups[$policy.name] $policy.name
            if ($policy.name -eq "quake-586") {
                $allQuakeSamples = @(
                    @($bucket.automatic) + @($bucket.interpreter) +
                    @($discardedWarmups[$policy.name].automatic) +
                    @($discardedWarmups[$policy.name].interpreter)
                )
                $backendSummary["completion_fixture_identity"] = `
                    Assert-BackendQuakeFixtureSet $allQuakeSamples
            }
            $backendSummary["discarded_warmups"] = $workloadWarmups
            $workloads += $backendSummary
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
    $doomFrozenStable = $null -eq $doomSnapshot -or
        (Get-DirectoryTreeSha256 $doomSnapshot.frozen_path) -eq $doomSnapshot.frozen_sha256
    $quakeFrozenStable = $null -eq $quakeSnapshot -or
        (Get-DirectoryTreeSha256 $quakeSnapshot.frozen_path) -eq $quakeSnapshot.frozen_sha256
    if (-not $revisionProofComparison -and -not $doomFrozenStable) {
        throw "The frozen Doom workload changed during the gate."
    }
    if (-not $revisionProofComparison -and -not $quakeFrozenStable) {
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
if ($null -ne $outerAffinityRestoreFailure -and -not $revisionProofComparison) {
    throw $outerAffinityRestoreFailure
}

$verifiedChildAffinityStable = $true
if ($ProcessorIndex -ge 0) {
    $expectedVerifiedChildren = if ($pairedRun) {
        $policies.Count * ($(if ($DirectQuakeCampaign) { 4 } else { 2 }) + 2 * $Runs)
    } else {
        $policies.Count * $Runs
    }
    $verifiedChildAffinityStable = $verifiedChildAffinityMasks.Count -eq $expectedVerifiedChildren
    if (-not $revisionProofComparison -and -not $verifiedChildAffinityStable) {
        throw "Not every warmup and measured child received a verified processor affinity."
    }
}

$candidateExecutableStable = $candidateHashAfter -eq $candidateArtifact.sha256
if (-not $revisionProofComparison -and -not $candidateExecutableStable) {
    throw "The frozen candidate executable changed during the gate."
}
$parentExecutableStable = $true
if ($revisionPairedRun) {
    $parentExecutableStable = $baselineHashAfter -eq $baselineArtifact.sha256
    if (-not $revisionProofComparison -and -not $parentExecutableStable) {
        throw "The frozen baseline executable changed during the gate."
    }
}
$doomSourceStable = $null -eq $doomSnapshot -or
    (Get-DirectoryTreeSha256 $DoomFolder) -eq $doomSnapshot.source_initial_sha256
$quakeSourceStable = $null -eq $quakeSnapshot -or
    (Get-DirectoryTreeSha256 $QuakeFolder) -eq $quakeSnapshot.source_initial_sha256
if (-not $revisionProofComparison -and -not $doomSourceStable) {
    throw "The Doom workload tree changed during the gate."
}
if (-not $revisionProofComparison -and -not $quakeSourceStable) {
    throw "The Quake workload tree changed during the gate."
}
$gateSourceClosureAtCompletion = Get-GateSourceClosureIdentity `
    $gateMainScriptPath $gateSelfTestScriptPath $gateSummaryScriptPath
$gateSourceClosureStable = @(Get-GateSourceClosureMismatches `
    $gateSourceClosureAtEntry $gateSourceClosureAtCompletion).Count -eq 0
if (-not $revisionProofComparison) {
    Assert-GateSourceClosureUnchanged `
        $gateSourceClosureAtEntry $gateSourceClosureAtCompletion "during the measurement"
}
$gateScriptHashAfter = $gateSourceClosureAtCompletion.members[0].sha256
if (-not $revisionProofComparison -and $gateScriptHashAfter -ne $gateScriptHash) {
    throw "The throughput gate script changed during the measurement."
}
$gateSourceClosureEvidence = [ordered]@{
    schema = "izarravm-gate-source-closure-v1"
    at_entry = ConvertTo-GateSourceClosureEvidence $gateSourceClosureAtEntry
    at_completion = ConvertTo-GateSourceClosureEvidence $gateSourceClosureAtCompletion
}
$fixtureManifestHashAfter = (Get-FileHash -LiteralPath $fixtureManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$fixtureManifestStable = $fixtureManifestHashAfter -eq $fixtureManifestHash
if (-not $revisionProofComparison -and -not $fixtureManifestStable) {
    throw "The accepted workload manifest changed during the measurement."
}
$repositoryAtCompletion = Get-RepositoryState $repositoryRoot
$repositoryStable = $repositoryAtCompletion.head_commit -eq $repositoryAtSelection.head_commit -and
    ($repositoryAtCompletion.status -join "`n") -eq ($repositoryAtSelection.status -join "`n")
if (-not $revisionProofComparison -and -not $ReportOnly -and -not $repositoryStable) {
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

if ($PollSkipComparison) {
    $summary = New-PollSkipComparisonSummary $workloads
} elseif ($DirectQuakeCampaign) {
    $summary = New-DirectQuakeCampaignSummary $workloads
} elseif ($TrackMComparison) {
    $summary = New-TrackMComparisonSummary $workloads
} elseif ($BackendBakeoff) {
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
    $baseFinalEligible = $evidencePolicy.final_eligible -and
        $candidateArtifact.verified -and
        $candidateArtifact.built_this_invocation -and
        -not $repositoryAtSelection.dirty -and
        $Runs -eq 6 -and
        $ProcessorIndex -eq 8 -and
        $verifiedChildAffinityMasks.Count -eq $policies.Count * (2 + 2 * $Runs) -and
        $powerSchemeStable
    $survivalFailures = @($workloads | Where-Object { $_.survival.verdict -ne "pass" })
    $survivalComponentFailures = @(
        Get-FailedBackendSurvivalComponents $aggregateVerdicts
    )
    $finalTerminationReasons = @(
        Get-BackendFinalTerminationReasonsFromWorkloads $workloads
    )
    $classification = Get-BackendFinalClassification `
        ([bool]$Screening) `
        $baseFinalEligible `
        $finalTerminationReasons.Count `
        ($survivalComponentFailures.Count -eq 0 -and $survivalFailures.Count -eq 0)
    $finalEligible = $classification.final_eligible
    $trackASurvival = $classification.track_a_survival
    $verdict = $classification.verdict
    $backendFailures = @()
    if (-not $powerSchemeStable) {
        $backendFailures += if ($powerSchemeRecorded) {
            "the active power scheme changed during the bakeoff"
        } else {
            "the active power scheme could not be recorded"
        }
    }
    if ($finalTerminationReasons.Count -gt 0) {
        $backendFailures += "quake-586: $($finalTerminationReasons.Count) measured observations failed the required post-timedemo TestExit contract; final proof requires one 969-frame result, one later wait marker, and Lotura TestExit code 0"
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
    $quakeCompletionFixtureSummary = $null
    $quakeBackendWorkloads = @($workloads | Where-Object { $_.name -ceq "quake-586" })
    if ($quakeBackendWorkloads.Count -eq 1) {
        $quakeBackendWorkload = $quakeBackendWorkloads[0]
        $firstQuakeSample = @($quakeBackendWorkload.automatic.runs)[0]
        $firstQuakeFixture = $firstQuakeSample.gate_fixture
        $quakeCompletionFixtureSummary = [ordered]@{
            fixture_class = "backend_bakeoff_quake_completion_v1"
            fresh_copy_per_observation = $true
            canonical_tree_sha256 = $firstQuakeFixture.canonical_tree_sha256
            autoexec_before_sha256 = $firstQuakeFixture.autoexec_before_sha256
            bench_cfg_before_sha256 = $firstQuakeFixture.bench_cfg_before_sha256
            izarra_autoexec_override_sha256 = $backendQuakeAutoexecSha256
            bench_cfg_override_sha256 = $backendQuakeBenchCfgSha256
            wait_marker = $backendQuakeWaitMarker
            prelaunch_overridden_tree_sha256 = `
                $firstQuakeFixture.prelaunch_overridden_tree_sha256
            all_observations_fixture_identity = $quakeBackendWorkload.completion_fixture_identity
            same_izarra_bytes_across_roles = $true
            cycle_budget_is_safety_ceiling = $true
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
        gate_source_closure = $gateSourceClosureEvidence
        workload_manifest_sha256 = [ordered]@{
            at_entry = $fixtureManifestHash
            at_completion = $fixtureManifestHashAfter
        }
        injected_exitvm_sha256 = $exitVmHash
        workload_inputs_sha256 = $workloadInputHashes
        workload_trees_sha256 = $workloadTreeHashes
        workload_canonical_trees_sha256 = $workloadCanonicalTreeHashes
        quake_completion_fixture = $quakeCompletionFixtureSummary
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
            quake = "exactly one 969-frame timedemo, the fixed post-demo wait marker, then Lotura TestExit code 0"
            quake_diagnostic = "fixed 6.2G cycle limit with exactly one 969-frame identity; never final-eligible"
        }
        acceptance = [ordered]@{
            # Derived from Get-WorkloadPolicy so this evidence block cannot
            # drift from the enforced floors: literals here survived TWO
            # re-pins (the 3.5 was the pre-2026-08-04 aspirational floor) and
            # published contradictory acceptance criteria into every summary.
            workload_real_time_factor_floors = [ordered]@{
                doom_486 = (Get-WorkloadPolicy "doom-486").minimum_real_time_factor
                doom_586 = (Get-WorkloadPolicy "doom-586").minimum_real_time_factor
                quake_586 = (Get-WorkloadPolicy "quake-586").minimum_real_time_factor
            }
            minimum_backend_median_ratio = 1.05
            minimum_backend_lower_95_ratio_exclusive = 1.0
            paired_lower_bound = "one-sided 95% Student-t"
            exact_work_fields = @(
                "perf.instructions", "master_ticks", "elapsed_budget_clocks",
                "executed_cpu_core_clocks", "raw_bus_clocks", "scaled_bus_clocks", "stop",
                "timedemo_identity", "result_block_sha256",
                "measurement_fixture_identity", "quake_completion_identity"
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
            $reasons += ("one or more candidate samples have direct-native coverage below " +
                "{0:P2}" -f $candidateChecks.coverage_floor)
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
    gate_source_closure = $gateSourceClosureEvidence
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
        # Derived, not literal — see the matching comment in the bakeoff
        # branch's acceptance block.
        workload_real_time_factor_floors = [ordered]@{
            doom_486 = (Get-WorkloadPolicy "doom-486").minimum_real_time_factor
            doom_586 = (Get-WorkloadPolicy "doom-586").minimum_real_time_factor
            quake_586 = (Get-WorkloadPolicy "quake-586").minimum_real_time_factor
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
$trackMEvidence = $null
$directQuakeEvidence = $null
if ($DirectQuakeCampaign) {
    $directQuakeEvidence = Write-DirectQuakeCampaignEvidencePackage `
        $ResultsDirectory $summaryPath $summary $candidateArtifact $baselineArtifact `
        $gateSourceClosureAtCompletion $gateMainScriptPath $gateSelfTestScriptPath `
        $gateSummaryScriptPath $fixtureManifestPath
} elseif ($TrackMComparison) {
    $trackMEvidence = Write-TrackMEvidencePackage `
        $ResultsDirectory $summaryPath $summary $candidateArtifact $baselineArtifact `
        $gateSourceClosureAtCompletion $gateMainScriptPath $gateSelfTestScriptPath `
        $gateSummaryScriptPath $fixtureManifestPath
}

foreach ($workloadResult in $summary.workloads) {
    if ($PollSkipComparison) {
        $metric = $workloadResult.paired_metrics.real_time_factor
        Write-Host ("{0}: skip_on/skip_off RTF median={1:N4} geometric-mean={2:N4} lower95={3:N4} verdict={4}" -f `
            $workloadResult.name, $metric.median_ratio, $metric.geometric_mean_ratio, `
            $metric.lower_95_ratio, $workloadResult.verdicts.performance)
    } elseif ($BackendBakeoff) {
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
    if (-not $PollSkipComparison -and $null -ne $workloadResult.paired_metrics) {
        Write-Host ("  paired IPS={0:N3} (lower95={1:N3}) RTF={2:N3} (lower95={3:N3})" -f `
            $workloadResult.paired_metrics.instructions_per_host_second.median_ratio,
            $workloadResult.paired_metrics.instructions_per_host_second.lower_95_ratio,
            $workloadResult.paired_metrics.real_time_factor.median_ratio,
            $workloadResult.paired_metrics.real_time_factor.lower_95_ratio)
    }
}
Write-Host "Summary: $summaryPath"
if ($DirectQuakeCampaign) {
    Write-Host "Evidence manifest: $($directQuakeEvidence.manifest_path)"
    Write-Host "Evidence manifest SHA-256: $($directQuakeEvidence.manifest_sha256)"
    Write-Host "Result log: $($directQuakeEvidence.result_log_path)"
} elseif ($TrackMComparison) {
    Write-Host "Evidence manifest: $($trackMEvidence.manifest_path)"
    Write-Host "Evidence manifest SHA-256: $($trackMEvidence.manifest_sha256)"
    Write-Host "Result log: $($trackMEvidence.result_log_path)"
}

if ($PollSkipComparison -and $summary.verdict -cne "improved") {
    throw "The POLL-SKIP comparison did not demonstrate a speedup: $($summary.verdict)."
} elseif ($DirectQuakeCampaign -and -not $summary.evidence_valid) {
    throw "The Direct Quake campaign evidence is invalid: $($summary.failure_reasons -join ' | ')."
} elseif ($DirectQuakeCampaign -and $CampaignStage -ceq "Screen" -and
    $summary.verdict -cne "screen_positive") {
    throw "The Direct Quake campaign screen did not reach its 2% median triage threshold."
} elseif ($DirectQuakeCampaign -and $CampaignStage -ceq "Proof" -and
    $summary.verdict -notin @(
        "normal_promotion_threshold_met",
        "narrow_requires_mechanism_evidence",
        "twelve_pair_extension_eligible"
    )) {
    throw "The Direct Quake campaign proof did not reach a provisional promotion class."
} elseif ($TrackMComparison -and $summary.verdict -ne "passed") {
    throw "The Track M comparison did not pass: $($summary.failure_reasons -join ' | ')."
} elseif ($BackendBakeoff -and -not $Screening -and $summary.track_a_survival -ne "pass") {
    throw "The backend bakeoff did not meet its survival gate: $($summary.failure_reasons -join ' | ')."
}
if (-not $PollSkipComparison -and -not $TrackMComparison -and -not $BackendBakeoff -and
    -not $DirectQuakeCampaign -and
    -not $ReportOnly -and $verdict -ne "passed") {
    throw "The paired throughput gate did not pass: $($formalFailures -join ' | ')."
}
} finally {
    if ($null -ne $measurementLockLease) {
        $measurementLockLease.handle.Dispose()
    }
}
