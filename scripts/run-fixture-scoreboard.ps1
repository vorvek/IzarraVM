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

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -SelfTest
#>

param(
    [string]$Executable = "target/release/izarravm.exe",
    [string[]]$Fixtures = @(),
    [string]$Label = "",
    [string]$ResultsDirectory = "",
    [int]$ProcessorIndex = -1,
    # Per fixture, not for the sweep. duke3d-586 alone is about half an hour of
    # wall since it has to play a DUKEMARK demo to completion, so the old 1800
    # would kill the run it is meant to protect.
    [int]$HostTimeoutSeconds = 3600,
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
    [switch]$ListFixtures,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$invariantPath = Join-Path $PSScriptRoot "fixture-scoreboard-invariants.json"
$scoreboardSchema = "izarravm-fixture-scoreboard-v2"

function Get-RequiredUInt64Property($InputObject, [string]$Name, [string]$Path) {
    if ($null -eq $InputObject) {
        throw "coverage accounting is missing $Path"
    }
    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "coverage accounting is missing $Path.$Name"
    }
    $value = $property.Value
    if ($null -eq $value) {
        throw "coverage accounting field $Path.$Name is null"
    }
    $integralTypes = @(
        [SByte], [Byte], [Int16], [UInt16], [Int32], [UInt32], [Int64], [UInt64],
        [Numerics.BigInteger]
    )
    if ($integralTypes -notcontains $value.GetType()) {
        throw "coverage accounting field $Path.$Name is not an integer"
    }
    $wide = [Numerics.BigInteger]$value
    if ($wide -lt [Numerics.BigInteger]::Zero -or
        $wide -gt [Numerics.BigInteger][UInt64]::MaxValue) {
        throw "coverage accounting field $Path.$Name is outside the UInt64 range"
    }
    return [UInt64]$wide
}

function Get-CoverageMetrics($Profile) {
    if ($null -eq $Profile) {
        throw "coverage accounting is missing the profile"
    }
    $perfProperty = $Profile.PSObject.Properties["perf"]
    $stallsProperty = $Profile.PSObject.Properties["direct_stalls"]
    if ($null -eq $perfProperty) {
        throw "coverage accounting is missing perf"
    }
    if ($null -eq $stallsProperty) {
        throw "coverage accounting is missing direct_stalls"
    }

    [UInt64]$total = Get-RequiredUInt64Property $perfProperty.Value "instructions" "perf"
    [UInt64]$direct = Get-RequiredUInt64Property `
        $perfProperty.Value "jit_direct_insns" "perf"
    [UInt64]$entries = Get-RequiredUInt64Property `
        $perfProperty.Value "jit_direct_entries" "perf"
    [UInt64]$entries16 = Get-RequiredUInt64Property `
        $perfProperty.Value "jit_direct_entries_sixteen_bit" "perf"
    [UInt64]$insns16 = Get-RequiredUInt64Property `
        $perfProperty.Value "jit_direct_insns_sixteen_bit" "perf"
    [UInt64]$attempts = Get-RequiredUInt64Property `
        $stallsProperty.Value "jit_direct_callout_executed" "direct_stalls"
    [UInt64]$abnormal = Get-RequiredUInt64Property `
        $stallsProperty.Value "side_exit_callout_abnormal" "direct_stalls"

    if ($abnormal -gt $attempts) {
        throw "coverage accounting has callout abnormal $abnormal above attempts $attempts"
    }
    [UInt64]$helper = $attempts - $abnormal
    if ($helper -gt $direct) {
        throw "coverage accounting has helper instructions $helper above direct instructions $direct"
    }
    if ($direct -gt $total) {
        throw "coverage accounting has direct instructions $direct above total instructions $total"
    }
    if ($direct -gt 0 -and $entries -eq 0) {
        throw "coverage accounting has $direct direct instructions with zero direct entries"
    }
    if ($entries16 -gt $entries) {
        throw "coverage accounting has 16-bit entries $entries16 above direct entries $entries"
    }
    if ($insns16 -gt $direct) {
        throw "coverage accounting has 16-bit instructions $insns16 above direct instructions $direct"
    }
    if ($insns16 -gt 0 -and $entries16 -eq 0) {
        throw "coverage accounting has $insns16 16-bit instructions with zero 16-bit entries"
    }
    if ($total -eq 0 -and
        ($direct -ne 0 -or $entries -ne 0 -or $entries16 -ne 0 -or $insns16 -ne 0 -or
            $attempts -ne 0 -or $abnormal -ne 0)) {
        throw "coverage accounting has zero total instructions with nonzero component counters"
    }

    [UInt64]$emitted = $direct - $helper
    [UInt64]$interpreted = $total - $direct
    $conserved = [Numerics.BigInteger]$emitted +
        [Numerics.BigInteger]$helper +
        [Numerics.BigInteger]$interpreted
    if ($conserved -ne [Numerics.BigInteger]$total) {
        throw "coverage accounting categories do not conserve total instructions"
    }

    $directCoverage = if ($total -eq 0) { 0.0 } else { [double]$direct / [double]$total }
    $emittedCoverage = if ($total -eq 0) { 0.0 } else { [double]$emitted / [double]$total }
    $helperCoverage = if ($total -eq 0) { 0.0 } else { [double]$helper / [double]$total }
    $interpretedCoverage = if ($total -eq 0) {
        0.0
    } else {
        [double]$interpreted / [double]$total
    }
    $directIpe = if ($entries -eq 0) { 0.0 } else { [double]$direct / [double]$entries }
    $emittedIpe = if ($entries -eq 0) { 0.0 } else { [double]$emitted / [double]$entries }
    $helperIpe = if ($entries -eq 0) { 0.0 } else { [double]$helper / [double]$entries }
    $direct16Ipe = if ($entries16 -eq 0) { 0.0 } else {
        [double]$insns16 / [double]$entries16
    }

    return [pscustomobject][ordered]@{
        total_insns                  = $total
        direct_insns                 = $direct
        emitted_insns                = $emitted
        helper_insns                 = $helper
        interpreted_insns            = $interpreted
        callout_attempts              = $attempts
        callout_abnormal              = $abnormal
        entries                       = $entries
        entries_16bit                 = $entries16
        insns_16bit                   = $insns16
        direct_coverage               = $directCoverage
        emitted_coverage              = $emittedCoverage
        helper_coverage               = $helperCoverage
        interpreted_coverage          = $interpretedCoverage
        direct_insns_per_entry        = $directIpe
        emitted_insns_per_entry       = $emittedIpe
        helper_insns_per_entry        = $helperIpe
        direct_insns_per_entry_16bit  = $direct16Ipe
    }
}

function Add-CoverageMetrics($Result, $Profile) {
    $coverage = Get-CoverageMetrics $Profile

    $Result.instructions = $coverage.total_insns
    $Result.entries = $coverage.entries
    $Result.direct_insns = $coverage.direct_insns
    $Result.emitted_insns = $coverage.emitted_insns
    $Result.helper_insns = $coverage.helper_insns
    $Result.interpreted_insns = $coverage.interpreted_insns
    $Result.callout_attempts = $coverage.callout_attempts
    $Result.callout_abnormal = $coverage.callout_abnormal
    $Result.direct_coverage = [math]::Round($coverage.direct_coverage, 6)
    $Result.emitted_coverage = [math]::Round($coverage.emitted_coverage, 6)
    $Result.helper_coverage = [math]::Round($coverage.helper_coverage, 6)
    $Result.interpreted_coverage = [math]::Round($coverage.interpreted_coverage, 6)
    $Result.direct_insns_per_entry = [math]::Round($coverage.direct_insns_per_entry, 3)
    $Result.emitted_insns_per_entry = [math]::Round($coverage.emitted_insns_per_entry, 3)
    $Result.helper_insns_per_entry = [math]::Round($coverage.helper_insns_per_entry, 3)
    $Result.entries_16bit = $coverage.entries_16bit
    $Result.insns_16bit = $coverage.insns_16bit
    $Result.insns_per_entry_16bit = [math]::Round(
        $coverage.direct_insns_per_entry_16bit,
        3
    )

    # Compatibility fields keep their v1 names and rounding.
    $Result.native_insns = $coverage.direct_insns
    $Result.native_coverage = [math]::Round($coverage.direct_coverage, 4)
    $Result.insns_per_entry = $Result.direct_insns_per_entry
    return $coverage
}

function Format-ScoreboardPercent([double]$Value) {
    return [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        "{0:F2}%",
        $Value * 100.0
    )
}

function Format-ScoreboardDecimal([double]$Value) {
    return [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        "{0:F3}",
        $Value
    )
}

function Get-ScoreboardMarkdown($Rows, [string]$BoardLabel, [string]$BoardArm,
    [string]$StoreArm, [string]$LoadArm) {
    $markdown = @()
    $markdown += "# Fixture scoreboard$(if ($BoardLabel) { ": $BoardLabel" })"
    $markdown += ""
    $markdown += "Recorded $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')), JIT arm ``$BoardArm``, one-lookup store ``$StoreArm``, one-lookup load ``$LoadArm``. rt is guest seconds per wall second; 1.0 is real time."
    $markdown += "Direct insns/entry includes emitted instructions and successful helper instructions. Abnormal helper attempts replay in the interpreter and do not count as helper-retired instructions."
    $markdown += ""
    $markdown += "| fixture | rt | wall s | emitted | helper | interpreter | entries | direct insns/entry | emitted insns/entry | 16-bit direct insns/entry | invariant |"
    $markdown += "|---|---|---|---|---|---|---|---|---|---|---|"
    foreach ($row in $Rows) {
        $has = { param($n) $row.PSObject.Properties.Name -contains $n }
        $markdown += ("| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7} | {8} | {9} | {10}{11} |" -f
            $row.name,
            $(if (& $has "real_time_factor") { $row.real_time_factor } else { "-" }),
            $(if (& $has "wall_seconds") { $row.wall_seconds } else { "-" }),
            $(if (& $has "emitted_coverage") {
                    Format-ScoreboardPercent $row.emitted_coverage
                } else { "-" }),
            $(if (& $has "helper_coverage") {
                    Format-ScoreboardPercent $row.helper_coverage
                } else { "-" }),
            $(if (& $has "interpreted_coverage") {
                    Format-ScoreboardPercent $row.interpreted_coverage
                } else { "-" }),
            $(if (& $has "entries") { $row.entries } else { "-" }),
            $(if (& $has "direct_insns_per_entry") {
                    Format-ScoreboardDecimal $row.direct_insns_per_entry
                } else { "-" }),
            $(if (& $has "emitted_insns_per_entry") {
                    Format-ScoreboardDecimal $row.emitted_insns_per_entry
                } else { "-" }),
            $(if (& $has "insns_per_entry_16bit") {
                    Format-ScoreboardDecimal $row.insns_per_entry_16bit
                } else { "-" }),
            $row.invariant,
            $(if ($row.contaminated) { " (contaminated)" } else { "" }))
    }
    $markdown += ""
    foreach ($row in $Rows) {
        if ($row.notes.Count -gt 0) {
            $markdown += "* **$($row.name)**: $($row.notes -join '; ')"
        }
    }
    return $markdown
}

function Assert-ScoreboardSelfTestEqual($Actual, $Expected, [string]$Message) {
    if ($Actual -ne $Expected) {
        throw "scoreboard self-test failed: $Message (expected $Expected, got $Actual)"
    }
}

function Assert-ScoreboardSelfTestThrows([scriptblock]$Action, [string]$Expected,
    [string]$Message) {
    $failure = $null
    try {
        $null = & $Action
    } catch {
        $failure = $_.Exception.Message
    }
    if ($null -eq $failure) {
        throw "scoreboard self-test failed: $Message did not throw"
    }
    if (-not $failure.Contains($Expected, [StringComparison]::Ordinal)) {
        throw "scoreboard self-test failed: $Message threw '$failure', expected '$Expected'"
    }
}

function New-CoverageSelfTestProfile([UInt64]$Total, [UInt64]$Direct,
    [UInt64]$Entries, [UInt64]$Attempts, [UInt64]$Abnormal,
    [UInt64]$StepBreak = 0, [UInt64]$Entries16 = 0, [UInt64]$Insns16 = 0) {
    return [pscustomobject][ordered]@{
        direct_native_coverage = if ($Total -eq 0) { 0.0 } else {
            [double]$Direct / [double]$Total
        }
        perf = [pscustomobject][ordered]@{
            instructions       = $Total
            jit_direct_insns   = $Direct
            jit_direct_entries = $Entries
            jit_direct_entries_sixteen_bit = $Entries16
            jit_direct_insns_sixteen_bit = $Insns16
        }
        direct_stalls = [pscustomobject][ordered]@{
            jit_direct_callout_executed = $Attempts
            side_exit_callout_abnormal  = $Abnormal
            side_exit_callout_step_break = $StepBreak
        }
    }
}

function Invoke-ScoreboardSelfTest {
    $noJit = Get-CoverageMetrics (New-CoverageSelfTestProfile 100 0 0 0 0)
    Assert-ScoreboardSelfTestEqual $noJit.interpreted_insns 100 "no-JIT instructions"
    Assert-ScoreboardSelfTestEqual $noJit.emitted_coverage 0.0 "no-JIT emitted coverage"
    Assert-ScoreboardSelfTestEqual $noJit.helper_coverage 0.0 "no-JIT helper coverage"
    Assert-ScoreboardSelfTestEqual $noJit.interpreted_coverage 1.0 "no-JIT coverage"

    $allEmitted = Get-CoverageMetrics (New-CoverageSelfTestProfile 100 100 10 0 0)
    Assert-ScoreboardSelfTestEqual $allEmitted.emitted_insns 100 "all-emitted instructions"
    Assert-ScoreboardSelfTestEqual $allEmitted.direct_insns_per_entry 10.0 "all-emitted IPE"

    $mixedProfile = New-CoverageSelfTestProfile 100 80 10 22 2
    $mixed = Get-CoverageMetrics $mixedProfile
    Assert-ScoreboardSelfTestEqual $mixed.emitted_insns 60 "mixed emitted instructions"
    Assert-ScoreboardSelfTestEqual $mixed.helper_insns 20 "mixed helper instructions"
    Assert-ScoreboardSelfTestEqual $mixed.interpreted_insns 20 "mixed interpreted instructions"
    Assert-ScoreboardSelfTestEqual $mixed.direct_coverage 0.8 "mixed direct coverage"
    Assert-ScoreboardSelfTestEqual $mixed.emitted_coverage 0.6 "mixed emitted coverage"
    Assert-ScoreboardSelfTestEqual $mixed.helper_coverage 0.2 "mixed helper coverage"
    Assert-ScoreboardSelfTestEqual $mixed.interpreted_coverage 0.2 "mixed interpreted coverage"
    Assert-ScoreboardSelfTestEqual $mixed.direct_insns_per_entry 8.0 "mixed direct IPE"
    Assert-ScoreboardSelfTestEqual $mixed.emitted_insns_per_entry 6.0 "mixed emitted IPE"
    Assert-ScoreboardSelfTestEqual $mixed.helper_insns_per_entry 2.0 "mixed helper IPE"

    $stepBreak = Get-CoverageMetrics (New-CoverageSelfTestProfile 1 1 1 1 0 1)
    Assert-ScoreboardSelfTestEqual $stepBreak.helper_insns 1 "step-break helper retirement"

    $zero = Get-CoverageMetrics (New-CoverageSelfTestProfile 0 0 0 0 0)
    Assert-ScoreboardSelfTestEqual $zero.direct_coverage 0.0 "zero direct coverage"
    Assert-ScoreboardSelfTestEqual $zero.emitted_coverage 0.0 "zero emitted coverage"
    Assert-ScoreboardSelfTestEqual $zero.helper_coverage 0.0 "zero helper coverage"
    Assert-ScoreboardSelfTestEqual $zero.interpreted_coverage 0.0 "zero interpreted coverage"
    Assert-ScoreboardSelfTestEqual $zero.direct_insns_per_entry 0.0 "zero direct IPE"
    Assert-ScoreboardSelfTestEqual $zero.emitted_insns_per_entry 0.0 "zero emitted IPE"
    Assert-ScoreboardSelfTestEqual $zero.helper_insns_per_entry 0.0 "zero helper IPE"

    $largeProfile = New-CoverageSelfTestProfile `
        9007199254741001 9007199254740993 3 5 2
    $largeProfile = $largeProfile | ConvertTo-Json -Depth 4 | ConvertFrom-Json
    $large = Get-CoverageMetrics $largeProfile
    Assert-ScoreboardSelfTestEqual $large.direct_insns 9007199254740993 `
        "large direct count"
    Assert-ScoreboardSelfTestEqual $large.helper_insns 3 "large helper count"
    Assert-ScoreboardSelfTestEqual $large.emitted_insns 9007199254740990 `
        "large emitted count"
    Assert-ScoreboardSelfTestEqual $large.interpreted_insns 8 "large interpreted count"

    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 80 10 1 2)
    } "abnormal 2 above attempts 1" "abnormal above attempts"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 10 10 11 0)
    } "helper instructions 11 above direct instructions 10" "helper above direct"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 101 10 0 0)
    } "direct instructions 101 above total instructions 100" "direct above total"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 1 0 0 0)
    } "direct instructions with zero direct entries" "direct instructions with zero entries"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 0 0 0 1 1)
    } "zero total instructions with nonzero component counters" `
        "zero total with nonzero counters"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 80 11 0 0 0 12 70)
    } "16-bit entries 12 above direct entries 11" "16-bit entries above direct entries"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 80 11 0 0 0 10 81)
    } "16-bit instructions 81 above direct instructions 80" `
        "16-bit instructions above direct instructions"
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics (New-CoverageSelfTestProfile 100 80 11 0 0 0 0 1)
    } "16-bit instructions with zero 16-bit entries" `
        "16-bit instructions with zero entries"

    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics $null
    } "missing the profile" "missing profile"
    $missingPerf = New-CoverageSelfTestProfile 100 80 10 20 0
    $missingPerf.PSObject.Properties.Remove("perf")
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics $missingPerf
    } "missing perf" "missing perf block"
    $missingStalls = New-CoverageSelfTestProfile 100 80 10 20 0
    $missingStalls.PSObject.Properties.Remove("direct_stalls")
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics $missingStalls
    } "missing direct_stalls" "missing direct-stalls block"

    foreach ($missingName in @(
            "instructions",
            "jit_direct_insns",
            "jit_direct_entries",
            "jit_direct_entries_sixteen_bit",
            "jit_direct_insns_sixteen_bit"
        )) {
        $missingPerfField = New-CoverageSelfTestProfile 100 80 10 20 0
        $missingPerfField.perf.PSObject.Properties.Remove($missingName)
        Assert-ScoreboardSelfTestThrows {
            Get-CoverageMetrics $missingPerfField
        } "missing perf.$missingName" "missing perf field $missingName"
    }

    $missingAttempts = New-CoverageSelfTestProfile 100 80 10 20 0
    $missingAttempts.direct_stalls.PSObject.Properties.Remove("jit_direct_callout_executed")
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics $missingAttempts
    } "missing direct_stalls.jit_direct_callout_executed" "missing callout attempts"
    $missingAbnormal = New-CoverageSelfTestProfile 100 80 10 20 0
    $missingAbnormal.direct_stalls.PSObject.Properties.Remove("side_exit_callout_abnormal")
    Assert-ScoreboardSelfTestThrows {
        Get-CoverageMetrics $missingAbnormal
    } "missing direct_stalls.side_exit_callout_abnormal" "missing abnormal callouts"

    $invalidValues = @(
        [pscustomobject]@{ value = 1.5; expected = "is not an integer"; name = "fractional" },
        [pscustomobject]@{ value = -1; expected = "outside the UInt64 range"; name = "negative" },
        [pscustomobject]@{ value = $null; expected = "is null"; name = "null" },
        [pscustomobject]@{ value = "100"; expected = "is not an integer"; name = "string" },
        [pscustomobject]@{
            value = [Numerics.BigInteger][UInt64]::MaxValue + [Numerics.BigInteger]::One
            expected = "outside the UInt64 range"
            name = "oversized"
        }
    )
    foreach ($invalid in $invalidValues) {
        $invalidProfile = New-CoverageSelfTestProfile 100 80 10 20 0
        $invalidProfile.perf.instructions = $invalid.value
        Assert-ScoreboardSelfTestThrows {
            Get-CoverageMetrics $invalidProfile
        } $invalid.expected "invalid $($invalid.name) counter"
    }

    $row = [ordered]@{
        name = "mixed"
        real_time_factor = 1.0
        wall_seconds = 1.0
        insns_per_entry_16bit = 0.0
        invariant = "pass"
        contaminated = $false
        notes = @()
    }
    $null = Add-CoverageMetrics $row $mixedProfile
    $json = [ordered]@{
        schema = $scoreboardSchema
        rows = @([pscustomobject]$row)
    } | ConvertTo-Json -Depth 4 | ConvertFrom-Json
    Assert-ScoreboardSelfTestEqual $json.schema $scoreboardSchema "schema"
    Assert-ScoreboardSelfTestEqual $json.rows[0].native_insns 80 "legacy native_insns alias"
    Assert-ScoreboardSelfTestEqual $json.rows[0].native_coverage 0.8 "legacy native_coverage alias"
    Assert-ScoreboardSelfTestEqual $json.rows[0].insns_per_entry 8.0 "legacy IPE alias"

    $markdown = @(Get-ScoreboardMarkdown @([pscustomobject]$row) "self-test" "on" "1" "1")
    $rendered = $markdown -join "`n"
    if ($rendered -notmatch '\| emitted \| helper \| interpreter \|' -or
        $rendered -notmatch 'Direct insns/entry includes emitted instructions' -or
        $rendered -notmatch 'Abnormal helper attempts replay in the interpreter') {
        throw "scoreboard self-test failed: Markdown coverage headers or IPE definition are missing"
    }
    Write-Host "fixture scoreboard self-test passed"
}

if ($SelfTest) {
    Invoke-ScoreboardSelfTest
    return
}

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
# DUKEMARK pins. DUKEMARK.EXE is a modified Duke Nukem 3D Atomic build that
# plays a canned demo, samples FPS about four times a second, then exits to DOS
# and prints a report.
#
# The whole run is GUEST-DRIVEN, which is the point of this shape:
#
#     @echo off
#     cd \DUKE3D
#     DUKEMARK.EXE /bqBENCH2 > C:\DUKEMARK.TXT
#     C:\EXITVM.COM
#
# DOS redirection captures the report into a file on the mounted host folder,
# and EXITVM.COM (the house 15-byte Lotura unit-tester exit poke, the same one
# the Doom and bench16 fixtures carry) ends the VM. So the cycle budget is a
# GUARD, not the thing that ends the run: the demo finishes when it finishes.
#
# The invariants, in descending order of how much they are worth:
#
#   exitCode  the run stopped as `test_exit` with EXITVM's code. The game
#             returned to DOS on its own and the batch reached its last line.
#             Completely insensitive to timing, and it is what replaced the
#             cutoff-phase framebuffer hash.
#   resultFile the redirected report exists and parses. It also guards the one
#             real risk in this design: DUKEMARK's report goes through DOS
#             stdout today (verified -- the text page is blank on a redirected
#             run and the file holds the whole report), and if that ever became
#             direct-video output the file would be empty rather than wrong.
#   info      the Info String, a config fingerprint of
#             Demo,Width,Height,Mode,Hud,Detail,Sound,Music read straight out of
#             DUKE3D.CFG. Also timing-insensitive. `1,1` at the tail is sound and
#             music both ENABLED, so an audio regression that silences the game
#             cannot quietly present itself as a speedup. The first field does
#             NOT identify the demo -- it reads 2 for BENCH1, BENCH2 and BENCH3
#             alike (measured) -- so the sample count is the only field that does.
#   samples   the extrapolation count, DUKEMARK's own stall detector, held to a
#             TOLERANCE rather than an exact value. Its docs call the count
#             constant per demo across machines, and it is not: BENCH2 reads 919
#             at the 486 persona and 1026 at the 586, reproducibly. It is
#             therefore a function of emulated timing, and pinning it exactly
#             would rebuild the re-pin treadmill this fixture was rewritten to
#             escape. The band absorbs ordinary timing-model drift and is far
#             tighter than the "stalls very hard" case it exists to catch: a
#             multi-second stall inside a ~131 s demo moves it several percent.
#             Within one build the count is EXACT: two 486 runs twenty minutes
#             apart, on a host busy enough that their WALL times differed by 38%,
#             agreed to the digit on 919 samples and on every guest-side counter
#             in the profile. The band absorbs model drift between builds, not
#             run-to-run noise, and a count that varies WITHIN a build is a
#             determinism bug rather than drift.
#
#             THE BAND IS SIZED AGAINST A MEASUREMENT (2026-08-10). Under the
#             largest lever this harness has, `-Arm off` -- both JIT halves off,
#             duke3d-486 coverage 0.7235 -> 0.5932, wall 141.1 s -> 155.2 s --
#             the count moved from 919 to 920, one count against an allowance
#             of 18. It survives because arm off moves GUEST time by three parts
#             in ten thousand (163.150 -> 163.103 s): charging is per
#             instruction and does not care which backend retired it. So +/-2%
#             covers JIT-mix work with a factor of 18 in hand and deliberately
#             does NOT cover timing-model work -- the same day's storage-charge
#             slices moved this count 580 -> 919 -- which is the class of change
#             that SHOULD reach a reviewer as a pin move. See .bench/PROTOCOL.md.
#
#             The count and its band live in the SIDECAR JSON beside the frame
#             hashes, not here, and go through the same -RecordInvariants /
#             -Force machinery: a pin that moves is a reviewable one-line diff
#             with the manifest sha moved in the same breath, which is exactly
#             the argument the sidecar comment below makes for the hashes. The
#             constants below are only what a FIRST record starts from.
#
# FPS min/max/avg are MEASUREMENTS. They are guest-observed frame rates and move
# with host load, so they are reported and never asserted.
$dukemarkSampleTolerance = 0.02
function New-DukemarkPins {
    @{
        demo       = "BENCH2"
        info       = "2,320,200,2,0,1,1,1"
        resultFile = "DUKEMARK.TXT"
        # EXITVM.COM poking 0x51 at the unit-tester exit register, not zero.
        exitCode   = 0x51
    }
}

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
            # Shifted down 86 tics on 2026-08-10 with the storage-charge changes,
            # keeping the band's width and its margins around the measurement.
            # Doom READS FROM DISK DURING THE TIMEDEMO -- charged I/O stall over
            # this budget fell from 0.996 to 0.171 guest seconds -- so the demo
            # completes in fewer tics while gametics stays 2134, which is what
            # says the demo itself is unchanged.
            realticsMinimum = 2814; realticsMaximum = 2964; gametics = 2134
            qconsole = $false; resultPpm = $false; injection = @(); dukemark = $null
        }
        [pscustomobject]@{
            name = "doom-586"; folder = "jemmex_doom_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]6640000000
            # Shifted down 19 tics on 2026-08-10 for the same reason as the 486
            # row, band width and margins preserved.
            realticsMinimum = 951; realticsMaximum = 1021; gametics = 2134
            qconsole = $false; resultPpm = $false; injection = @(); dukemark = $null
        }
        [pscustomobject]@{
            name = "quake-586"; folder = "quake_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]6200000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            # QCONSOLE.LOG is the invariant. perf.instructions is NOT one: the
            # demo finishes before the budget and the run stops in an idle tail
            # whose length moves with the timing model.
            qconsole = $true; resultPpm = $false; injection = @(); dukemark = $null
        }
        [pscustomobject]@{
            name = "prince-486"; folder = "prince_c"
            # 486 for cost, not compatibility. A 1989 game does not need 166 MHz,
            # and at 66 MHz the same guest time costs a third of the cycles.
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]4000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            # Six Shifts to reach level 1, then right HELD so he runs instead of
            # standing. A bare {right} is a tap and leaves him standing.
            injection = @("--inject-keys", ("400000000:{shift};600000000:{shift};" +
                "800000000:{shift};1000000000:{shift};1200000000:{shift};" +
                "1400000000:{shift};1600000000:{+right}"))
        }
        [pscustomobject]@{
            name = "wolf3d-486"; folder = "wolf3d_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]8000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            # One Enter at the signon's "Press a key" so the title/credits/demo
            # rotation runs. Without it (and without the memory manager the
            # fixture's CONFIG.SYS was missing until 2026-08-08) every earlier
            # wolf3d number measured an out-of-memory CRASH LOOP, not the game.
            injection = @("--inject-keys", "2000000000:
")
        }
        [pscustomobject]@{
            name = "wolf3d-586"; folder = "wolf3d_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # 12e9 (72 guest seconds) so the end frame lands INSIDE demo
            # playback, past the ~35 guest seconds of startup plus rotation.
            cycles = [uint64]12000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            # See wolf3d-486: the Enter is what gets the game past its signon.
            injection = @("--inject-keys", "2000000000:
")
        }
        [pscustomobject]@{
            name = "duke3d-486"; folder = "duke3d_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            # A GUARD, not the length of the run: the guest exits itself through
            # EXITVM once the demo is done, which lands at about 10.8e9 (163
            # guest seconds) since the HDD-geometry slice of 2026-08-10 took the
            # FAT-chain walking out of the load phase, and landed at 19.4e9 (294
            # guest seconds) before it. 26.4e9 is 400 guest seconds, so a run
            # that hits the budget has genuinely failed to finish and says so.
            cycles = [uint64]26400000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $false; injection = @()
            dukemark = (New-DukemarkPins)
        }
        [pscustomobject]@{
            name = "duke3d-586"; folder = "duke3d_c"
            # The most expensive fixture in the set, and the one furthest below
            # real time, which is why it is the workload the campaign's merge
            # rule protects.
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # Same guard role as the 486 row. 79.68e9 is 480 guest seconds at
            # 166 MHz, comfortably past where EXITVM actually fires (about
            # 23.2e9, 140 guest seconds).
            cycles = [uint64]79680000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $false; injection = @()
            # The sample pin is per persona (the count follows emulated timing,
            # so the 586 row does not read the 486 row's number); both live in
            # the sidecar json.
            dukemark = (New-DukemarkPins)
        }
        [pscustomobject]@{
            name = "nascar-586"; folder = "nascar1_c"
            # No --video: PROTOCOL.md's recorded invocation omits it and the
            # invariant hash was measured that way.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]4980000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @(); dukemark = $null
        }
        [pscustomobject]@{
            name = "gp2-586"; folder = "gp2_c"
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]13280000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
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

<#
Read DUKEMARK's result out of the file the guest redirected it into.

The fixture's AUTOEXEC runs `DUKEMARK.EXE /bqBENCH2 > C:\DUKEMARK.TXT` and then
`C:\EXITVM.COM`, so the whole run is guest-driven: the demo plays, DOS captures
the report through ordinary stdout redirection, and the guest ends the VM itself
through the Lotura unit-tester exit port. Katea holds guest writes until
`flush_hdd_folder()`, which the run's normal end-of-run path performs whatever
the stop reason was, so the file is on the host by the time this reads it.

VERIFIED, because it was the design's one real risk: DUKEMARK's final report DOES
go through DOS stdout and lands in the file intact. Redirection would have caught
nothing if the Build engine had painted that screen directly, and the check is
cheap to repeat -- the text page is BLANK on a redirected run, so a regression to
direct-video output shows up as an empty file rather than as silently wrong
numbers.

The tail of the file it is looking at is:

     DukeMark by DXZeff

     Info         : 2,320,200,2,0,1,1,1
     FPS Minimum  : 11
     FPS Maximum  : 50
     FPS Average  : 31
     Extrapolated : 919 Samples

Returns `found = $false` when the file is missing entirely, which is a different
failure (the redirection or the flush broke) from a present file with no Info
line (the game never reached its own exit path).
#>
function Read-DukemarkResult([string]$ResultPath) {
    $scraped = @{
        found = $false; info = $null; samples = $null
        fps_min = $null; fps_max = $null; fps_avg = $null
        report = $null
    }
    if (-not (Test-Path -LiteralPath $ResultPath)) { return $scraped }
    $lines = @(Get-Content -LiteralPath $ResultPath)
    $scraped.found = $true
    # Only the tail is worth keeping as evidence: everything before it is the
    # engine's start-up chatter, which is not what this fixture measures.
    $scraped.report = (@($lines | Where-Object { $_.Trim().Length -gt 0 }) |
        Select-Object -Last 6) -join "`n"
    foreach ($line in $lines) {
        $trimmed = $line.TrimEnd()
        if ($trimmed -match '^\s*Info\s*:\s*(\S+)\s*$') { $scraped.info = $Matches[1] }
        elseif ($trimmed -match '^\s*FPS Minimum\s*:\s*(\d+)\s*$') { $scraped.fps_min = [int]$Matches[1] }
        elseif ($trimmed -match '^\s*FPS Maximum\s*:\s*(\d+)\s*$') { $scraped.fps_max = [int]$Matches[1] }
        elseif ($trimmed -match '^\s*FPS Average\s*:\s*(\d+)\s*$') { $scraped.fps_avg = [int]$Matches[1] }
        elseif ($trimmed -match '^\s*Extrapolated\s*:\s*(\d+)\s+Samples\s*$') {
            $scraped.samples = [int]$Matches[1]
        }
    }
    return $scraped
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

# Write text with LF endings and no BOM, which is what .gitattributes normalises
# these two files to on the way into a commit.
#
# CORRECTION to the claim made when this helper landed: it said the CRLF bug WAS
# the cause of the three red mains the comment below counts. It was not. Those
# three commits carried the PREVIOUS commit's (LF) sha in the manifest row --
# the manifest was simply not updated at all, which is the omission the
# auto-sync below now closes. The CRLF defect is real and is fixed here, but it
# was LATENT: `Set-Content -Encoding utf8` writes CRLF on Windows, so the sha
# this script recorded would have been the sha of a CRLF file that git then
# stored as LF, and no amount of keeping the two writes in step would have
# helped, because the mismatch happened AFTER both of them. Two distinct
# defects; only one of them had fired.
function Write-TextLf([string]$Path, [string]$Text) {
    $normalised = $Text -replace "`r`n", "`n"
    if (-not $normalised.EndsWith("`n")) { $normalised += "`n" }
    [IO.File]::WriteAllText($Path, $normalised, (New-Object Text.UTF8Encoding $false))
}

function Write-Invariants($Table) {
    $json = $Table.GetEnumerator() |
        Sort-Object Key |
        ForEach-Object -Begin { $ordered = [ordered]@{} } `
            -Process { $ordered[$_.Key] = $_.Value } `
            -End { $ordered } |
        ConvertTo-Json -Depth 6
    Write-TextLf $invariantPath $json

    # The invariants json is LICENSE_MANIFEST-covered, and a re-record without the
    # matching manifest sha has turned main red THREE times now (the file-policy
    # gate compares content hashes). Update the manifest row in the same breath so
    # the two files can never be committed out of step by this script's doing.
    $manifestPath = Join-Path $repositoryRoot "LICENSE_MANIFEST.tsv"
    if (Test-Path -LiteralPath $manifestPath) {
        $newSha = Get-FileSha256 $invariantPath
        $rows = Get-Content -LiteralPath $manifestPath
        $updated = $false
        for ($i = 0; $i -lt $rows.Count; $i++) {
            $cells = $rows[$i] -split "`t"
            if ($cells.Count -ge 5 -and $cells[0] -eq "scripts/fixture-scoreboard-invariants.json") {
                if ($cells[4] -ne $newSha) {
                    $cells[4] = $newSha
                    $rows[$i] = $cells -join "`t"
                    $updated = $true
                }
                break
            }
        }
        if ($updated) {
            Write-TextLf $manifestPath ($rows -join "`n")
            Write-Host "updated LICENSE_MANIFEST.tsv sha for the invariants json"
        }
    }
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

    # Same hazard for DUKEMARK's redirected report: if a copy ever ends up in the
    # source fixture, a run that produced nothing would be graded on it.
    $dukemarkResultPath = $null
    if ($null -ne $Fixture.dukemark) {
        $dukemarkResultPath = Join-Path $workingCopy $Fixture.dukemark.resultFile
        if (Test-Path -LiteralPath $dukemarkResultPath) {
            Remove-Item -LiteralPath $dukemarkResultPath -Force
        }
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

    $stdoutPath = Join-Path $ScratchRoot "$($Fixture.name)-$stamp.out"
    $start = @{
        FilePath               = $ExecutablePath
        ArgumentList           = $arguments
        NoNewWindow            = $true
        PassThru               = $true
        RedirectStandardOutput = $stdoutPath
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

    # A run that wrote no profile, or wrote one that will not parse, is a run
    # that told us NOTHING -- an emulator that crashed on start looks exactly
    # like this. It used to report a third word, `no-profile`, which the exit
    # check at the bottom did not count, so a sweep whose fixtures all crashed
    # exited 0 and read as a clean sweep. It is a FAIL; the note is what says
    # which kind of fail it is.
    if (-not (Test-Path -LiteralPath $profilePath)) {
        $result.invariant = "FAIL"
        $result.notes += ("no profile JSON was written (the emulator crashed, or never " +
            "started); exit code $exitCode")
        Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
        return $result
    }

    $profile = $null
    try {
        $profile = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json
    } catch {
        $profile = $null
    }
    if ($null -eq $profile -or $null -eq $profile.PSObject.Properties["perf"]) {
        $result.invariant = "FAIL"
        $result.notes += ("the profile JSON did not parse or carries no perf block " +
            "(truncated by a crash mid-write?); exit code $exitCode")
        Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
        return $result
    }

    # Preserve the raw profile before validating the derived scoreboard fields. If a new or
    # inconsistent counter contract fails below, the row fails but the evidence remains available.
    if (-not [string]::IsNullOrWhiteSpace($KeepProfilesIn)) {
        Copy-Item -LiteralPath $profilePath `
            -Destination (Join-Path $KeepProfilesIn "$($Fixture.name).json") -Force
    }

    $result.real_time_factor = [math]::Round($profile.real_time_factor, 4)
    $result.guest_seconds = [math]::Round($profile.guest_seconds, 3)
    $result.wall_seconds = [math]::Round($profile.wall_seconds, 3)
    $coverageFailure = $null
    try {
        $null = Add-CoverageMetrics $result $profile
    } catch {
        $coverageFailure = $_.Exception.Message
    }
    $result.stop = $profile.stop

    if ($backgroundLoad -ge $maximumBackgroundLoadPercent) {
        $result.contaminated = $true
        $result.notes += ("background load median {0}% over {1} samples, peak {2}%, threshold {3}%" -f
            $backgroundLoad, $waited.samples.Count, $peakLoad, $maximumBackgroundLoadPercent)
    }

    # --- invariants -------------------------------------------------------
    $failures = @()
    if ($null -ne $coverageFailure) {
        $failures += $coverageFailure
    }

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

    # DUKEMARK. Four deterministic assertions and three reported measurements;
    # see New-DukemarkPins for why the split falls exactly there. There is no
    # framebuffer hash on this fixture at all any more: the old end-of-budget
    # frame was cutoff-phase sensitive and moved six times in three days for
    # entirely benign reasons, which is the whole reason this replaced it.
    if ($null -ne $Fixture.dukemark) {
        $pins = $Fixture.dukemark
        $result.dukemark_demo = $pins.demo

        # 1. The guest ended the VM itself. A cycle_limit stop means the budget
        #    ran out first, i.e. the run never got to C:\EXITVM.COM -- the budget
        #    on this fixture is a guard, not the thing that ends the run.
        $stopKind = $profile.stop.kind
        $result.stop_kind = $stopKind
        if ($stopKind -ne "test_exit") {
            $failures += ("the guest did not exit through EXITVM: stop was '$stopKind', " +
                "expected 'test_exit' (budget too small, or the game never returned to DOS)")
        } else {
            $stopCode = [int]$profile.stop.code
            $result.stop_code = $stopCode
            if ($stopCode -ne $pins.exitCode) {
                $failures += ("EXITVM reported exit code $stopCode, expected $($pins.exitCode)")
            }
        }

        # 2-4. The redirected report.
        $scraped = Read-DukemarkResult $dukemarkResultPath
        if (-not $scraped.found) {
            $failures += ("no $($pins.resultFile) was written: the redirection or the " +
                "host-folder flush failed")
        } else {
            $result.dukemark_info = $scraped.info
            $result.dukemark_samples = $scraped.samples
            # Measurements, never asserted.
            $result.fps_min = $scraped.fps_min
            $result.fps_max = $scraped.fps_max
            $result.fps_avg = $scraped.fps_avg

            if ($null -eq $scraped.info) {
                $failures += ("$($pins.resultFile) carries no Info String -- either the demo " +
                    "never reached its exit, or DUKEMARK stopped printing its report through " +
                    "DOS stdout and redirection no longer captures it")
            } elseif ($scraped.info -ne $pins.info) {
                $failures += ("DUKEMARK Info String is '$($scraped.info)', expected " +
                    "'$($pins.info)' -- the fixture's configuration moved " +
                    "(Demo,Width,Height,Mode,Hud,Detail,Sound,Music)")
            }
            # The count itself is graded in the driver against the sidecar pin,
            # the same way the frame hashes are. Its ABSENCE is graded here,
            # because a report with no count at all is a broken report rather
            # than a moved pin.
            if ($null -eq $scraped.samples) {
                $failures += "$($pins.resultFile) carries no extrapolation count"
            }
            $result.notes += ("DUKEMARK {0}: fps min {1} / avg {2} / max {3} (MEASUREMENTS), " +
                "{4} samples, info {5}") -f $pins.demo, $scraped.fps_min, $scraped.fps_avg,
                $scraped.fps_max, $scraped.samples, $scraped.info
            if (-not [string]::IsNullOrWhiteSpace($KeepProfilesIn) -and $null -ne $scraped.report) {
                Set-Content -Encoding utf8 `
                    -LiteralPath (Join-Path $KeepProfilesIn "$($Fixture.name).dukemark.txt") `
                    -Value $scraped.report
            }
        }
    }

    $result.invariant = if ($failures.Count -eq 0) { "pass" } else { "FAIL" }
    $result.notes += $failures

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

        # The DUKEMARK extrapolation count, held to a band. Same sidecar, same
        # -RecordInvariants / -Force machinery and the same three outcomes as the
        # frame hash above: a moved pin is a reviewable one-line diff with the
        # manifest sha moved beside it, never a hand edit inside this script.
        # Unlike the hash it is a BAND, so what the sidecar carries is the centre
        # and the tolerance, and only a value outside the band fails.
        if ($row.Contains("dukemark_samples") -and $null -ne $row.dukemark_samples) {
            $recorded = if ($invariants.Contains($fixture.name)) {
                $invariants[$fixture.name]
            } else { $null }
            $pinned = if ($null -ne $recorded -and $recorded.Contains("dukemark_samples")) {
                [int]$recorded.dukemark_samples
            } else { $null }
            $tolerance = if ($null -ne $recorded -and
                $recorded.Contains("dukemark_samples_tolerance")) {
                [double]$recorded.dukemark_samples_tolerance
            } else { $dukemarkSampleTolerance }

            if ($RecordInvariants) {
                $allowed = if ($null -ne $pinned) {
                    [math]::Max(1, [math]::Round($pinned * $tolerance))
                } else { 0 }
                if ($null -ne $pinned -and
                    [math]::Abs($row.dukemark_samples - $pinned) -gt $allowed -and -not $Force) {
                    throw ("$($fixture.name) already has a recorded DUKEMARK sample pin of " +
                        "$pinned +/- $allowed and this run read $($row.dukemark_samples). " +
                        "Re-recording would erase the evidence of a real change. Pass -Force " +
                        "only if you have established that the move is legitimate.")
                }
                if (-not $invariants.Contains($fixture.name)) {
                    $invariants[$fixture.name] = @{}
                }
                $invariants[$fixture.name].dukemark_samples = $row.dukemark_samples
                $invariants[$fixture.name].dukemark_samples_tolerance = $tolerance
                $row.notes += "DUKEMARK sample pin recorded ($($row.dukemark_samples) +/- $tolerance)"
            } elseif ($null -eq $pinned) {
                $row.notes += "no recorded DUKEMARK sample pin to compare against"
                if ($row.invariant -eq "pass") { $row.invariant = "unpinned" }
            } else {
                $allowed = [math]::Max(1, [math]::Round($pinned * $tolerance))
                $drift = [math]::Abs($row.dukemark_samples - $pinned)
                $row.dukemark_samples_pin = $pinned
                $row.dukemark_samples_drift = $drift
                if ($drift -gt $allowed) {
                    $row.invariant = "FAIL"
                    $row.notes += ("DUKEMARK extrapolated $($row.dukemark_samples) samples " +
                        "against a pin of $pinned +/- $allowed -- the demo stalled or did not " +
                        "play to completion")
                }
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
    schema           = $scoreboardSchema
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

$markdown = @(Get-ScoreboardMarkdown $rows $Label $Arm $OneLookupStore $OneLookupLoad)
$markdownPath = Join-Path $ResultsDirectory "scoreboard.md"
$markdown -join "`n" | Set-Content -LiteralPath $markdownPath -Encoding utf8

Write-Host ""
Write-Host ($markdown -join "`n")
Write-Host ""
Write-Host "wrote $jsonPath"

# Anything that is not a checked pass or a deliberate `unpinned` is a failure.
# An allow-list rather than a `-eq "FAIL"` test on purpose: the old form counted
# only the one word, so a fixture that never got as far as being graded (a
# crashed emulator reported `no-profile`, an early return left `unchecked`)
# exited 0 and a sweep of nothing but crashes read as a clean sweep.
$failed = @($rows | Where-Object { $_.invariant -notin @("pass", "unpinned") })
if ($failed.Count -gt 0) {
    Write-Host ""
    Write-Error ("{0} fixture(s) failed their invariant: {1}" -f
        $failed.Count, (($failed | ForEach-Object { "$($_.name) [$($_.invariant)]" }) -join ", "))
    exit 1
}

# Say 0 explicitly. Falling off the end leaves $LASTEXITCODE holding whatever
# the last NATIVE command set, and the last native command here is the emulator
# -- so a board on which every fixture passed reported exit 1 on 2026-08-12
# purely because a guest had exited non-zero. That is the dangerous direction
# for a gate: the obvious repair is to stop trusting this script's status, which
# would also silence the real `exit 1` above and turn the gate into decoration.
exit 0
