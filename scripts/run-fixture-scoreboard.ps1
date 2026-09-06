# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
One pass over every game fixture, reporting real-time factor and the JIT
counters beside each fixture's correctness invariant.

.DESCRIPTION
One current-model capture per fixture, with runtime validity checked separately
from context-qualified historical pins. The realtime gate retains historical
calibration and refuses the current sole timing model.

Each fixture is invoked with the EXACT arguments recorded for it in
.bench/PROTOCOL.md. That is not a style choice: the framebuffer hashes below
were measured under those arguments, so changing a persona, a memory size or a
video card silently invalidates the invariant rather than failing loudly.

INVARIANT HISTORY

2026-08-10  duke3d-486 and duke3d-586 lost their framebuffer hash entirely. The
            end-of-budget frame was cutoff-phase sensitive and moved six times in
            three days for entirely benign reasons. What replaced it: the guest's
            own EXITVM exit code, the redirected DUKEMARK report, its Info String
            config fingerprint, and the extrapolation count held to a band. See
            the DUKEMARK block below.

2026-08-18  tombraid-586 and nascar-586 lost their end-of-budget framebuffer
            hash for the same reason, and gained a FRAME CONTRACT in its place:
            one exact frame hash at a cadence-stable EARLY anchor, plus content
            bands, a display class and a guest-progress tolerance at the budget.
            See New-FrameContract for the full argument and every derivation.

            The two motivating incidents are both from that day. tombraid-586's
            hash moved 84.31% of its pixels under the IOPL-3 V86 monitor
            (.bench/results/iopl3-tombraid-attribution/) -- the attract camera a
            beat further along and the blinking "Demo Mode" caption in its other
            phase, with rendering perfect. nascar-586's moved 12.41% under the
            same day's follow-up (.bench/results/postiopl-nascar-attribution/) --
            the camera a beat along the trackside banner. Both re-pins were
            justified; both cost a full attribution cycle to prove that nothing
            was wrong; and both anchors would have moved again on the next
            cadence-adjacent change.

            SCOPE. tombraid is the owner-approved task. nascar was re-pinned the
            same day for the identical cause and the coordinator extended the
            scope to it, so the two rows are redesigned together and share one
            mechanism.

            This is NOT the duke3d answer applied literally. duke3d could drop
            its frame invariant outright because DUKEMARK scores itself; these
            two rows print no score, so dropping it would leave them graded on
            counts alone -- and PROTOCOL.md trap 0 records that count-only
            framebuffer invariants DO NOT DISCRIMINATE. The early anchor exists
            to keep an exact-pixel invariant on the row after the fragile one is
            gone.

Real-time factor is guest seconds per wall second. 1.0 is real time, higher is
faster than the machine being emulated.

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -Label before-slice

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -Fixtures doom-486,wolf3d-486 -Label quick

.EXAMPLE
pwsh scripts/run-fixture-scoreboard.ps1 -SelfTest
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound [string] parameter.
# Measured 2026-08-27: `-Fixtures prince-486 tombraid-loader-586` (the shape an
# outer PowerShell produces from `-Fixtures @('prince-486','tombraid-loader-586')`)
# bound only prince-486 to -Fixtures, bound 'tombraid-loader-586' to
# -ResultsDirectory, ran ONE row of a two-row sweep, wrote the board into a
# directory literally named tombraid-loader-586 in the repository root, and
# EXITED 0. With positional binding off, the stray token is a binder error
# before one line of this script runs. Every argument must be named; every
# documented caller already complies. The comma shape `-Fixtures a,b` still
# works: Resolve-FixtureSelection splits it, the same way
# Resolve-KnobPassthrough splits -Knobs.
[CmdletBinding(PositionalBinding = $false)]
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
    # ARM PASSTHROUGH. Extra IZARRAVM_* knobs to arm on EVERY row, each written
    # as one NAME=VALUE string:
    #
    #   -Knobs IZARRAVM_SEGMENT_RETIRE_GOVERNOR=off
    #   -Knobs IZARRAVM_JCC_SHADOW=1,IZARRAVM_PIT_BULK_ADVANCE=0
    #
    # Comma-separated, and this script splits on the comma ITSELF rather than
    # leaving it to the parameter binder: `pwsh -File ... -Knobs A=1,B=2` binds
    # ONE string, so the binder would otherwise arm A to the value "1,B=2" and
    # never arm B at all. A knob value therefore may not contain a comma; one
    # that does is rejected loudly, never truncated. See Resolve-KnobPassthrough.
    #
    # This exists so a knob slice can ladder ON against OFF with THIS harness
    # instead of a fresh one-off direct-invocation script per slice. Four such
    # scripts accumulated in .bench/scripts/ before this parameter existed and
    # rewriting them produced real errors -- one invented a counter name that
    # does not exist, another paraphrased a fixture's key injection and would
    # have graded a crash loop as if it were the game.
    #
    # NOTHING IS INHERITED. The caller states the value; the scrub in
    # Get-RowEnvironment still removes every IZARRAVM_* the parent shell
    # happened to be carrying, including any name listed here. Reading the
    # value out of the parent environment instead would re-open exactly the
    # hole that scrub was built to close.
    #
    # EMPTY IS NOT UNSET, AND THIS SCRIPT WILL NOT GUESS WHICH YOU MEANT.
    # Two incompatible knob conventions are live in the tree right now:
    #
    #   IZARRAVM_SEGMENT_RETIRE_GOVERNOR  unset means the default `cap`,
    #                                     but "" means OFF. They DISAGREE.
    #   IZARRAVM_JCC_SHADOW,              unset == "" == the default.
    #   IZARRAVM_PIT_BULK_ADVANCE,        They AGREE.
    #   IZARRAVM_CHAIN_ENTRY_CHECK
    #
    # So the two states are spelled differently and neither is ever silently
    # turned into the other:
    #
    #   NAME=       sets NAME to the EMPTY STRING in the child -- present,
    #               and readable by var_os()/is_some().
    #   (omitted)   leaves NAME REMOVED from the child by the scrub.
    #
    # A bare NAME with no `=` is rejected rather than assumed, because it is
    # ambiguous between those two and they mean OPPOSITE things depending on
    # which knob is named. Values are passed through byte for byte -- never
    # trimmed, never case-folded, never re-quoted.
    #
    # Names the board sets itself (IZARRAVM_JIT, the JIT16 pair, the one-lookup
    # pair, the barrier census, and every observer override) are RESERVED and
    # rejected loudly. Use -Arm / -OneLookupStore / -OneLookupLoad for those;
    # a caller who reaches for -Knobs to override IZARRAVM_JIT has made a
    # mistake and is told so rather than quietly winning or quietly losing.
    # The reserved set is DERIVED from the board's own table in
    # Get-BoardOwnedEnvironment, so it cannot drift out of date.
    #
    # Whatever is armed is recorded as the `knobs` object on every row and on
    # the board summary, so a leg can be audited afterwards and nobody has to
    # take it on trust that the knob was actually set.
    [string[]]$Knobs = @(),
    [switch]$RecordInvariants,
    [switch]$Force,
    [switch]$ListFixtures,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

# The benchmark workspace: fixtures, results and pinned binaries. It is NOT in the
# repository -- ~115 GB across ~450,000 files, and the game fixtures are commercial
# installs that cannot be redistributed -- so only this harness is tracked and the
# data lives wherever the machine has room for it.
#
# IZARRAVM_BENCH_ROOT overrides the location. Nulling follows the campaign's
# AGREEING convention, the one IZARRAVM_JCC_SHADOW / IZARRAVM_PIT_BULK_ADVANCE /
# IZARRAVM_CHAIN_ENTRY_CHECK use: UNSET and EMPTY both mean the default,
# <repo>/.bench. (IZARRAVM_SEGMENT_RETIRE_GOVERNOR is the one knob on this campaign
# where unset and empty DISAGREE; do not copy it.)
#
# A value that is set but does not exist is a HARD ERROR, never a silent fallback to
# the default. A typo'd bench root that quietly resolved to <repo>/.bench would run
# the wrong fixture set and report it as the right one.
function Resolve-BenchRoot([string]$RepositoryRoot) {
    $configured = $env:IZARRAVM_BENCH_ROOT
    if ([string]::IsNullOrEmpty($configured)) {
        return [IO.Path]::GetFullPath((Join-Path $RepositoryRoot ".bench"))
    }
    $resolved = [IO.Path]::GetFullPath($configured)
    if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
        throw ("IZARRAVM_BENCH_ROOT is set to '$configured' (resolved '$resolved') but " +
            "that directory does not exist. Point it at the benchmark workspace, or " +
            "unset it to use the default <repo>/.bench.")
    }
    return $resolved
}
$benchRoot = Resolve-BenchRoot $repositoryRoot
$invariantPath = Join-Path $PSScriptRoot "fixture-scoreboard-invariants.json"
$scoreboardSchema = "izarravm-fixture-scoreboard-v3"

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

    # B2 entry governor. OPTIONAL, defaulting to zero: baseline profiles recorded before the
    # governor existed carry no such keys, and a scoreboard that threw on them could not compare
    # a governed run against the baseline it has to be compared against. `governor_backoffs` is
    # the Gate-0 zero-check; `governor_windows` is its non-vacuity companion, because a row with
    # zero back-offs and zero windows never measured anything.
    $perf = $Profile.PSObject.Properties["perf"].Value
    $Result.governor_windows = Get-OptionalUInt64Property $perf "governor_windows" "perf"
    $Result.governor_backoffs = Get-OptionalUInt64Property $perf "governor_backoffs" "perf"
    $Result.governor_probe_windows =
        Get-OptionalUInt64Property $perf "governor_probe_windows" "perf"
    $Result.governor_rearms = Get-OptionalUInt64Property $perf "governor_rearms" "perf"

    # Compatibility fields keep their v1 names and rounding.
    $Result.native_insns = $coverage.direct_insns
    $Result.native_coverage = [math]::Round($coverage.direct_coverage, 4)
    $Result.insns_per_entry = $Result.direct_insns_per_entry
    return $coverage
}

# A counter that may be absent from an older profile. Unlike `Get-RequiredUInt64Property` this
# tolerates absence (returning 0) but NOT a present-and-malformed value.
function Get-OptionalUInt64Property($InputObject, [string]$Name, [string]$Path) {
    if ($null -eq $InputObject -or $null -eq $InputObject.PSObject.Properties[$Name]) {
        return [UInt64]0
    }
    return Get-RequiredUInt64Property $InputObject $Name $Path
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
    [string]$StoreArm, [string]$LoadArm, $BoardKnobs = $null) {
    $markdown = @()
    $markdown += "# Fixture scoreboard$(if ($BoardLabel) { ": $BoardLabel" })"
    $markdown += ""
    $markdown += "Recorded $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')), JIT arm ``$BoardArm``, one-lookup store ``$StoreArm``, one-lookup load ``$LoadArm``. rt is guest seconds per wall second; 1.0 is real time."
    # State the passthrough on the human-readable board too. A ladder leg whose
    # Markdown does not say which arm it ran is a leg that gets mislabelled the
    # moment two of them sit in the same directory. `NAME=` with nothing after
    # it is a knob armed to the empty string, which is NOT the same as absent.
    if ($null -ne $BoardKnobs -and $BoardKnobs.Count -gt 0) {
        $markdown += ("Arm passthrough: " + (($BoardKnobs.GetEnumerator() |
            ForEach-Object { "``$($_.Key)=$($_.Value)``" }) -join ", ") +
            ". Every other IZARRAVM_* knob is removed from the child environment.")
    }
    $markdown += "Direct insns/entry includes emitted instructions and successful helper instructions. Abnormal helper attempts replay in the interpreter and do not count as helper-retired instructions."
    $markdown += ""
    $markdown += "| fixture | rt | wall s | emitted | helper | interpreter | entries | direct insns/entry | emitted insns/entry | 16-bit direct insns/entry | governor win/back/probe/rearm | invariant |"
    $markdown += "|---|---|---|---|---|---|---|---|---|---|---|---|"
    foreach ($row in $Rows) {
        $has = { param($n) $row.PSObject.Properties.Name -contains $n }
        $markdown += ("| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7} | {8} | {9} | {10} | {11}{12} |" -f
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
            $(if (& $has "governor_backoffs") {
                    "{0}/{1}/{2}/{3}" -f $row.governor_windows, $row.governor_backoffs,
                        $row.governor_probe_windows, $row.governor_rearms
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

function Assert-ScoreboardQualificationSelfTest {
    $table = @(Get-FixtureTable)
    Assert-ScoreboardSelfTestEqual $table.Count 21 'full board row count'
    foreach ($pair in @(@('doom-486', 32000000000), @('doom-586', 26560000000),
        @('quake-586', 24800000000), @('tyrian-setup-486', 4700000000), @('tombraid-loader-586', 500000000))) {
        Assert-ScoreboardSelfTestEqual (@($table | Where-Object name -eq $pair[0])[0].cycles) $pair[1] 'literal fixture window'
    }
    Assert-ScoreboardSelfTestEqual (Get-FrameContract (@($table | Where-Object name -eq 'tombraid-586')[0])).anchorCycles 500000000 'unchanged anchor window'
    $directory = Join-Path ([IO.Path]::GetTempPath()) ('izarravm-pin-selftest-' + [Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $directory
    $previousBench = $script:benchRoot
    $previousKnobs = $script:Knobs
    $previousEpoch = [Environment]::GetEnvironmentVariable('IZARRAVM_TIMING_EPOCH')
    try {
        $script:benchRoot = $directory
        $hdd = Join-Path $directory 'hdd'
        $null = New-Item -ItemType Directory -Path $hdd
        [IO.File]::WriteAllText((Join-Path $hdd 'AUTOEXEC.BAT'), 'GAME.EXE')
        [IO.File]::WriteAllText((Join-Path $hdd '.hidden'), 'hidden input')
        [IO.File]::SetAttributes((Join-Path $hdd '.hidden'), [IO.FileAttributes]::Hidden)
        [IO.File]::WriteAllText((Join-Path $directory 'disc.cue'), 'FILE "track.bin" BINARY')
        [IO.File]::WriteAllBytes((Join-Path $directory 'track.bin'), [byte[]]@(1,2,3))
        $fixture = [pscustomobject]@{
            name = 'synthetic'; folder = 'hdd'; arguments = @('--cpu','586','--memory-mib','64','--video','vega')
            cycles = [uint64]100; injection = @('--inject-key-at', '10:1c'); resultPpm = $true
            qconsole = $false; gametics = $null; dukemark = $null; cdImage = 'disc.cue'
            frameContract = New-FrameContract -AnchorCycles 20 -AnchorDisplay VgaRaster
        }
        $descriptor = Get-FixtureDescriptor $fixture $hdd
        Assert-ScoreboardSelfTestEqual $descriptor.hdd_files.Count 2 'hidden fixture bytes included'
        Assert-ScoreboardSelfTestEqual $descriptor.cd_files.Count 2 'CUE and track included'
        $context = New-PinContext $fixture $descriptor
        $pins = @{ pin_context = $context; qualified_axes = @('frame') }
        $script:Knobs = @('IZARRAVM_DEVICE_TIMING=ata')
        $alternate = New-PinContext $fixture (Get-FixtureDescriptor $fixture $hdd)
        Assert-ScoreboardSelfTestEqual (Test-PinContext $pins $alternate) $false 'disk timing policy changes pin context'
        $script:Knobs = $previousKnobs
        $row = [ordered]@{ pin_context = $context; invariant = 'pass'; notes = @(); refused_axes = @() }
        Assert-ScoreboardSelfTestEqual (Test-RowPin $pins $row 'frame') $true 'exact qualified context'
        Assert-ScoreboardSelfTestEqual (Test-RowPin $pins $row 'profile_bands') $false 'axis is independently qualified'
        Assert-ScoreboardSelfTestEqual (Test-PinContext @{} $context) $false 'historical context absent'
        foreach ($key in @('timing_model_epoch', 'cycle_budget', 'anchor_cycle_budget', 'fixture_contract_sha256')) {
            $changed = $context | ConvertTo-Json -Depth 16 | ConvertFrom-Json -AsHashtable
            $changed[$key] = if ($key -eq 'fixture_contract_sha256') { 'different' } else { 999 }
            Assert-ScoreboardSelfTestEqual (Test-PinContext @{ pin_context = $changed } $context) $false "changed $key"
            $changed.Remove($key)
            Assert-ScoreboardSelfTestEqual (Test-PinContext @{ pin_context = $changed } $context) $false "missing $key"
        }
        foreach ($mutation in @('hdd', 'track', 'arguments', 'injection')) {
            switch ($mutation) {
                hdd { [IO.File]::WriteAllText((Join-Path $hdd 'AUTOEXEC.BAT'), 'OTHER.EXE') }
                track { [IO.File]::WriteAllBytes((Join-Path $directory 'track.bin'), [byte[]]@(3,2,1)) }
                arguments { $fixture.arguments[1] = '486' }
                injection { $fixture.injection[1] = '11:1c' }
            }
            $changed = New-PinContext $fixture (Get-FixtureDescriptor $fixture $hdd)
            Assert-ScoreboardSelfTestEqual (Test-PinContext $pins $changed) $false "input mutation $mutation"
            [IO.File]::WriteAllText((Join-Path $hdd 'AUTOEXEC.BAT'), 'GAME.EXE')
            [IO.File]::WriteAllBytes((Join-Path $directory 'track.bin'), [byte[]]@(1,2,3))
            $fixture.arguments[1] = '586'; $fixture.injection[1] = '10:1c'
        }
        Assert-ScoreboardSelfTestThrows { Get-ContainedPath $hdd '../escape' } 'Invalid fixture path' 'parent traversal'
        $junction = Join-Path $directory 'linked'
        $null = New-Item -ItemType Junction -Path $junction -Target $hdd
        try {
            Assert-ScoreboardSelfTestThrows {
                Get-FixtureFileIdentities $directory @('linked/AUTOEXEC.BAT')
            } 'reparse point' 'selected track cannot traverse a junction'
            Assert-ScoreboardSelfTestThrows {
                Get-FixtureFileIdentities $junction @('AUTOEXEC.BAT')
            } 'reparse point' 'selected input root cannot be a junction'
        } finally { Remove-Item -LiteralPath $junction -Force }
        $fixture.PSObject.Properties.Remove('frameContract')
        $context = New-PinContext $fixture (Get-FixtureDescriptor $fixture $hdd)
        $invariants = [ordered]@{ synthetic = @{ frame_sha256 = 'old'; final_nonblack_percent_min = 80
            pin_context = @{ schema = 'obsolete' }; qualified_axes = @('content_bands') } }
        $observation = [ordered]@{ pin_context = $context; invariant = 'pass'; notes = @(); refused_axes = @(); frame_sha256 = 'new' }
        Complete-RowPins $fixture $observation $invariants $true $true
        Assert-ScoreboardSelfTestEqual (@($invariants.synthetic.qualified_axes) -join ',') 'frame' 'recording does not qualify old sibling bands'
        Complete-RowPins $fixture $observation $invariants $true $true
        $roundTrip = $invariants | ConvertTo-Json -Depth 16 | ConvertFrom-Json -AsHashtable
        Assert-ScoreboardSelfTestEqual (Test-PinContext $roundTrip.synthetic $context) $true 'pin context JSON round trip'
        Assert-ScoreboardSelfTestEqual (@($roundTrip.synthetic.qualified_axes) -join ',') 'frame' 'repeated recording does not launder siblings'
        $invalid = [ordered]@{ pin_context = $context; invariant = 'FAIL'; notes = @('bad anchor'); refused_axes = @() }
        Complete-RowPins $fixture $invalid @{}
        Assert-ScoreboardSelfTestEqual $invalid.invariant 'FAIL' 'failed capture remains failed without pins'
        Assert-ScoreboardSelfTestThrows { Complete-RowPins $fixture $invalid @{} $true $true } 'Cannot record invalid capture' 'failed anchor cannot qualify pins'
        $profile = [pscustomobject]@{
            schema = 'izarravm-hdd-profile-v2'
            timing_model_epoch = 2; mode = '586'; cycle_budget = 100; elapsed_budget_clocks = 100
            real_time_factor = 1.0; guest_seconds = 1.0; wall_seconds = 1.0
            stop = [pscustomobject]@{ kind = 'cycle_limit'; requested = 7 }
        }
        Assert-FixtureCapture $fixture $profile 0 100
        $profile.schema = 'wrong'
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'profile schema' 'wrong schema'
        $profile.schema = 'izarravm-hdd-profile-v2'
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 1 100 } 'Host exit code' 'host failure'
        $profile.mode = '486'
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'Effective CPU' 'CMOS override'
        $profile.mode = '586'; $profile.timing_model_epoch = 1
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'explicitly report timing model 2' 'old model'
        $profile.PSObject.Properties.Remove('timing_model_epoch')
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'missing profile.timing_model_epoch' 'missing current model'
        $profile | Add-Member timing_model_epoch 2
        $profile.elapsed_budget_clocks = 99
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'full cycle window' 'truncated window'
        $profile.elapsed_budget_clocks = 100; $profile.real_time_factor = [double]::NaN
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $fixture $profile 0 100 } 'Invalid real_time_factor' 'NaN capture'
        $nanBand = Test-ProfileBands $profile @(@{ path = 'real_time_factor'; min = 0; max = 10 }) $false
        Assert-ScoreboardSelfTestEqual $nanBand.failures.Count 1 'unqualified bands still reject malformed fields'
        $profile.real_time_factor = 1.0
        $profile.elapsed_budget_clocks = 50
        $profile.stop = [pscustomobject]@{ kind = 'test_exit'; code = 81 }
        foreach ($completionName in @('duke3d-586', 'mojo-586')) {
            $completionFixture = @($table | Where-Object name -eq $completionName)[0]
            Assert-FixtureCapture $completionFixture $profile 0 100
            Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $completionFixture $profile 81 100 } 'Host exit code' 'guest code is not a host success code'
            $profile.stop.code = 0
            Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $completionFixture $profile 0 100 } 'Guest did not complete' 'wrong guest completion code'
            $profile.stop.code = 81
        }
        $doom = @($table | Where-Object name -eq 'doom-586')[0]
        $profile.stop.code = 0
        Assert-FixtureCapture $doom $profile 0 100
        $profile.stop.kind = 'cycle_limit'; $profile.elapsed_budget_clocks = 100
        Assert-ScoreboardSelfTestThrows { Assert-FixtureCapture $doom $profile 0 100 } 'Guest did not complete' 'Doom idle tail is not completion'
        $prepared = Join-Path $directory 'prepared-doom'
        Copy-Fixture $hdd $prepared
        Prepare-FixtureInputs $doom $prepared
        Assert-ScoreboardSelfTestEqual (Test-Path -LiteralPath (Join-Path $hdd 'EXITVM.COM')) $false 'original fixture stays untouched'
        Assert-ScoreboardSelfTestEqual ([IO.File]::ReadAllText((Join-Path $prepared 'AUTOEXEC.BAT'))) 'GAME.EXE' 'preparation preserves AUTOEXEC'
        Assert-ScoreboardSelfTestEqual ([Convert]::ToHexString([IO.File]::ReadAllBytes((Join-Path $prepared 'EXITVM.COM')))) 'B00CE6E4B000E6E5B003E6E6F4EBFD' 'canonical zero-exit helper'
        $preparedDescriptor = Get-FixtureDescriptor $doom $prepared
        Assert-ScoreboardSelfTestEqual $preparedDescriptor.hdd_files.Count 3 'prepared identity includes the exit helper'
        Prepare-FixtureInputs $fixture $hdd
        Assert-ScoreboardSelfTestEqual (Test-Path -LiteralPath (Join-Path $hdd 'EXITVM.COM')) $false 'unrelated fixture receives no helper'
        $quake = Join-Path $directory 'qconsole.log'
        [IO.File]::WriteAllText($quake, '969 frames 24.3 seconds 39.9 fps')
        $null = Read-ScoreboardQuakeResult $quake
        foreach ($bad in @('968 frames 24.3 seconds 39.9 fps', '969 frames 0 seconds 39.9 fps',
            '969 frames 24.3 seconds 100 fps', "969 frames 24.3 seconds 39.9 fps`n969 frames 24.3 seconds 39.9 fps")) {
            [IO.File]::WriteAllText($quake, $bad)
            Assert-ScoreboardSelfTestThrows { Read-ScoreboardQuakeResult $quake } 'QCONSOLE' 'invalid timedemo'
        }
        [Environment]::SetEnvironmentVariable('IZARRAVM_TIMING_EPOCH', '2')
        $child = Get-ChildEnvironmentSnapshot (Get-RowEnvironment)
        Assert-ScoreboardSelfTestEqual $child.ContainsKey('IZARRAVM_TIMING_EPOCH') $false 'actual child drops inherited selector'
        Assert-ScoreboardSelfTestThrows {
            Resolve-KnobPassthrough @('IZARRAVM_TIMING_EPOCH=2') @((Get-BoardOwnedEnvironment).Keys)
        } 'which this board sets itself' 'selector override refused'
        $board = @'
Info for Voodoo board # 0:
=====================================================
Virtual Base Address:                       0x10400000
Physical Base Address:                      0xe1000000
PCI Device Number:                          0x10
Vendor ID:                                  0x121a
Device ID:                                  0x1
FBI Revision:                               2
FBI Memory:                                 4 MB
FBI PowerOn Sense:                          0x6
TMU PowerOn Sense:                          0xc1
FBI DAC Output Color Format:                24BPP
Scan-Line Interleaved?                      No
TMU Revision:                               1
Number TMUs:                                2
TMU 0 RAM:                                  4 MB
TMU 1 RAM:                                  4 MB
'@
        $registers = @'
  Register Name      Data  Address
---------------  -------- --------
         status: 0ffff03f        0
		       3f : pci fifo free space (63)
			0 : vertical retrace
			0 : fbi busy
			0 : tmu busy
			0 : sst busy
			0 : displayed buffer
		     ffff : mem fifo free space (65535)
			0 : swap buffers pending
   fbzColorPath: 00000000      104
        fogMode: 00000000      108
      alphaMode: 00000000      10c
        fbzMode: 00000000      110
		          : zfunction
			0 : drawbuffer (0=front, 1=back)
        lfbMode: 00000000      114
		      565 : lfb format
			0 : writebuffer (0=front, 1=back, 2=aux)
			0 : readbuffer (0=front, 1=back, 2=aux)
		     ARGB : rgba lanes
  clipLeftRight: 00000000      118
  clipBottomTop: 00000000      11c
        stipple: 00000000      140
             c0: 00000000      144
             c1: 00000000      148
    fbiPixelsIn: 00000000      14c
  fbiChromaFail: 00000000      150
   fbiZfuncFail: 00000000      154
   fbiAfuncFail: 00000000      158
   fbiPixelsOut: 00000000      15c
       fbiInit4: 00000003      200
       vRetrace: 00000206      204
      backPorch: 00000000      208
videoDimensions: 00000000      20c
       fbiInit0: 00001c10      210
       fbiInit1: 002011a8      214
       fbiInit2: 1824b0e0      218
       fbiInit3: 00110601      21c
'@
        Assert-MojoReports $board $registers
        foreach ($bad in @($board.Replace('0x121a', '0x1234'), $board.Replace('4 MB', '2 MB'),
            $board.Replace('Number TMUs:', 'Missing TMUs:'), "$board`nNumber TMUs: 1",
            "$board`nNo Voodoo boards found", "$board`nInfo for Voodoo board # 0:",
            $board.Replace('Info for Voodoo board # 0:', ''))) {
            Assert-ScoreboardSelfTestThrows { Assert-MojoReports $bad $registers } 'MOJO' 'invalid board identity'
        }
        foreach ($bad in @('', ($registers -split "`n" | Select-Object -First 10) -join "`n",
            "$registers`nstatus: 00000000 0", $registers.Replace('fbzColorPath:', 'missing:'),
            ($registers -replace '(?m)^(\s*status:)\s+\S+', '$1 invalid!'),
            ($registers -replace '(?m)^(\s*fbzColorPath:\s+\S+\s+)104', '${1}105'))) {
            Assert-ScoreboardSelfTestThrows { Assert-MojoReports $board $bad } 'MOJO' 'invalid register report'
        }
        $dynamic = $registers -replace '(?m)^(\s*(?:status|vRetrace):)\s+\S+', '$1 12345678'
        Assert-MojoReports $board $dynamic
    } finally {
        $script:benchRoot = $previousBench
        $script:Knobs = $previousKnobs
        [Environment]::SetEnvironmentVariable('IZARRAVM_TIMING_EPOCH', $previousEpoch)
        $resolved = [IO.Path]::GetFullPath($directory)
        if (-not $resolved.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()), [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Self-test cleanup left the temporary root'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}


function Invoke-ScoreboardSelfTest {
    # The profile-band grader must be provable RED: a gate that cannot fail is
    # systemic. One in-range band, one below the floor, one missing path.
    $bandProfile = [pscustomobject]@{
        timer = [pscustomobject]@{ irq0_edges = 3500 }
        mpu   = [pscustomobject]@{ wavetable = [pscustomobject]@{ data_writes = 90 } }
    }
    $bandPass = Test-ProfileBands $bandProfile @(
        @{ path = "timer.irq0_edges"; min = 2400; max = 4800 })
    Assert-ScoreboardSelfTestEqual $bandPass.failures.Count 0 "in-range band passes"
    Assert-ScoreboardSelfTestEqual $bandPass.values["band_timer_irq0_edges"] 3500.0 `
        "band value is recorded"
    $bandFail = Test-ProfileBands $bandProfile @(
        @{ path = "mpu.wavetable.data_writes"; min = 3000; max = 20000 })
    Assert-ScoreboardSelfTestEqual $bandFail.failures.Count 1 "a collapsed count goes RED"
    $bandMissing = Test-ProfileBands $bandProfile @(
        @{ path = "sb_dsp.command_bytes"; min = 1; max = 2 })
    Assert-ScoreboardSelfTestEqual $bandMissing.failures.Count 1 "a missing field goes RED"
    # A path that stops one segment short resolves to an OBJECT; the grader
    # must turn the failed cast into a RED row, never a terminating error.
    $bandObject = Test-ProfileBands $bandProfile @(
        @{ path = "mpu.wavetable"; min = 1; max = 2 })
    Assert-ScoreboardSelfTestEqual $bandObject.failures.Count 1 "a non-numeric target goes RED"

    Assert-ScoreboardQualificationSelfTest

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
    # A profile with no governor keys -- every pre-B2 baseline -- must read zeros rather than
    # throw, or a governed run could not be compared against the baseline it is measured against.
    Assert-ScoreboardSelfTestEqual $json.rows[0].governor_backoffs 0 "absent governor counter"

    $markdown = @(Get-ScoreboardMarkdown @([pscustomobject]$row) "self-test" "on" "1" "1")
    $rendered = $markdown -join "`n"
    if ($rendered -notmatch '\| governor win/back/probe/rearm \|' -or
        $rendered -notmatch '\| emitted \| helper \| interpreter \|' -or
        $rendered -notmatch 'Direct insns/entry includes emitted instructions' -or
        $rendered -notmatch 'Abnormal helper attempts replay in the interpreter') {
        throw "scoreboard self-test failed: Markdown coverage headers or IPE definition are missing"
    }
    $first = "2c55fef04eeb555d02790b336a36fff2a7ce04245a40b810ea3bea83d9061403"
    $second = "30abcde0d496b5e275704c0dcf270f0ea15a3e7171cf9f1d04d7468074b259dd"
    $third = "0000000000000000000000000000000000000000000000000000000000000000"
    if (-not (Test-Sha256Allowed $first $first)) {
        throw "scoreboard self-test failed: a scalar allowed hash was not normalised"
    }
    if (-not (Test-Sha256Allowed @($first, $second) $second)) {
        throw "scoreboard self-test failed: the second allowed hash was rejected"
    }
    if (Test-Sha256Allowed @($first, $second) $third) {
        throw "scoreboard self-test failed: an unlisted third hash was accepted"
    }
    Assert-ScoreboardFrameStatsSelfTest

    # The armed passthrough has to show up on the human-readable board too, or
    # two ladder legs in one directory become indistinguishable.
    $knobbed = @(Get-ScoreboardMarkdown @([pscustomobject]$row) "self-test" "on" "1" "1" `
        ([ordered]@{ "IZARRAVM_SEGMENT_RETIRE_GOVERNOR" = "off" })) -join "`n"
    if ($knobbed -notmatch 'Arm passthrough: `IZARRAVM_SEGMENT_RETIRE_GOVERNOR=off`') {
        throw "scoreboard self-test failed: the Markdown board does not state the armed knobs"
    }
    if ($rendered -match 'Arm passthrough') {
        throw ("scoreboard self-test failed: an unknobbed board claims an arm passthrough " +
            "it did not run")
    }

    Assert-ScoreboardKnobPassthroughSelfTest
    Assert-ScoreboardFixtureSelectionSelfTest

    Write-Host "fixture scoreboard self-test passed"
}

# The -SelfTest dispatch used to sit here. It now sits just above the driver,
# because the frame-stats checks call a function defined further down the file
# and PowerShell only knows a function after its definition has been executed.

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
    param([string]$Demo = "BENCH2")
    @{
        demo       = $Demo
        info       = "2,320,200,2,0,1,1,1"
        resultFile = "DUKEMARK.TXT"
        # EXITVM.COM poking 0x51 at the unit-tester exit register, not zero.
        exitCode   = 0x51
    }
}

# ---------------------------------------------------------------------------
# FRAME CONTRACTS (2026-08-18). The replacement for the end-of-budget framebuffer
# hash on tombraid-586 and nascar-586.
#
# WHY. Both rows pinned a sha256 of the frame at a fixed cycle budget that lands
# mid-attract-demo, with the camera in flight. That hash is a function of the
# demo's PHASE, and phase is a function of interrupt cadence, so any
# cadence-adjacent change moves it even when rendering is perfect. It moved twice
# on 2026-08-18 alone: tombraid-586 by 84.31% of its pixels under the IOPL-3 V86
# monitor (.bench/results/iopl3-tombraid-attribution/), which was the camera a
# beat further along plus the blinking "Demo Mode" caption in the other phase,
# and nascar-586 by 12.41% under the same day's follow-up
# (.bench/results/postiopl-nascar-attribution/), the camera a beat along the
# trackside banner. Both re-pins were justified and both cost a full attribution
# cycle to establish that nothing was wrong. That is the duke3d situation
# exactly -- see the DUKEMARK block above, whose frame hash "moved six times in
# three days for entirely benign reasons" -- and this is the duke3d answer,
# adapted to two fixtures that print no score of their own.
#
# The evidence that the OLD invariant was measuring phase and not rendering: at
# the 28e9 budget, across the largest interrupt-cadence change this project has
# made, tombraid's frame changed in 84.31% of its pixels while its non-black
# coverage moved 305925 -> 305933 pixels (99.585% -> 99.588%), its colour count
# 173 -> 174, and its retired instruction count 0.129%. Every aggregate said the
# same picture; only the exact bytes disagreed. nascar reads the same way:
# 307186 -> 307188 non-black, 125 -> 121 colours, instructions 0.145%.
#
# WHAT REPLACES IT, in descending order of how much it is worth:
#
#   anchor    ONE exact frame hash, taken from a SECOND, short run of the same
#             fixture stopped early, at a point where the picture is not moving.
#             This is the piece that keeps the fixture honest about actual
#             rendering: PROTOCOL.md trap 0 records that count-only framebuffer
#             invariants DO NOT DISCRIMINATE (Grand Prix 2 produced the same
#             307,152 non-zero and the same 199 colours from two DIFFERENT
#             frames), so a row graded on bands alone would be a weaker fixture
#             than the one it replaced. The bands say the scene is right; the
#             anchor says the pixels are.
#
#             An anchor is only worth having where it is CADENCE-STABLE, and
#             that is a measurable property rather than a hope. The test used
#             here: sweep the anchor budget across a window and keep a point
#             whose frame is bit-identical across a window far wider than any
#             cadence-induced phase shift. A cadence change moves the guest by a
#             small offset -- 0.13-0.15% of retired instructions across the
#             IOPL-3 change -- so a frame that survives a +/-10% sweep of the
#             budget cannot be moved by one.
#
#             tombraid-586, 0.5e9: the DOS/4GW banner text page. Measured
#             bit-identical at 0.45e9, 0.50e9 and 0.60e9, and at 0.40e9 and
#             0.55e9 it takes a SECOND value differing in exactly 18 pixels --
#             a 9x2 block at x0-8, y334-335, the DOS underline cursor in
#             character cell (0,20), toggling between #000000 and #AAAAAA. The
#             cursor blink is the only moving thing on that page, so the anchor
#             pin is a SET of exactly two hashes and the fixture declares that
#             count; a third distinct value is a real change and fails even
#             under -Force. Everything outside those 18 pixels is bit-exact.
#             GROUND TRUTH: the 0.5e9 frame is byte-identical on the pre-IOPL-3
#             build, the IOPL-3 build (both in the attribution artifacts) and
#             on a393404e, which also carries PR 725's Katea write-through --
#             three builds spanning both an interrupt-cadence change and a
#             storage-path change.
#
#             nascar-586, 0.445e9: the game's own startup logo screen, a static
#             4-colour Margo LFB frame. Measured bit-identical at 0.395, 0.400,
#             0.450, 0.470, 0.490 and 0.495e9 -- a 100e6-cycle window, bounded
#             on BOTH sides by an all-black transition frame (0.385e9 and
#             0.498e9), which is why the anchor sits at the window's centre with
#             50e6 cycles of margin each way. That margin is 77x the phase shift
#             the IOPL-3 change produced. The 0.5e9 frame is also byte-identical
#             across 44592f6a and a393404e, i.e. across PR 725.
#
#   bands     Non-frame assertions at the end of the budget, where the picture IS
#             moving. Non-black coverage and distinct-colour count, each held to
#             a band, plus retired instructions to a tolerance. All three are
#             blind to which frame of the demo we landed on and none of them can
#             be satisfied by a black screen, a palette wipeout or a frame from
#             the wrong phase of the run.
#
#             HOW THE BANDS WERE DERIVED. Not from round numbers and not from one
#             run. Each row was sampled at several budgets either side of the
#             graded one -- a PHASE SPREAD, which moves the camera far further
#             than any cadence change can -- and the band was opened outward from
#             the envelope of those samples until it cleared the wrong-scene
#             frames the same fixture produces at other budgets.
#
#             tombraid-586, non-black coverage, band [89.0, 100.0]. Legitimate
#             samples: 99.498% at 27.5e9 and 99.517% at 28.5e9 (a393404e), 99.585%
#             pre-IOPL-3 and 99.588% post-IOPL-3 at the graded 28e9. Envelope
#             0.090 points wide. The floor is set 10 points under the lowest
#             rather than at 2.5x the envelope, because four frames under-sample
#             how much of a 3D scene can fall on pure black; nascar's six-frame
#             spread of the same kind of render swings 4.3 points, so 10 is about
#             2.3x the largest such swing measured anywhere in this harness.
#             FAILS, as it must: an all-black frame (0.0%), and the wrong-scene
#             frames this fixture itself produces -- the FMV at 5e9 (24.837%) and
#             at 15e9 (17.450%), and the boot text page (19.168%, which the
#             geometry check rejects too, at 720x400). Clearance over the worst of
#             those is 3.58x. The 100.0 ceiling is the physical maximum
#             and carries no information on its own; a solid non-black fill is
#             rejected by the colour floor, not by this band.
#
#             tombraid-586, distinct colours, band [79, 256]. Legitimate samples
#             160, 173, 174, 158 over the same four budgets. Floor is half the
#             lowest sample, which is 4.9 envelopes below it. FAILS: a black frame
#             or any single-colour palette wipeout (1), the boot text page (8).
#             The ceiling is not an arbitrary number -- 256 is the palette bound
#             of the 8bpp mode this fixture renders in, so it can never fire on a
#             legitimate frame, and it DOES fire on the fixture's own FMV frames,
#             which come through a deeper mode and read 564, 986 and 994.
#
#             nascar-586, non-black coverage, band [84.0, 100.0]. Legitimate
#             samples: 95.722% at 4.88e9, 99.748% at 4.93e9, 99.995% and 99.996%
#             at the graded 4.98e9 (pre and post the change that re-pinned it),
#             99.996% at 5.03e9, 99.992% at 5.08e9. Envelope 4.274 points wide,
#             so the floor sits 2.74 envelopes under the lowest sample -- 11.72
#             points of room. FAILS: the all-black transition frames measured at
#             0.385e9 and 0.498e9 (0.000%), the startup logo (6.112%), the boot
#             text page (20.048%, 720x400), and the mid-load screen at 0.5e9
#             (32.765%). Clearance over the worst of those is 2.56x.
#
#             nascar-586, distinct colours, band [45, 256]. Legitimate samples
#             118, 122, 125, 121, 126, 147. Floor is 2.52 envelopes under the
#             lowest. FAILS: black or solid fill (1), the startup logo (4), the
#             boot text page (8).
#
#             THE TWO BANDS ARE COMPLEMENTARY AND NEITHER IS SUFFICIENT ALONE,
#             which is the direct answer to PROTOCOL.md trap 0. nascar's mid-load
#             screen carries 121 distinct colours -- inside the colour band, and
#             a colour-only invariant would pass it -- and is rejected only by its
#             32.8% coverage. A solid-colour palette wipeout is 100% covered and
#             is rejected only by the colour floor. Both are pinned for that
#             reason, and the display class is pinned beside them because a guest
#             that dropped out of the game onto a DOS error page would satisfy
#             both bands and neither is looking at the display path.
#
#   class     The display path, depth, mode and frame geometry, and the stop
#             reason. Fixture constants, graded in Invoke-Fixture rather than
#             from the sidecar because they are not measurements: they are what
#             the fixture IS. They draw the one line the pixel bands cannot --
#             a guest that fell out of the game onto a DOS text page paints a
#             screen that is neither black nor single-coloured.
#
# WHAT IS DELIBERATELY NOT ASSERTED:
#
#   the end-of-budget frame hash. Recorded as `final_frame_sha256`, reported,
#   diffable, never graded. It is the first thing an attribution cycle wants and
#   the last thing a gate should trust.
#
#   `entries`, and every other coverage counter. They are JIT-ARM dependent by
#   construction -- `-Arm off` is meant to move them -- so asserting them would
#   fail every off-arm board. `instructions` is the exception and the reason it
#   is the guest-progress counter chosen here: charging is per instruction and
#   does not care which backend retired it, the same fact the DUKEMARK sample
#   pin rests on (arm off moved duke's guest time by 3 parts in 10,000).
#
#   wall, real-time factor, guest seconds. Untouched by this redesign. They were
#   never invariants on these rows and they are not invariants now; the anchor
#   run is not timed and contributes nothing to them.
#
# COST. One extra emulator invocation per row: 7 s on tombraid-586 against its
# ~250 s, 5 s on nascar-586 against its ~60 s.
#
# The instruction tolerance. 5%, and the derivation is two-sided like every band
# in the sidecar. The legitimate side: across the IOPL-3 change the count at a
# fixed budget moved 0.129% on tombraid and 0.145% on nascar, so 5% is 34x the
# largest legitimate move this class of change has produced; it is also arm-safe,
# because charging is per instruction (arm off moved duke's guest time by 3 parts
# in 10,000). The failing side: it is not vacuous, because the count at a fixed
# budget is a rate, and the rate is a function of WHICH PHASE the run is in --
# tombraid retires 0.697 instructions per cycle over the graded budget but only
# 0.625-0.644 over budgets that end inside the FMV (measured at 5e9 and 15e9), so
# a run that failed to get out of the intro is 8-10% low and fails. A run that
# never launched the game at all misses by far more.
$frameInstructionTolerance = 0.05
# The frame contract of a fixture, or $null for the rows that do not carry one.
#
# Probed by name and guarded, exactly like `cdImage`: under StrictMode
# `$Fixture.PSObject.Properties['frameContract']` is $null on a row without the
# field and reading `.Value` off that THROWS. Doing it inline cost a board run --
# the two contract rows passed and wolf3d-486 died on the property access.
function Get-FrameContract($Fixture) {
    $property = $Fixture.PSObject.Properties['frameContract']
    if ($null -eq $property) { return $null }
    return $property.Value
}

function New-FrameContract {
    param(
        [Parameter(Mandatory)][uint64]$AnchorCycles,
        [Parameter(Mandatory)][string]$AnchorDisplay,
        [int]$AnchorPhases = 1,
        [string]$Display = "MargoLfb",
        [int]$Width = 640,
        [int]$Height = 480,
        [int]$Bpp = 8,
        [string]$Mode = "0x0101"
    )
    @{
        anchorCycles  = $AnchorCycles
        anchorDisplay = $AnchorDisplay
        # How many distinct frames the anchor point is allowed to take. More than
        # one ONLY where the extra states have been identified pixel by pixel and
        # written down above; the number is a cap the record path enforces, so an
        # unexplained new state cannot be absorbed by re-recording.
        anchorPhases  = $AnchorPhases
        display       = $Display
        width         = $Width
        height        = $Height
        bpp           = $Bpp
        mode          = $Mode
    }
}

# ---------------------------------------------------------------------------
# The fixture table. Arguments are copied from .bench/PROTOCOL.md; see the note
# in the .DESCRIPTION above about why they are copied rather than normalised.
# ---------------------------------------------------------------------------

# Grade one profile against a row's `profileBands` (see the Tyrian rows).
# Returns @{ values = <ordered name->number>; failures = <string[]> }. Pure so
# the self-test can drive it red and green without an emulator run.
function Test-ProfileBands($Profile, $Bands, [bool]$CompareCalibration = $true) {
    $values = [ordered]@{}
    $failures = @()
    foreach ($band in $Bands) {
        $value = $Profile
        $resolved = $true
        foreach ($segment in ($band.path -split '\.')) {
            $property = if ($null -ne $value) { $value.PSObject.Properties[$segment] } else { $null }
            if ($null -eq $property) { $resolved = $false; break }
            $value = $property.Value
        }
        if (-not $resolved) {
            $failures += "profile band '$($band.path)': the profile has no such field"
            continue
        }
        # The cast throws under the script's Stop preference when the path
        # resolves to a non-numeric node (a typo'd path that stops one segment
        # short lands on an object). That must be one RED row, not a dead
        # sweep -- a gate that can only crash is a gate that cannot fail.
        $number = $null
        try { $number = [double]$value } catch {
            $failures += "profile band '$($band.path)': the value is not numeric"
            continue
        }
        if ($null -eq $value -or $value -is [string] -or $value -is [bool] -or -not [double]::IsFinite($number)) {
            $failures += "profile band '$($band.path)': the value is not a finite number"
            continue
        }
        $values["band_" + ($band.path -replace '\.', '_')] = $number
        if ($CompareCalibration -and ($number -lt $band.min -or $number -gt $band.max)) {
            $failures += ("profile band '$($band.path)' is {0}, outside [{1}, {2}]" -f
                $number, $band.min, $band.max)
        }
    }
    @{ values = $values; failures = $failures }
}

function Get-FixtureTable {
    @(
        [pscustomobject]@{
            name = "doom-486"; folder = "jemmex_doom_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]32000000000
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
            cycles = [uint64]26560000000
            # Shifted down 19 tics on 2026-08-10 for the same reason as the 486
            # row, band width and margins preserved.
            realticsMinimum = 951; realticsMaximum = 1021; gametics = 2134
            qconsole = $false; resultPpm = $false; injection = @(); dukemark = $null
        }
        [pscustomobject]@{
            name = "quake-586"; folder = "quake_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]24800000000
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
            # 2026-09-02 re-pin, e312f8f3 -> 6cc0d354 (the SAME hash this row pinned before the
            # 2026-08-27 entry below, `90c5e41d` "Re-pin prince-486 and tombraid-loader-586 to
            # the PR 736 kernel", moved back to it). The CR3 data-side gate's PR #826 (T1+T2,
            # `dev_docs/2026-09-02-cr3-data-side-design.md`) shifts the fixed 4e9-cycle budget one
            # torch-flame frame earlier -- the same class of drift `90c5e41d` describes for the
            # opposite direction (156 of 128000 pixels, all inside the two torch sprites). Content
            # is verified by hash identity to that already-inspected frame, not by a fresh read.
            #
            # CAUSE, HONESTLY: T1 alone (`dev_docs/2026-09-02-cr3-data-side-design.md` T1)
            # reproduces the prior `e312f8f3` hash and the prior instruction count UNCHANGED, so
            # T1 is cleared. T1+T2 together move both. Inside T2, three internal mechanisms were
            # tested as the specific cause, individually and in combination -- reverting
            # `Tlb::insert`'s eviction-report predicate to its pre-T2 form, forcing every `MOV
            # CR3` to fully re-flush the TLB instead of retaining it (`ContextSelect::Reselected`
            # forced through `retire_all_slots`), and forcing `flush_tlb_keep_code_caches`'s PG=1
            # arm to fully flush instead of `flush_live_slot` -- and NONE of them, alone or
            # together, moved the hash or the instruction count off `6cc0d354` /
            # `2079676716`. The earlier claim in this row ("T2's retention behavior itself") is
            # RETRACTED as unsupported: the experiment that should have refuted it (forcing full
            # re-flush on the CR3-write gate) left the shifted hash exactly in place, which
            # contradicts that claim rather than confirming it. The specific mechanism inside T2
            # responsible for the shift was not isolated. This does not change the merge decision
            # -- the content question is settled independently by hash identity to a previously
            # shipped, hand-verified frame -- but the causal sentence itself is not asserted.
            #
            # irq0_edges (independent confirmation the shift is a phase shift, not a different
            # execution path): main and T1-only both read 4064 (pit_writes 23); T1+T2 reads 4069
            # (pit_writes 28), +5 edges (+0.12%) against instructions -0.0104%, proportionate and
            # in the direction a slightly earlier-landing budget predicts (a few more timer edges
            # land inside the same fixed cycle budget). Read from `--profile-json`'s `timer` block
            # on both binaries, prince-486's own schedule.
            #
            # 2026-08-30 re-pin, 04fd8558 -> 802e9d4f. The ISA I/O wait-state
            # flip: prince issues only 43 PIT writes all run, so the charge
            # moves it by ppm and the 4e9 budget lands one torch-flame frame
            # off -- the SAME class as this row's two previous re-pins below.
            # The frame was READ before pinning: level-1 dungeon, torches lit,
            # the injected run in progress. (A first capture WITHOUT the key
            # schedule showed the intro and briefly read as an input bug; the
            # schedule is load-bearing -- see the injection comment below.)
            #
            # 2026-08-29 re-pin, e312f8f3 -> 04fd8558. PR #760 (Tier B B3)
            # rebuilt KERNEL.SYS inside tokados-hdd.img (73071 -> 73487 bytes),
            # which shifts the boot phase; the 4e9 budget lands one torch-flame
            # frame off. Proven by a code-identical A/B at 0333d956 with only
            # the image swapped: old image reproduces e312f8f3, new image
            # 04fd8558. 16 of 128000 pixels differ, all inside the left torch
            # sprite at x 47-50 / y 160-169. The #762/#763 merges are NOT the
            # movers (each reproduces the same hash as plain 0333d956).
            #
            # 2026-08-27 re-pin, 6cc0d354 -> e312f8f3. PR #736 (the Toka-DOS FAT
            # prefetch slice) cut the INT 13h count of the load phase, so the
            # 4e9-cycle budget now lands one TORCH-FLAME frame further along.
            # 156 of 128000 pixels differ, every one of them inside the two
            # torch sprites at x 43-82 / y 136-169; the room, the scroll
            # position and the HUD are byte-identical, and both flame frames are
            # clean sprites. Measured against `d2640de0` (last good) and
            # `96882738` (PR #736 merge, first bad); `c1447356` reproduces
            # 96882738's hash, entries and coverage exactly.
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
            # The CHEAP duke3d-586 row: the same guest workload as duke3d-586,
            # stopped early. `.bench/duke3d_short_c` is duke3d_c with one dword
            # rewritten -- BENCH2S.DMO is BENCH2.DMO with its record count cut
            # from 3909 to 1140 -- so the records that play are byte-for-byte
            # the long row's first 1140 and the short row is a PREFIX of the
            # long row's workload rather than a different one. Built by
            # `scripts/make-duke-short-fixture.ps1`; see PROTOCOL.md.
            #
            # It exists because the long row costs ~470 s a leg, which makes a
            # six-leg floor most of an evening. It is NOT a replacement: re-run
            # the long row before any merge decision. This is the row to ladder
            # candidates on.
            name = "duke3d-586-short"; folder = "duke3d_short_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # Same guard role as the two long duke rows: 33.2e9 is 200 guest
            # seconds at 166 MHz against the ~60 where EXITVM actually fires.
            cycles = [uint64]33200000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $false; injection = @()
            dukemark = (New-DukemarkPins -Demo "BENCH2S")
        }
        [pscustomobject]@{
            name = "nascar-586"; folder = "nascar1_c"
            # No --video: PROTOCOL.md's recorded invocation omits it and the
            # invariants were measured that way.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]4980000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @(); dukemark = $null
            # The end-of-budget frame lands mid-attract-demo with the camera in
            # flight, so it is graded on bands; the exact-frame invariant moved
            # to the static startup logo at 0.445e9. See New-FrameContract.
            frameContract = (New-FrameContract -AnchorCycles ([uint64]445000000) `
                    -AnchorDisplay "MargoLfb")
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
        [pscustomobject]@{
            # Tyrian 2000 SETUP.EXE: the settings menu, then the jukebox. The
            # row exists for the guest's 70 Hz audio clock -- the Loudness
            # driver paces music (MPU-401 MIDI at P300) and its DSP re-arm
            # chain off a PIT channel 0 it reprograms every video frame, and
            # the 2026-08-28 write-edge bug silenced exactly this screen.
            # Schedule at 66 M cycles/guest-second: the menu is stable by 4 s;
            # five {down}s walk the cursor to Jukebox at ~25 s, {enter} at
            # ~27 s, {esc} back to the menu at ~59 s. The end frame is the
            # static settings menu, so it takes an exact hash.
            #
            # The bands are liveness floors, not cadence pins. Measured on the
            # fixed tree (854237ed, 486, 71.2 guest s, scoreboard-20260829-
            # 001320): irq0_edges 4830 (70/s steady), MIDI data_writes 10823,
            # DSP command bytes 14345. The broken parent reads irq0 ~100
            # (edges stop at 3.5 s), MIDI 5237 (menu phases silent), DSP
            # 6803. irq0 is the PRIMARY discriminator (~35x separation); the
            # MIDI and DSP floors sit between the arms with thinner margins
            # (broken 5237 / floor 7000 / fixed 10823, and 6803/8500/14345)
            # and exist to catch a collapse the irq0 count alone cannot see,
            # e.g. music dead with the timer alive.
            name = "tyrian-setup-486"; folder = "tyrian_setup_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]4700000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            injection = @("--inject-keys", ("1670000000:{down};1690000000:{down};" +
                "1710000000:{down};1730000000:{down};1750000000:{down};" +
                "1800000000:{enter};3900000000:{esc}"))
            profileBands = @(
                @{ path = "timer.irq0_edges"; min = 3500; max = 7000 }
                @{ path = "mpu.wavetable.data_writes"; min = 7000; max = 21000 }
                @{ path = "sb_dsp.command_bytes"; min = 8500; max = 26000 }
            )
        }
        [pscustomobject]@{
            # Tyrian 2000 gameplay: title -> Start New Game -> 1 Player Full
            # Game -> episode -> difficulty -> station menu -> Start Level,
            # then the left mouse button HELD from 31 s so the ship fires
            # through the first waves. The ship dies at ~53 s under this
            # schedule; the 3.2e9 budget (~48.5 guest s, ~17.5 s of play)
            # ends the run safely inside gameplay. The end frame animates, so
            # the row keeps no frame artifact at all and grades on bands.
            # Same 70 Hz-clock liveness rationale as tyrian-setup-486.
            name = "tyrian-486"; folder = "tyrian_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]3200000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $false; dukemark = $null
            injection = @(
                "--inject-keys", ("1056000000:{enter};1188000000:{enter};" +
                    "1320000000:{enter};1452000000:{enter};1650000000:{down};" +
                    "1670000000:{down};1690000000:{down};1710000000:{down};" +
                    "1780000000:{enter};1910000000:{enter}"),
                "--inject-mouse", "2050000000:down")
            profileBands = @(
                @{ path = "timer.irq0_edges"; min = 2400; max = 4800 }
                @{ path = "mpu.wavetable.data_writes"; min = 3000; max = 20000 }
                @{ path = "sb_dsp.command_bytes"; min = 4000; max = 25000 }
            )
        }
        [pscustomobject]@{
            # The same gameplay run at 586, the persona the owner reports at
            # ~10% realtime under load: this is the PERF row of the pair.
            # Offsets are the 486 row's guest-second schedule at 166 M
            # cycles/guest-second; the menus wait on input, so keys landing
            # later than the screen appears is safe.
            name = "tyrian-586"; folder = "tyrian_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]8050000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $false; dukemark = $null
            injection = @(
                "--inject-keys", ("2656000000:{enter};2988000000:{enter};" +
                    "3320000000:{enter};3652000000:{enter};4117000000:{down};" +
                    "4167000000:{down};4216000000:{down};4266000000:{down};" +
                    "4482000000:{enter};4814000000:{enter}"),
                "--inject-mouse", "5163000000:down")
            profileBands = @(
                @{ path = "timer.irq0_edges"; min = 2400; max = 4800 }
                @{ path = "mpu.wavetable.data_writes"; min = 3000; max = 20000 }
                @{ path = "sb_dsp.command_bytes"; min = 4000; max = 25000 }
            )
        }
        [pscustomobject]@{
            # The Tyrian 2000 Ship Specs dwell, the recipe the 09-02/03 CR3-gate
            # and reflected-call campaigns are graded on
            # (dev_docs/2026-09-02-tyrian-586-specs-diag.md,
            # dev_docs/2026-09-02-tyrian-586-reprofile.md,
            # dev_docs/2026-09-03-morning-handoff.md). `tyrian-586` above polls
            # 62x less than this screen and cannot show the lever the campaign
            # works on -- this row exists to be graded on instead.
            #
            # The owner's install tree, not the plain `tyrian_c` fixture: the
            # owner's CONFIG.SYS (`DEVICE=C:\DOS\TOKAEMM.SYS RAM /T`,
            # `DOS=HIGH,UMB`), the owner's AUTOEXEC (`SET BLASTER=A220 I7 D1 H5
            # P300 T6`, `LH TOKAMOUS /T`, `SNDCTRL /B /T`), and the owner's
            # TYRIAN.CFG / TYRIAN.SAV (save slot 1 = MicroCorp Stalker-B).
            # Fixture tree at `.bench/tyrian_specs_c`, copied verbatim from the
            # throwaway harness at `D:\ctd\tyr586\src`.
            #
            # Schedule (guest cycles at 166 MHz = guest seconds x 166e6): title
            # menu is stable by t=12s; {down}+{enter} at 14.0/14.5 opens Load
            # Game; {enter} at 16.5 loads save 1; {down}+{enter} at 19.0/19.5
            # opens Ship Specs. The picture is static from t=20.5s onward (the
            # screen draws nothing; the guest busy-polls for a keypress) and the
            # graded window in the diagnostic docs is t=21.0-31.0, ten guest
            # seconds of dwell. The 5.15e9 cycle budget ends the run at
            # t=31.024s, inside that static window, so the end frame IS the
            # dwell picture.
            #
            # Reference rates from the campaign's own re-profile
            # (2026-09-02-tyrian-586-reprofile.md, dwell t=21-31s on the PR #820
            # binary): 159.7 M guest instructions per guest second, and the
            # 2026-09-03 morning handoff reports 17.7 M dispatcher entries per
            # guest second after the #825/#826 CR3 gate. Measured on this row
            # at main 29a7b6dd (this branch's parent): 160.6 M instructions/gs
            # and 18.48 M entries/gs over the same t=21-31s window (phase marks,
            # IZARRAVM_PHASE_INTERVAL_MS=500) -- the same ballpark, moved by the
            # commits between #825 and 29a7b6dd. These are NOT asserted by this
            # row (no periodic phase sampling runs on a board leg); they are the
            # reason the row exists and are recorded here so a future slice can
            # compare its own dwell rate against this baseline.
            #
            # PROVISIONAL: no rt anchor. `realticsMinimum`/`Maximum` are $null
            # below and nothing wall-clock is asserted, on purpose -- the box
            # was not quiet while this row was built (background load 61-66%
            # on two of the three legs that measured it), and a wall-clock
            # anchor recorded under load is not a final pin. Re-anchor rt once
            # the box is quiet, if an rt anchor for this row is wanted at all;
            # the frame hash and the counters above are the real invariant.
            name = "tyrian-specs-586"; folder = "tyrian_specs_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = [uint64]5150000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            injection = @("--inject-keys", ("2324000000:{down};2407000000:{enter};" +
                "2739000000:{enter};3154000000:{down};3237000000:{enter}"))
            # The Ship Specs screen is READ, not assumed: the picture behind
            # this hash is MicroCorp Stalker-B's data page -- wireframe ship,
            # the "MicroSol continues the Stalker line..." body text, the stat
            # block, and the "Press a key" prompt the screen always shows while
            # it dwells -- confirmed by eye against `--presented-ppm` before the
            # hash was recorded. Two fresh runs from the fixture tree are
            # frame-identical (this hash, twice) and read the identical pixel
            # stats printed to stdout: 29,000 non-zero pixels (22.7%), 19
            # distinct colours, top colours #000000/#104110/#04280C/#04A60C/
            # #18DB3C.
            #
            # `gradePresentedFrame`, not a re-render: this screen is exactly the
            # shape PROTOCOL.md warns about (a defect could fill video memory
            # and never publish it), so the row grades what the scanout
            # actually presented.
            gradePresentedFrame = $true
            frame_sha256_allowed = @(
                "87e8f37c171de793c62bc4a1604e15a576cf467c2df2c95cd00f4c18fee3aee0"
            )
            stdout_contains = "video mode: mode 13h (320x200x256)"
            expected_display = "VgaRaster"; expected_video_mode = "Mode13h"
            expected_width = 320; expected_height = 400
        }
        [pscustomobject]@{
            name = "tombraid-loader-586"; folder = "tombraid_loader_c"
            arguments = @("--cpu", "586", "--memory-mib", "64")
            cycles = [uint64]500000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @(); dukemark = $null
            # The graded frame is the Toka-DOS boot screen, and its title line is
            # `TOKA_BUILD_LINE_1` (toka-dos/freedos/kernel/hdr/version.h:61-63),
            # which ends in __DATE__. Every KERNEL.SYS rebuild therefore moves
            # this hash whether or not anything else changed, and the move is
            # NINETY-ODD PIXELS of that one date string.
            #
            # 8bef41b4 is `2c55fef0` with "Compiled Aug  7 2026" repainted as
            # "Compiled Aug 26 2026". Measured 2026-08-27 against `96882738`
            # (PR #736, the Toka-DOS FAT prefetch slice, KERNEL.SYS 71084 ->
            # 88603): 79 of 288000 pixels differ, all inside rows 178-187, and
            # the parent `d2640de0` still hashes 2c55fef0. `c1447356` (current
            # main) reproduces 8bef41b4 with entries and coverage IDENTICAL to
            # 96882738, so nothing merged after PR #736 touches this row.
            #
            # REPINNED 2026-08-28 to e446305c alone: IzarraCD CD-2 replaced the
            # IZCDEX install line with the kernel's own claim line on the boot
            # screen this anchor contains. Bit-identical ON and OFF
            # IZARRAVM_TEST_WORD_ROWS on this binary (hash AND 18,136,142,698
            # retired instructions).
            #
            # REPINNED 2026-08-30 to 075cc2bb alone. The dead TOKACD/IZCDEX
            # lines were removed from all four hand-made fixture trees: both
            # `DEVICEHIGH=C:\DOS\TOKACD.SYS` and `IZCDEX /I /D:TOKACD01 /L:D /T`
            # named files that stopped shipping with the IzarraCD consolidation
            # in PRs #755/#756, so the boot screen this row GRADES carried three
            # lines of CONFIG.SYS error plus one `Bad command or filename` that
            # should never have been there. Four text rows leave the page;
            # 8,251 of 288,000 pixels differ, all in rows 272-382, and the CD
            # still reaches D: because the BIOS serves it.
            #
            # The move is legitimate and was established, not assumed: two
            # sessions reproduced 075cc2bb independently on two different
            # binaries, and the frame was READ before it was hashed rather than
            # trusted because it was stable.
            frame_sha256_allowed = @(
                "075cc2bb62c055d6be98a3cc6a7c9076de07d197386622d11cde487ac36b0901"
            )
            stdout_contains = "DOS/4GW Protected Mode Run-time  Version 1.97"
            expected_display = "VgaRaster"; expected_video_mode = "Text"
            expected_width = 720; expected_height = 400
        }
        [pscustomobject]@{
            name = "tombraid-586"; folder = "tombraid_c"
            # 586 ONLY: the game needs a Pentium+FPU; the 486 persona cannot
            # hold it. Software renderer. The run covers the CD-streamed RPL
            # intro FMV, the title menu, and the demo the menu starts by
            # itself after idling - no input schedule needed.
            arguments = @("--cpu", "586", "--memory-mib", "64")
            # 28e9 = 169 guest seconds. Measured timeline (phase marks,
            # .bench/results/tombraid-bringup/timeline-20260814-231557):
            # boot 0-3, CD-streamed FMV 3-125, title menu 126-141 (the menu
            # starts the demo itself), level load 142-144, demo 145-179. The
            # end frame lands MID-DEMO on purpose: it is where the fixture's
            # real work is, and 30e9 would land on the demo-to-menu transition
            # second. It is graded on BANDS, not on a hash -- mid-demo is
            # precisely where a hash cannot survive a cadence change.
            cycles = [uint64]28000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @(); dukemark = $null
            # The exact-frame invariant moved to the DOS/4GW banner page at
            # 0.5e9, whose only moving part is the two-state text cursor.
            # See New-FrameContract.
            frameContract = (New-FrameContract -AnchorCycles ([uint64]500000000) `
                    -AnchorDisplay "VgaRaster" -AnchorPhases 2)
            # The disc is REQUIRED: FMV, CD audio, and the game's CD check.
            # Mounted read-only from the shared tree, never copied per run.
            cdImage = "tombraid_cd\tombeng.cue"
        }
        [pscustomobject]@{
            name = "tombraid3d-586"; folder = "tombraid3d_c"
            # The 3dfx build of the same game, next to the software row on
            # purpose: same engine, same disc, same sound configuration, and the
            # ONLY difference is that every pixel comes through Glide and the
            # Distira rasteriser instead of the CPU. A regression that moves one
            # of the two rows and not the other says which half it is in.
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # 19e9 = 114.5 guest seconds. Measured timeline with this schedule:
            # boot 0-4, Glide splash 5-9, a black wait 9-24, title 35-50, attract
            # DEMO 50-85, title 85-100, DEMO 100-130, and so on. The budget lands
            # 14 seconds into the SECOND demo, so the run holds one COMPLETE demo
            # plus half of another and the end frame sits 16 seconds clear of the
            # nearest transition. Real-time factor 0.87, 132 s wall.
            cycles = [uint64]19000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            # ONE Escape. The 3dfx build sits on a black screen after the splash
            # and waits for a key; the software build does not, which is why THAT
            # row carries no schedule. MEASURED: the game ignores input until
            # between 3.5e9 and 4e9 cycles, and any key from 4e9 to at least 6e9
            # works, so 5e9 keeps about a quarter of the window in hand on the
            # early side and has no bound on the late side. Keep it to ONE key:
            # a second lands on the title, opens the ring menu, and the attract
            # demo then never starts. That was measured too, not assumed.
            injection = @("--inject-keys", "5000000000:{esc}")
            # A Glide row MUST grade the PUBLISHED frame. Distira's scanout is
            # the only path that shows it, and a re-render reports what video
            # memory holds -- which on a double-buffered Voodoo is the buffer
            # nobody is looking at.
            gradePresentedFrame = $true
            # No end-of-budget hash, for the reason tombraid-586 lost its own:
            # the end frame is mid-demo, where any cadence-adjacent change moves
            # it legitimately. Two repeat runs from a fresh copy are
            # bit-identical (99.57% non-black, 365 colours, 19,864,778,122
            # instructions), so the determinism is real; it is robustness to CODE
            # change that a hash would lack. The anchor is the Toka-DOS boot text
            # at 0.5e9, whose only moving part is the two-state cursor -- the
            # same anchor and the same two phases as the software row, because
            # both trees boot the same DOS and their first four guest seconds are
            # byte-identical to each other.
            frameContract = (New-FrameContract -AnchorCycles ([uint64]500000000) `
                    -AnchorDisplay "VgaRaster" -AnchorPhases 2 `
                    -Display "Distira" -Width 640 -Height 480 -Bpp 16 -Mode "Text")
            # The SAME disc the software row mounts: same game, same pressing, so
            # a second 643 MB copy would buy nothing.
            cdImage = "tombraid_cd\tombeng.cue"
        }
        [pscustomobject]@{
            name = "descent2-3dfx-586"; folder = "descent2_c"
            # The second Glide row, and not a duplicate of the first: it ships
            # the BYTE-IDENTICAL glide2x.ovl (md5 341b8f5d82daa46fd1ce2363...)
            # and drives it far harder. Where Tomb Raider runs at rt 0.87, this
            # runs at 0.32 -- six-degree-of-freedom geometry, per-pixel lighting
            # and a rear-view viewport, all through the same rasteriser. It is
            # the heavier of the two and the one a Distira regression should
            # reach first.
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # 9e9 = 54.2 guest seconds. Boot 0-4, Glide splash 5-10, a release
            # notice at 23-24, and the recorded demo from 29 onward, still
            # running past 170. The budget lands 25 seconds into the demo, at
            # "PLAYBACK (33% DONE)", deep inside the phase and far from either
            # end of it. 170 s wall.
            cycles = [uint64]9000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; dukemark = $null
            # The AUTOEXEC passes `-nomovies -autodemo`. `-nomovies` skips the
            # Interplay MVE intro, which is why the fixture tree does not carry
            # the three .MVL files at all: they are 220 MB of the source tree's
            # 266 MB and this row never opens them. `-autodemo` starts
            # DEMOS\DESCENT2.DEM from the title with no further input.
            #
            # The two Escapes clear the release-notice screen. A THIRD key would
            # land after the demo starts and raise "ABORT AUTODEMO?" -- measured,
            # not guessed.
            injection = @("--inject-keys", "3000000000:{esc};4000000000:{esc}")
            gradePresentedFrame = $true
            # Same argument as the row above. Two repeat runs from a fresh copy
            # are bit-identical (83.09% non-black, 834 colours, 8,579,000,326
            # instructions).
            frameContract = (New-FrameContract -AnchorCycles ([uint64]500000000) `
                    -AnchorDisplay "VgaRaster" -AnchorPhases 2 `
                    -Display "Distira" -Width 640 -Height 480 -Bpp 16 -Mode "Text")
            # REQUIRED: the game reads Redbook audio and its own CD check from
            # the disc. 691 MB, mounted read-only, never copied per run.
            cdImage = "descent2_cd\DESCENT_II.cue"
        }
        [pscustomobject]@{
            name = "psycho-486"; folder = "psycho_c"
            # 486 / 64 MiB / Vega, the machine the fault was reported on. A 1995
            # game does not need 166 MHz, and at 66 MHz the same guest time costs
            # a third of the cycles.
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            # 8e9 clocks is 121 guest seconds: past the language menu, the title,
            # the attract tables and the load, and roughly 60 seconds into a
            # table in play. Gameplay is where this row earns its place.
            cycles = [uint64]8000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            qconsole = $false; resultPpm = $true; injection = @(); dukemark = $null
            # THE POINT OF THIS ROW. It grades the PUBLISHED frame, not a
            # re-render, because the defect it exists to catch is invisible to a
            # re-render. With the `resize_work` raster wipe restored, the same
            # run reads 82.9% non-black and 127 colours through --result-ppm and
            # 0.0% / 1 colour through --presented-ppm. No other row in this table
            # would have failed.
            #
            # WHAT IT CATCHES, both arms measured by restoring the defect and
            # re-running rather than by argument:
            #   raster wipe restored -> FAIL, "non-black coverage % is 0, outside
            #     the band [60, 95]" plus "distinct colours is 1".
            #   mode X CRTC reseed restored -> PASS. It does NOT catch that one.
            #     The reseed only damages the FIRST mode set, which is the menu
            #     phase; by the budget the game has re-entered its gameplay mode
            #     from inside mode X, so the geometry is 320x368 either way. A
            #     menu-phase anchor cannot cover it either, because the menu
            #     animates continuously. Geometry regressions are covered by the
            #     video-crate unit tests instead, which is where they belong.
            gradePresentedFrame = $true
            # No end-of-budget HASH, for the reason Duke3D and Tomb Raider lost
            # theirs: the picture animates continuously, so any cadence-adjacent
            # change moves it legitimately. Three repeat runs are bit-identical
            # (82.92%, 127 colours, 3734683259 instructions), so the determinism
            # is real; it is robustness to CODE change that a hash would lack.
            #
            # There is no early static anchor either, and that was measured
            # rather than assumed: at 250 ms sampling over the first 25 guest
            # seconds the ONLY run of four identical frames is the DOS boot text
            # at 250-1000 ms, which exercises no graphics at all.
            # The anchor is the Toka-DOS boot text at 0.6 guest seconds, the one
            # place in this title's first 25 seconds where four consecutive
            # 250 ms samples are bit-identical. It pins boot determinism only;
            # the graphics evidence is the content bands at the budget.
            frameContract = (New-FrameContract -AnchorCycles ([uint64]40000000) `
                    -AnchorDisplay "VgaRaster" -AnchorPhases 1 `
                    -Display "VgaRaster" -Width 320 -Height 368 -Bpp 8 -Mode "ModeX")
        }
        [pscustomobject]@{
            name = "mojo-586"; folder = "mojo_c"
            # THE BOARD-IDENTITY ROW, and the cheapest row in the table by an
            # order of magnitude: 0.73 s of wall against descent2's 170.
            #
            # Every other Distira row grades PICTURES. A picture is a very
            # indirect witness to what the board says it IS: Tomb Raider and
            # Descent II would render identically whether the SST-1 reports one
            # TMU or two, 2 MB or 4 MB, and neither would notice if the vendor
            # ID moved. MOJO.EXE is 3dfx's own DOS diagnostic for the SST-1 --
            # `usage: mojo [-v]`, no menus, no keypress, no graphics -- and it
            # reads exactly those facts out of the hardware and prints them.
            # Bare MOJO prints the board report (vendor/device ID, FBI revision
            # and memory, FBI and TMU power-on sense, DAC colour format, SLI
            # state, TMU revision, TMU count and per-TMU memory); `-v` prints
            # the SST-1 register file INSTEAD of it, so the AUTOEXEC runs both
            # and the row pins both.
            #
            # It also reaches the board a different way than a game does. The
            # per-TMU memory line is not read from any register: MOJO sizes
            # texture memory by writing and reading back through the texture
            # aperture until it aliases, so this row grades the APERTURE BOUNDS,
            # which nothing else in the table touches. The TMU config byte is
            # read the way real silicon delivers it, by setting trexInit1's
            # sendConfig bit and reading the config byte back as a rendered
            # pixel through the LFB -- a path with no other coverage here.
            #
            # The fixture tree is built by scripts/make-mojo-fixture.ps1, which
            # hard-fails on the sha256 of mojo.exe: the pins below are pins on
            # ONE build of one diagnostic, and a different build would fail the
            # row for a reason that has nothing to do with Distira.
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            # A GUARD, not the length of the run -- EXITVM.COM ends the VM once
            # both reports are written, at roughly 0.55e9 clocks. 4e9 is 24 guest
            # seconds and about seven times the headroom needed.
            cycles = [uint64]4000000000
            realticsMinimum = $null; realticsMaximum = $null; gametics = $null
            # No frame to grade: the run ends at the DOS prompt in text mode with
            # both reports redirected to files, and MOJO opens no graphics mode
            # at all. The reports ARE the invariant.
            qconsole = $false; resultPpm = $false; injection = @(); dukemark = $null
            # No key injection, and that is a property of the tool rather than a
            # choice: MOJO takes its whole configuration from the command line
            # and never waits for input. A DOS diagnostic that needs no schedule
            # is the rare case; see dos-3d-title-waiting-looks-hung for the rule
            # it is the exception to.
            # MOJO.TXT is pinned raw -- every line of it is a board-identity
            # fact and none of them may drift benignly. MOJOV.TXT needs the
            # beam-phase lines masked, and the mask was earned rather than
            # assumed: Distira synthesises `vRetrace` from the live beam
            # position, so it reports whatever scanline the raster was on
            # when the program asked. Rebuilding this very fixture tree with
            # two unused binaries removed moved it 0x29 -> 0x14 and moved
            # NOTHING else in the file.
            #
            # 2026-09-04 (dev_docs/2026-09-04-red-pins-bisect.md): the same
            # beam phase also leaked through `status:` (distira.rs
            # status_value() clears bit 0x40 while in_vretrace(), so
            # 0ffff07f <-> 0ffff03f tracks vRetrace exactly) and through
            # MOJO's own decode of that bit on the line beneath it. All
            # three lines carry the identical fact, so all three are masked
            # together now. Left unmasked it would be a phase-sensitive hash
            # of exactly the kind Duke3D, Tomb Raider and NASCAR each had to
            # give up; masked, the remaining ~37 lines of the SST-1 register
            # file stay under an exact pin.
            textResults = @{
                exitCode = 0x51
                files    = @("MOJO.TXT", "MOJOV.TXT")
                masks    = @{ "MOJOV.TXT" = @(
                    '^\s*vRetrace:',
                    '^\s*status:',
                    ':\s*vertical retrace\s*$'
                ) }
            }
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
    $null = Get-FixtureFilePaths $SourcePath
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
.SYNOPSIS
Hash a guest-written text report for a `textResults` row, with named lines
masked out.

.DESCRIPTION
Lines are joined with LF regardless of what the guest wrote, so a fixture whose
AUTOEXEC gains a line does not move the pin through DOS line endings, and any
line matching one of `$Masks` is replaced by `<masked>` before hashing.

THE MASK IS NOT A CONVENIENCE. It was earned: MOJO's `-v` register dump prints
`vRetrace`, which on Distira is a LIVE beam position rather than a stored
register, so it reads whatever scanline the raster happened to be on when the
program asked. Rebuilding the fixture tree with two unused binaries removed --
a change that cannot touch the SST-1 at all -- moved that one line from 0x29 to
0x14 and nothing else in the file. Pinned raw, `MOJOV.TXT` would be a
phase-sensitive hash of exactly the kind Duke3D, Tomb Raider and NASCAR each
had to give up; masked, the other 40 lines of the register file stay under an
exact pin.

The masked hash is therefore NOT the sha256 of the file on disk, and the file
itself is archived beside the profile so the difference is inspectable.
#>
function Get-TextResultSha256([string]$Path, [string[]]$Masks) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $lines = @(Get-Content -LiteralPath $Path)
    if ($null -ne $Masks -and $Masks.Count -gt 0) {
        $lines = @($lines | ForEach-Object {
            $line = $_
            foreach ($mask in $Masks) {
                if ($line -match $mask) { return "<masked>" }
            }
            return $line
        })
    }
    $bytes = [Text.Encoding]::UTF8.GetBytes(($lines -join "`n"))
    $stream = [IO.MemoryStream]::new($bytes)
    try {
        return (Get-FileHash -InputStream $stream -Algorithm SHA256).Hash.ToLowerInvariant()
    } finally { $stream.Dispose() }
}

function Test-Sha256Allowed($Allowed, [string]$Actual) {
    $normalised = @($Allowed)
    if ($normalised.Count -eq 0) { throw "allowed SHA-256 set is empty" }
    foreach ($hash in $normalised) {
        if ($hash -isnot [string] -or $hash -notmatch '^[0-9a-fA-F]{64}$') {
            throw "allowed SHA-256 set contains an invalid value"
        }
    }
    return $normalised -contains $Actual
}

<#
Frame CONTENT accounting for the band invariants: how much of the picture is not
pure black, and how many distinct colours it carries.

Computed HERE, from the PPM, rather than scraped from the emulator's stdout
summary, for two reasons. The numbers have to be produced by the same code the
negative probe runs against -- a band graded on a line of stdout cannot be tested
against a synthetic corrupt frame without also faking the stdout. And the stdout
summary is a human-readable diagnostic whose format is free to move; an invariant
that silently stops parsing is an invariant that silently stops failing.

The values agree with the emulator's own summary to the pixel, which is what
lets the bands below be derived from numbers recorded in the attribution
artifacts (those carry stdout, not always a re-runnable frame).

`non-black` and not `non-zero-luma`: a pixel counts when any channel is set.
That is the emulator's own definition, so the two accountings stay comparable.
#>
function Get-PpmFrameStats([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 16) { return $null }

    # P6 header: magic, width, height, maxval, then ONE whitespace byte and the
    # raw triples. Comments are legal between tokens even though this writer does
    # not emit them, so the tokenizer honours them rather than assuming.
    $cursor = 0
    $tokens = @()
    while ($tokens.Count -lt 4 -and $cursor -lt $bytes.Length) {
        $ch = [char]$bytes[$cursor]
        if ($ch -eq '#') {
            while ($cursor -lt $bytes.Length -and $bytes[$cursor] -ne 10) { $cursor++ }
            continue
        }
        if ([char]::IsWhiteSpace($ch)) { $cursor++; continue }
        $start = $cursor
        while ($cursor -lt $bytes.Length -and -not [char]::IsWhiteSpace([char]$bytes[$cursor]) ) {
            $cursor++
        }
        $tokens += [Text.Encoding]::ASCII.GetString($bytes, $start, $cursor - $start)
    }
    if ($tokens.Count -lt 4 -or $tokens[0] -ne "P6") { return $null }
    $cursor++  # the single whitespace byte that terminates the maxval token

    $width = 0; $height = 0; $maxValue = 0
    if (-not [int]::TryParse($tokens[1], [ref]$width) -or
        -not [int]::TryParse($tokens[2], [ref]$height) -or
        -not [int]::TryParse($tokens[3], [ref]$maxValue)) { return $null }
    if ($width -le 0 -or $height -le 0 -or $maxValue -ne 255) { return $null }
    $pixels = [int64]$width * $height
    if ($bytes.Length - $cursor -ne $pixels * 3) { return $null }

    $distinct = [Collections.Generic.HashSet[int]]::new()
    $nonBlack = 0
    for ($i = 0; $i -lt $pixels; $i++) {
        $at = $cursor + $i * 3
        # [int] casts, and they are load-bearing. PowerShell's shift operators
        # keep the LEFT OPERAND'S TYPE, so `[byte]65 -shl 16` is not 4259840, it
        # is 0 -- the result wraps back into a byte. Packing the channels without
        # these casts silently collapses every colour key to its blue channel,
        # and the count becomes "distinct blue values": 49 where the frame holds
        # 174 colours. Caught 2026-08-18 by the tombraid row FAILING its own new
        # band on a correct frame, which is the fixture working.
        $r = [int]$bytes[$at]; $g = [int]$bytes[$at + 1]; $b = [int]$bytes[$at + 2]
        if (($r -bor $g -bor $b) -ne 0) { $nonBlack++ }
        $null = $distinct.Add(($r -shl 16) -bor ($g -shl 8) -bor $b)
    }

    return [pscustomobject][ordered]@{
        width           = $width
        height          = $height
        pixels          = $pixels
        non_black       = $nonBlack
        non_black_pct   = [math]::Round(100.0 * $nonBlack / $pixels, 3)
        distinct_colors = $distinct.Count
    }
}

# Frame-stats self-test. It exists because the colour counter was WRONG on its
# first outing in a way no reviewer caught and no crash reported: the channel
# packing dropped red and green (see the [int] casts above), so it counted
# distinct BLUE values and read 49 where the frame held 174. What found it was
# the tombraid row failing its own new band on a perfectly good frame. These
# cases make that failure mode a test rather than an incident, and they are also
# the negative probe in permanent form -- a black screen, a solid fill and a
# two-colour wipeout are the three shapes the bands exist to reject.
function New-SelfTestPpm([string]$Path, [int]$Width, [int]$Height, [scriptblock]$Pixel) {
    $header = [Text.Encoding]::ASCII.GetBytes("P6`n$Width $Height`n255`n")
    $body = [byte[]]::new($Width * $Height * 3)
    for ($i = 0; $i -lt $Width * $Height; $i++) {
        $rgb = & $Pixel $i
        $body[$i * 3] = $rgb[0]; $body[$i * 3 + 1] = $rgb[1]; $body[$i * 3 + 2] = $rgb[2]
    }
    [IO.File]::WriteAllBytes($Path, ($header + $body))
}

function Assert-ScoreboardFrameStatsSelfTest {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-frame-selftest-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    try {
        # Four colours that share a blue channel and differ only in red/green.
        # Under the packing bug this frame read as ONE colour; it must read four.
        $shared = Join-Path $directory "shared-blue.ppm"
        New-SelfTestPpm $shared 4 1 {
            param($i)
            @(@(10, 20, 77), @(30, 20, 77), @(10, 40, 77), @(30, 40, 77))[$i]
        }
        $sharedStats = Get-PpmFrameStats $shared
        Assert-ScoreboardSelfTestEqual $sharedStats.distinct_colors 4 `
            "colours differing only in red and green"
        Assert-ScoreboardSelfTestEqual $sharedStats.non_black 4 "all-non-black count"
        Assert-ScoreboardSelfTestEqual $sharedStats.non_black_pct 100.0 "all-non-black percent"

        # Black is all three channels zero, and only that. A pixel with a single
        # channel set counts as painted.
        $black = Join-Path $directory "black.ppm"
        New-SelfTestPpm $black 4 1 { param($i) @(@(0, 0, 0), @(0, 0, 0), @(0, 0, 1), @(0, 0, 0))[$i] }
        $blackStats = Get-PpmFrameStats $black
        Assert-ScoreboardSelfTestEqual $blackStats.non_black 1 "one painted pixel"
        Assert-ScoreboardSelfTestEqual $blackStats.non_black_pct 25.0 "quarter coverage"
        Assert-ScoreboardSelfTestEqual $blackStats.distinct_colors 2 "black plus one colour"

        $solid = Join-Path $directory "solid.ppm"
        New-SelfTestPpm $solid 8 8 { param($i) @(85, 93, 93) }
        $solidStats = Get-PpmFrameStats $solid
        Assert-ScoreboardSelfTestEqual $solidStats.non_black_pct 100.0 "solid fill coverage"
        Assert-ScoreboardSelfTestEqual $solidStats.distinct_colors 1 "solid fill colours"

        Assert-ScoreboardSelfTestEqual $solidStats.width 8 "parsed width"
        Assert-ScoreboardSelfTestEqual $solidStats.height 8 "parsed height"

        # A truncated frame is not a frame. It must return $null so the row FAILS
        # rather than grading a partial picture.
        $truncated = Join-Path $directory "truncated.ppm"
        $bytes = [IO.File]::ReadAllBytes($solid)
        [IO.File]::WriteAllBytes($truncated, $bytes[0..($bytes.Length - 40)])
        if ($null -ne (Get-PpmFrameStats $truncated)) {
            throw "scoreboard self-test failed: a truncated PPM parsed as a frame"
        }
        if ($null -ne (Get-PpmFrameStats (Join-Path $directory "absent.ppm"))) {
            throw "scoreboard self-test failed: a missing PPM parsed as a frame"
        }

        # And the bands themselves, against the shapes they exist to reject. The
        # numbers are the recorded tombraid-586 band; the point is that the three
        # broken-frame shapes all land outside it and a real frame's numbers land
        # inside.
        $low = 89.0; $high = 100.0; $colourLow = 79; $colourHigh = 256
        foreach ($case in @(
                @{ label = "all-black screen"; pct = 0.0; colours = 1 }
                @{ label = "solid fill"; pct = 100.0; colours = 1 }
                @{ label = "two-colour wipeout"; pct = 100.0; colours = 2 }
                @{ label = "wrong-scene FMV frame"; pct = 24.837; colours = 994 }
            )) {
            $inside = $case.pct -ge $low -and $case.pct -le $high -and
                $case.colours -ge $colourLow -and $case.colours -le $colourHigh
            if ($inside) {
                throw ("scoreboard self-test failed: the tombraid band accepts a " +
                    "$($case.label)")
            }
        }
        $realFrame = @{ pct = 99.588; colours = 174 }
        if (-not ($realFrame.pct -ge $low -and $realFrame.pct -le $high -and
                $realFrame.colours -ge $colourLow -and $realFrame.colours -le $colourHigh)) {
            throw "scoreboard self-test failed: the tombraid band rejects its own measured frame"
        }
    } finally {
        Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
    }
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

function Get-FixtureOption($Fixture, [string]$Name) {
    $property = $Fixture.PSObject.Properties[$Name]
    if ($null -ne $property) { return $property.Value }
    return $null
}

function Get-ContainedPath([string]$Root, [string]$Relative) {
    if ([IO.Path]::IsPathRooted($Relative) -or $Relative -match '[\x00-\x1f:]' -or
        @($Relative -split '[/\\]' | Where-Object { $_ -in @('', '.', '..') -or $_ -match '[. ]$' }).Count) {
        throw "Invalid fixture path: '$Relative'"
    }
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $path = [IO.Path]::GetFullPath((Join-Path $rootPath $Relative))
    if (-not $path.StartsWith($rootPath + [IO.Path]::DirectorySeparatorChar,
            [StringComparison]::OrdinalIgnoreCase)) { throw "Path leaves fixture root: $path" }
    return $path
}

function Get-FixtureFilePaths([string]$Root, [string[]]$RelativeFiles = @()) {
    $rootPath = [IO.Path]::GetFullPath($Root)
    $paths = [Collections.Generic.List[string]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    if ($RelativeFiles.Count) {
        foreach ($relative in $RelativeFiles) {
            $path = Get-ContainedPath $rootPath $relative
            Assert-RegularFixturePath $rootPath $relative
            if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::Directory) {
                throw "Expected an input file: $path"
            }
            $pending.Push($path)
        }
    } else { $pending.Push($rootPath) }
    while ($pending.Count) {
        $path = $pending.Pop()
        $attributes = [IO.File]::GetAttributes($path)
        if ($attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Fixture contains a reparse point: $path" }
        if ($attributes -band [IO.FileAttributes]::Directory) {
            foreach ($child in [IO.Directory]::GetFileSystemEntries($path)) { $pending.Push($child) }
        } else {
            $relative = [IO.Path]::GetRelativePath($rootPath, $path).Replace('\', '/')
            $null = Get-ContainedPath $rootPath $relative
            $paths.Add($relative)
        }
    }
    $paths.Sort([StringComparer]::Ordinal)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($relative in $paths) {
        if (-not $seen.Add($relative)) { throw "Colliding fixture path: $relative" }
        $relative
    }
}

function Get-FixtureFileIdentities([string]$Root, [string[]]$RelativeFiles = @()) {
    foreach ($relative in @(Get-FixtureFilePaths $Root $RelativeFiles)) {
        $path = Get-ContainedPath $Root $relative
        [ordered]@{ path = $relative; length = (Get-Item -LiteralPath $path -Force).Length
            sha256 = Get-FileSha256 $path }
    }
}

function Assert-RegularFixturePath([string]$Root, [string]$Relative) {
    $null = Get-ContainedPath $Root $Relative
    $path = [IO.Path]::GetFullPath($Root)
    foreach ($component in @('') + @($Relative -split '[/\\]')) {
        if ($component) { $path = Join-Path $path $component }
        if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) {
            throw "Fixture contains a reparse point: $path"
        }
    }
}

function Get-FixtureCdIdentity($Fixture) {
    $image = Get-FixtureOption $Fixture 'cdImage'
    if (-not $image) { return @() }
    $path = Get-ContainedPath $benchRoot $image
    Assert-RegularFixturePath $benchRoot $image
    $root = [IO.Path]::GetDirectoryName($path)
    $files = @([IO.Path]::GetFileName($path))
    if ([IO.Path]::GetExtension($path) -ieq '.cue') {
        foreach ($line in [IO.File]::ReadAllLines($path)) {
            if ($line -notmatch '^\s*FILE\s') { continue }
            if ($line -notmatch '^\s*FILE\s+(?:"(?<quoted>[^"]+)"|(?<bare>\S+))\s+\S+\s*$') {
                throw "Malformed CUE FILE entry: $line"
            }
            $files += if ($Matches.quoted) { $Matches.quoted } else { $Matches.bare }
        }
        if ($files.Count -eq 1) { throw "CUE contains no tracks: $path" }
    }
    return @(Get-FixtureFileIdentities $root @($files | Select-Object -Unique))
}

function Clear-FixtureOutputs($Fixture, [string]$WorkingCopy) {
    $files = @('QUAKE/ID1/QCONSOLE.LOG')
    if ($Fixture.dukemark) { $files += $Fixture.dukemark.resultFile }
    $text = Get-FixtureOption $Fixture 'textResults'
    if ($text) { $files += $text.files }
    foreach ($file in $files) {
        $path = Get-ContainedPath $WorkingCopy $file
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    }
}

function Prepare-FixtureInputs($Fixture, [string]$WorkingCopy) {
    Clear-FixtureOutputs $Fixture $WorkingCopy
    if ($Fixture.name -in @('doom-486', 'doom-586')) {
        [IO.File]::WriteAllBytes((Join-Path $WorkingCopy 'EXITVM.COM'),
            [byte[]]@(0xB0, 0x0C, 0xE6, 0xE4, 0xB0, 0x00, 0xE6, 0xE5,
                0xB0, 0x03, 0xE6, 0xE6, 0xF4, 0xEB, 0xFD))
    }
}

function Get-FixtureDescriptor($Fixture, [string]$WorkingCopy) {
    $knobValues = Resolve-KnobPassthrough $Knobs @((Get-BoardOwnedEnvironment).Keys)
    $knobNames = [string[]]@($knobValues.Keys)
    [Array]::Sort($knobNames, [StringComparer]::Ordinal)
    $orderedKnobs = [ordered]@{}
    foreach ($name in $knobNames) { $orderedKnobs[$name] = $knobValues[$name] }
    return [ordered]@{
        schema = 'fixture-inputs-v1'; name = $Fixture.name
        hdd_files = @(Get-FixtureFileIdentities $WorkingCopy)
        arguments = @($Fixture.arguments); injection = @($Fixture.injection)
        knobs = $orderedKnobs
        frame_kind = $(if (-not $Fixture.resultPpm) { 'none' }
            elseif (Get-FixtureOption $Fixture 'gradePresentedFrame') { 'presented' } else { 'result' })
        cd_files = @(Get-FixtureCdIdentity $Fixture)
    }
}

function Get-DescriptorSha256($Descriptor) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Descriptor | ConvertTo-Json -Depth 16 -Compress))
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

function New-PinContext($Fixture, $Descriptor) {
    $contract = Get-FrameContract $Fixture
    return [ordered]@{
        schema = 'fixture-pin-context-v1'; timing_model_epoch = 2
        cycle_budget = [uint64]$Fixture.cycles
        anchor_cycle_budget = $(if ($contract) { [uint64]$contract.anchorCycles } else { $null })
        fixture_contract_sha256 = Get-DescriptorSha256 $Descriptor
    }
}

function Test-PinContext($Recorded, $Context) {
    if ($null -eq $Recorded -or -not $Recorded.Contains('pin_context')) { return $false }
    $pin = $Recorded.pin_context
    if ($null -eq $pin -or $pin.Count -ne $Context.Count) { return $false }
    foreach ($key in $Context.Keys) {
        if (-not $pin.Contains($key) -or $pin[$key] -cne $Context[$key]) { return $false }
    }
    return $true
}

function Test-RowPin($Recorded, $Row, [string]$Axis) {
    $qualified = (Test-PinContext $Recorded $Row.pin_context) -and
        $Recorded.Contains('qualified_axes') -and @($Recorded.qualified_axes) -ccontains $Axis
    if (-not $qualified -and $Row.refused_axes -notcontains $Axis) {
        $Row.refused_axes += $Axis
        $Row.notes += "$Axis pin is unqualified for this fixture, configuration and timing window"
    }
    return $qualified
}

function Get-RequiredPositiveNumber($Object, [string]$Name) {
    $value = Get-FixtureOption $Object $Name
    if ($null -eq $value -or $value -is [string] -or $value -is [bool]) { throw "Missing or nonnumeric $Name" }
    try { $number = [double]$value } catch { throw "Nonnumeric $Name" }
    if (-not [double]::IsFinite($number) -or $number -le 0) { throw "Invalid ${Name}: $value" }
    return $number
}

function Assert-FixtureCapture($Fixture, $Profile, [int]$ExitCode, [uint64]$Budget, [bool]$Anchor = $false) {
    if ((Get-FixtureOption $Profile 'schema') -cne 'izarravm-hdd-profile-v2') {
        throw 'Capture has no supported HDD profile schema'
    }
    if ((Get-RequiredUInt64Property $Profile 'timing_model_epoch' 'profile') -ne 2) {
        throw 'The capture must explicitly report timing model 2'
    }
    $cpuAt = [Array]::IndexOf([string[]]$Fixture.arguments, '--cpu')
    if ($cpuAt -lt 0 -or $Profile.mode -cne $Fixture.arguments[$cpuAt + 1]) {
        throw "Effective CPU '$($Profile.mode)' differs from the fixture's requested CPU"
    }
    if ((Get-RequiredUInt64Property $Profile 'cycle_budget' 'profile') -ne $Budget) {
        throw "Capture cycle_budget differs from the declared $Budget"
    }
    foreach ($name in @('real_time_factor', 'guest_seconds', 'wall_seconds')) {
        $null = Get-RequiredPositiveNumber $Profile $name
    }
    $text = Get-FixtureOption $Fixture 'textResults'
    $completion = -not $Anchor -and ($null -ne $Fixture.gametics -or $Fixture.dukemark -or $text)
    $code = if ($completion -and $Fixture.dukemark) { $Fixture.dukemark.exitCode }
        elseif ($completion -and $text) { $text.exitCode } else { 0 }
    if ($ExitCode -ne 0) { throw "Host exit code $ExitCode, expected 0" }
    if ($completion) {
        if ($Profile.stop.kind -ne 'test_exit' -or
            (Get-RequiredUInt64Property $Profile.stop 'code' 'profile.stop') -ne $code) {
            throw "Guest did not complete through test_exit code $code"
        }
    } elseif ($Profile.stop.kind -ne 'cycle_limit' -or
        (Get-RequiredUInt64Property $Profile 'elapsed_budget_clocks' 'profile') -lt $Budget) {
        throw 'Guest did not complete the declared full cycle window'
    }
}

function Read-ScoreboardQuakeResult([string]$Path) {
    $lines = @([IO.File]::ReadAllLines($Path) | Where-Object { $_ -match '\d+\s+frames' })
    if ($lines.Count -ne 1 -or $lines[0] -notmatch
        '^\s*(?<frames>\d+)\s+frames\s+(?<seconds>\d+(?:\.\d+)?)\s+seconds\s+(?<fps>\d+(?:\.\d+)?)\s+fps\s*$') {
        throw 'QCONSOLE must contain exactly one valid timedemo result'
    }
    $frames = [uint64]$Matches.frames
    $seconds = [double]::Parse($Matches.seconds, [Globalization.CultureInfo]::InvariantCulture)
    $fps = [double]::Parse($Matches.fps, [Globalization.CultureInfo]::InvariantCulture)
    if ($frames -ne 969 -or -not [double]::IsFinite($seconds) -or $seconds -le 0 -or
        -not [double]::IsFinite($fps) -or $fps -le 0 -or [Math]::Abs($frames / $seconds - $fps) -gt 0.2) {
        throw 'QCONSOLE timedemo identity is incomplete or inconsistent'
    }
    return $lines[0].Trim()
}

function Assert-MojoReports([string]$Board, [string]$Registers) {
    foreach ($report in @($Board, $Registers)) {
        if ($report -match "No Voodoo boards found|Couldn't get info for Voodoo|Bogus number of TMUs") {
            throw 'MOJO reported a diagnostic failure'
        }
    }
    if ([regex]::Matches($Board, '(?m)^\s*Info for Voodoo board #').Count -ne 1 -or
        $Board -notmatch '(?m)^\s*Info for Voodoo board #\s*0:\s*$') { throw 'Invalid MOJO board header' }
    $facts = [ordered]@{
        'Vendor ID:' = '0x121a'; 'Device ID:' = '0x1'; 'FBI Revision:' = '2'; 'FBI Memory:' = '4 MB'
        'FBI PowerOn Sense:' = '0x6'; 'TMU PowerOn Sense:' = '0xc1'
        'FBI DAC Output Color Format:' = '24BPP'; 'Scan-Line Interleaved?' = 'No'
        'TMU Revision:' = '1'; 'Number TMUs:' = '2'; 'TMU 0 RAM:' = '4 MB'; 'TMU 1 RAM:' = '4 MB'
    }
    foreach ($label in $facts.Keys) {
        $pattern = '(?m)^\s*' + [regex]::Escape($label)
        $rows = [regex]::Matches($Board, $pattern + '[^\r\n]*')
        if ($rows.Count -ne 1 -or $rows[0].Value -notmatch
            ($pattern + '\s*' + [regex]::Escape($facts[$label]) + '\s*$')) { throw "Invalid MOJO fact: $label" }
    }
    foreach ($label in @('Virtual Base Address:', 'Physical Base Address:', 'PCI Device Number:')) {
        $rows = [regex]::Matches($Board, '(?m)^\s*' + [regex]::Escape($label) + '[^\r\n]*')
        if ($rows.Count -ne 1 -or $rows[0].Value -notmatch '0x[0-9a-fA-F]+\s*$') { throw "Invalid MOJO location: $label" }
    }
    if ([regex]::Matches($Registers, '(?m)^\s*Register Name\s+Data\s+Address\s*$').Count -ne 1) { throw 'Invalid MOJO register header' }
    $addresses = [ordered]@{
        status = 0x000; fbzColorPath = 0x104; fogMode = 0x108; alphaMode = 0x10c
        fbzMode = 0x110; lfbMode = 0x114; clipLeftRight = 0x118; clipBottomTop = 0x11c
        stipple = 0x140; c0 = 0x144; c1 = 0x148; fbiPixelsIn = 0x14c; fbiChromaFail = 0x150
        fbiZfuncFail = 0x154; fbiAfuncFail = 0x158; fbiPixelsOut = 0x15c; fbiInit4 = 0x200
        vRetrace = 0x204; backPorch = 0x208; videoDimensions = 0x20c; fbiInit0 = 0x210
        fbiInit1 = 0x214; fbiInit2 = 0x218; fbiInit3 = 0x21c
    }
    foreach ($name in $addresses.Keys) {
        $rows = [regex]::Matches($Registers, '(?m)^\s*' + $name + ':[^\r\n]*')
        if ($rows.Count -ne 1 -or $rows[0].Value -notmatch ':\s+([0-9a-fA-F]{8})\s+([0-9a-fA-F]+)\s*$' -or
            [Convert]::ToUInt32($Matches[2], 16) -ne $addresses[$name]) { throw "Invalid MOJO register row: $name" }
    }
}

function Preserve-FixtureArtifacts($Fixture, [string]$ScratchRoot, [string]$Stamp,
    [string]$WorkingCopy, [string]$Archive, [string]$Suffix = '') {
    $stem = "$($Fixture.name)$Suffix"
    foreach ($extension in @('json', 'ppm', 'out', 'err', 'inputs.json')) {
        $source = Join-Path $ScratchRoot "$stem-$Stamp.$extension"
        if (Test-Path -LiteralPath $source) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $Archive "$stem.$extension") -Force
        }
    }
    $files = @('QUAKE/ID1/QCONSOLE.LOG')
    if ($Fixture.dukemark) { $files += $Fixture.dukemark.resultFile }
    $text = Get-FixtureOption $Fixture 'textResults'
    if ($text) { $files += $text.files }
    foreach ($file in $files) {
        $source = Get-ContainedPath $WorkingCopy $file
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination `
                (Join-Path $Archive "$stem.$($file.Replace('/', '_').Replace('\', '_'))") -Force
        }
    }
}

function Complete-RowPins($Fixture, $Row, $Invariants, [bool]$Record = $false, [bool]$Overwrite = $false) {
    if ($Row.invariant -eq 'FAIL') {
        if ($Record) { throw "Cannot record invalid capture: $($Fixture.name)" }
        return
    }
    $recorded = if ($Invariants.Contains($Fixture.name)) { $Invariants[$Fixture.name] } else { @{} }
    $sameContext = Test-PinContext $recorded $Row.pin_context
    if ($Record) {
        if (-not $sameContext) { $recorded.qualified_axes = @() }
        $recorded.pin_context = $Row.pin_context
        if (-not $recorded.Contains('qualified_axes')) { $recorded.qualified_axes = @() }
    }
    $specs = @()
    if ($Row.Contains('frame_sha256') -and -not (Get-FixtureOption $Fixture 'frame_sha256_allowed')) {
        $specs += @{ axis = 'frame'; key = 'frame_sha256'; value = $Row.frame_sha256; tolerance = 0 }
    }
    if ($Row.Contains('text_result_sha256')) {
        $specs += @{ axis = 'text_reports'; key = 'text_result_sha256'; value = $Row.text_result_sha256; tolerance = 0 }
    }
    if ($Row.Contains('dukemark_samples') -and $null -ne $Row.dukemark_samples) {
        $specs += @{ axis = 'dukemark_samples'; key = 'dukemark_samples'; value = $Row.dukemark_samples
            tolerance = $dukemarkSampleTolerance }
    }
    if (Get-FrameContract $Fixture) {
        if ($Row.Contains('anchor_frame_sha256')) {
            $specs += @{ axis = 'anchor_frame'; key = 'anchor_frame_sha256'; value = $Row.anchor_frame_sha256; tolerance = 0 }
        }
        $specs += @{ axis = 'final_instructions'; key = 'final_instructions'; value = $Row.instructions
            tolerance = $frameInstructionTolerance }
    }
    foreach ($spec in $specs) {
        $key = $spec.key
        $hasPin = $recorded.Contains($key)
        $expected = if ($hasPin) { $recorded[$key] } else { $null }
        $toleranceKey = $key + '_tolerance'
        $tolerance = if ($recorded.Contains($toleranceKey)) { $recorded[$toleranceKey] } else { $spec.tolerance }
        $matches = $false
        if ($hasPin) {
            if ($spec.axis -eq 'anchor_frame') { $matches = @($expected) -contains $spec.value }
            elseif ($spec.axis -eq 'text_reports') {
                $matches = $expected.Count -eq $spec.value.Count
                foreach ($name in $spec.value.Keys) {
                    $matches = $matches -and $expected.Contains($name) -and $expected[$name] -ceq $spec.value[$name]
                }
            } elseif ($spec.tolerance -gt 0) {
                $matches = [double]$expected -gt 0 -and
                    [Math]::Abs([double]$spec.value - [double]$expected) -le
                    [Math]::Max(1, [double]$expected * $tolerance)
            } else { $matches = $expected -ceq $spec.value }
        }
        if ($Record) {
            if ($hasPin -and -not $matches -and -not $Overwrite) { throw "Recording $key would replace a pin; use -Force after reviewing the change" }
            if ($spec.axis -eq 'anchor_frame') {
                $anchors = if ($sameContext -and $hasPin) { @($expected) } else { @() }
                if ($anchors -notcontains $spec.value) {
                    if ($anchors.Count -ge (Get-FrameContract $Fixture).anchorPhases) { throw 'Anchor phase limit exceeded' }
                    $anchors += $spec.value
                }
                $recorded[$key] = @($anchors)
            } else { $recorded[$key] = $spec.value }
            if ($spec.tolerance -gt 0) { $recorded[$toleranceKey] = $tolerance }
            if ($recorded.qualified_axes -notcontains $spec.axis) { $recorded.qualified_axes += $spec.axis }
        } elseif (Test-RowPin $recorded $Row $spec.axis) {
            if (-not $matches) { $Row.invariant = 'FAIL'; $Row.notes += "$key differs from the qualified pin" }
        }
    }
    if (Get-FrameContract $Fixture) {
        if (Test-RowPin $recorded $Row 'content_bands') {
            foreach ($band in @(
                @{ value = 'final_nonblack_pct'; low = 'final_nonblack_percent_min'; high = 'final_nonblack_percent_max' },
                @{ value = 'final_distinct_colors'; low = 'final_distinct_colors_min'; high = 'final_distinct_colors_max' })) {
                if (-not $recorded.Contains($band.low) -or -not $recorded.Contains($band.high) -or
                    $Row[$band.value] -lt $recorded[$band.low] -or $Row[$band.value] -gt $recorded[$band.high]) {
                    $Row.invariant = 'FAIL'; $Row.notes += "$($band.value) differs from the qualified content band"
                }
            }
        }
    }
    if ($Record) { $Invariants[$Fixture.name] = $recorded }
    if ($Row.invariant -ne 'FAIL' -and $Row.refused_axes.Count) { $Row.invariant = 'unpinned' }
}


function Write-Invariants($Table) {
    $json = $Table.GetEnumerator() |
        Sort-Object Key |
        ForEach-Object -Begin { $ordered = [ordered]@{} } `
            -Process { $ordered[$_.Key] = $_.Value } `
            -End { $ordered } |
        ConvertTo-Json -Depth 16
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

# The emulator's command line for one run of a fixture. Factored out of
# Invoke-Fixture because the frame-contract rows (see New-FrameContract) launch
# the SAME fixture a second time at the anchor budget, and an anchor run assembled
# from a second, hand-kept copy of this list would drift from the graded one --
# a different persona or a missing --cd-image would move the anchor hash and read
# as a regression.
function Get-FixtureArguments($Fixture, [string]$WorkingCopy, [uint64]$Cycles,
    [string]$ProfilePath, [string]$PpmPath) {
    $arguments = @()
    $arguments += $Fixture.arguments
    $arguments += @("--hdd-folder", $WorkingCopy)
    $arguments += @("--cycles", $Cycles.ToString())
    $arguments += @("--profile-json", $ProfilePath)
    if (-not [string]::IsNullOrWhiteSpace($PpmPath)) {
        # `--result-ppm` RE-RENDERS the whole frame at stop-time register state,
        # so it reports what video memory holds. `--presented-ppm` writes the
        # frame the scanout actually published, which is what a user sees. The
        # difference is not cosmetic: a defect that fills video memory correctly
        # and never publishes it reads 82.9% non-black through --result-ppm and
        # 0.0% through --presented-ppm, measured on psycho-486 with the
        # `resize_work` raster wipe restored. A row that grades CONTENT bands
        # wants the published frame; the rows that pin a re-render keep it,
        # because their pins were taken that way. Same path either way, so every
        # consumer downstream -- hash, bands, width and height -- is unchanged.
        $presentedProperty = $Fixture.PSObject.Properties['gradePresentedFrame']
        if ($null -ne $presentedProperty -and $presentedProperty.Value) {
            $arguments += @("--presented-ppm", $PpmPath)
        } else {
            $arguments += @("--result-ppm", $PpmPath)
        }
    }
    # A fixture that names a cdImage mounts it straight from the shared .bench
    # tree: the emulator reads the image into memory and never writes it, so a
    # per-run copy would spend 600+ MB of I/O to protect nothing. Property
    # probed by name because only CD fixtures carry it and StrictMode is on.
    $cdImageProperty = $Fixture.PSObject.Properties['cdImage']
    if ($null -ne $cdImageProperty -and $cdImageProperty.Value) {
        $arguments += @("--cd-image",
            (Join-Path $benchRoot $cdImageProperty.Value))
    }
    # The injection schedule is passed to the anchor run too. Its steps are keyed
    # to absolute guest cycles, so the ones past the anchor budget simply never
    # fire; dropping them would make the anchor run a DIFFERENT workload from the
    # graded one up to that point on any fixture whose schedule starts early.
    $arguments += $Fixture.injection
    return $arguments
}

# Everything the BOARD itself owns, in one table, values included. Two callers
# read it: Get-RowEnvironment applies it, and the -Knobs passthrough takes its
# RESERVED NAME LIST from `.Keys` here. That is why it is one table rather than
# a run of assignments plus a hand-copied list of names somewhere else -- a
# hand-copied list drifts the moment a knob is added, and the way it drifts is
# that the passthrough silently starts accepting a name the board also sets,
# with the winner decided by assignment order that nobody is looking at.
#
# Set every variable explicitly. Restoring one that was never set writes an
# EMPTY STRING rather than removing it, and an empty IZARRAVM_JIT reads as
# OFF, which has silently turned real observations into interpreter runs.
# The barrier census stays OFF: it only does work when the JIT is active, so
# it taxes exactly the runs this is trying to time.
function Get-BoardOwnedEnvironment {
    $armFlags = switch ($Arm) {
        "on"      { @{ jit16 = "1"; word486 = "1" } }
        "off"     { @{ jit16 = "0"; word486 = "0" } }
        "jit16"   { @{ jit16 = "1"; word486 = "0" } }
        "word486" { @{ jit16 = "0"; word486 = "1" } }
    }
    return [ordered]@{
        "IZARRAVM_JIT"                   = "1"
        "IZARRAVM_JIT16"                 = $armFlags.jit16
        "IZARRAVM_JIT16_486"             = $armFlags.word486
        "IZARRAVM_ONE_LOOKUP_STORE"      = $OneLookupStore
        "IZARRAVM_ONE_LOOKUP_LOAD"       = $OneLookupLoad
        "IZARRAVM_DIRECT_BARRIER_CENSUS" = "0"
        # Instrument and audio observers. None of them belongs in a board row; a
        # stray parent-shell value would otherwise apply to every row, silently.
        #
        # $null REMOVES the variable from the child environment. An empty string
        # does NOT: it sets the variable empty-but-set, and any reader that uses
        # var_os()/is_some() arms on it. Measured 2026-08-15: an empty
        # IZARRAVM_RIP_PROFILE armed the RIP sampler (500 us thread suspends) on
        # every board row. Removal is also the only value safe for EVERY binary
        # era: a pre-C1 exe reads an empty IZARRAVM_AUDIO_WAV as a real path and
        # aborts every row (measured 2026-08-14, eight FAILs).
        "IZARRAVM_TIMING_EPOCH"          = $null
        "IZARRAVM_CPU_PROFILE"           = $null
        "IZARRAVM_MACHINE_PROFILE"       = $null
        "IZARRAVM_RIP_PROFILE"           = $null
        "IZARRAVM_PHASE_INTERVAL_MS"     = $null
        "IZARRAVM_AUDIO_WAV"             = $null
        "IZARRAVM_AUDIO_WAV_WALL"        = $null
        "IZARRAVM_AUDIO_COST"            = $null
        "IZARRAVM_AUDIO_COST_SLICE_MS"   = $null
    }
}

<#
Parse the -Knobs arm passthrough into an explicit name -> value map.

Pure: it touches no environment and reads nothing from the parent shell, so the
self-test can drive it directly and so a typo fails in the first second rather
than after the first row of a half-hour board.

Every rejection here is deliberate and loud. The alternative to each throw is a
board that runs an arm nobody asked for and reports it as the arm they did:

  no `=`              ambiguous between "set empty" and "leave unset", which
                      are OPPOSITE arms for IZARRAVM_SEGMENT_RETIRE_GOVERNOR.
  not IZARRAVM_*      out of scope; this parameter arms the emulator, and
                      handing it PATH or RUST_LOG is a mistake, not a request.
  lower case          Windows environment names are CASE-INSENSITIVE, so
                      `izarravm_jit=0` would override IZARRAVM_JIT while
                      sailing past a case-sensitive reserved check. Requiring
                      upper case closes that hole at the name test, before the
                      reserved test ever runs.
  reserved            the board sets it; -Arm / -OneLookupStore /
                      -OneLookupLoad are the supported ways to move those.
  repeated            last-one-wins is a silent arm change.

The returned VALUES are always [string], never $null, so nothing that comes out
of here can mean "remove this variable". Removal is the scrub's job and the
scrub's alone.
#>
function Resolve-KnobPassthrough([string[]]$Specification, $ReservedNames) {
    $resolved = [ordered]@{}
    # Case-insensitive by default, which is the safe direction on Windows.
    $reserved = @{}
    foreach ($name in @($ReservedNames)) { $reserved[[string]$name] = $true }

    # SPLIT ON COMMA OURSELVES, and do it before anything else looks at an entry.
    #
    # `pwsh -File script.ps1 -Knobs A=1,B=2` binds ONE string "A=1,B=2" to a
    # [string[]] parameter -- measured 2026-08-24, quoted and unquoted alike, and
    # `-Knobs A=1 -Knobs B=2` is a binder error rather than two values. Without
    # this split the first knob would silently be armed to the value
    # "1,B=2" and the second would never be armed at all: a board that ran an arm
    # nobody asked for and labelled it one that nobody ran.
    #
    # -Fixtures does NOT survive the same binder quirk on its own. The old claim
    # here -- that its known-name check throws on the mangled value -- was true
    # for the comma shape only. For the two-token shape the extra token never
    # reaches -Fixtures at all: it binds POSITIONALLY to a later [string]
    # parameter, measured 2026-08-27 (see the note above the param block). So
    # -Fixtures gets the same treatment: PositionalBinding is off script-wide,
    # and Resolve-FixtureSelection splits commas the way this function does.
    # A knob value cannot be checked against a list, so the split for -Knobs
    # has to happen here.
    #
    # The consequence is that a knob VALUE may not contain a comma. That is not
    # a silent restriction: `IZARRAVM_X=a,b` splits to `IZARRAVM_X=a` and a bare
    # `b`, and the bare piece is rejected below for having no '='. No campaign
    # knob takes a comma today.
    $entries = @()
    foreach ($element in @($Specification)) {
        if ($null -eq $element) {
            throw "-Knobs contains a null entry. Write each knob as NAME=VALUE."
        }
        $entries += ([string]$element).Split(',')
    }

    foreach ($entry in $entries) {
        $separator = $entry.IndexOf('=')
        if ($separator -lt 0) {
            throw ("-Knobs entry '$entry' has no '='. Write NAME=VALUE. A bare name is " +
                "ambiguous between 'set it to the empty string' and 'leave it unset', " +
                "and those are OPPOSITE arms for some knobs " +
                "(IZARRAVM_SEGMENT_RETIRE_GOVERNOR: unset is the default 'cap', empty is " +
                "OFF), so this script refuses to guess. Write '$entry=' to set it empty, " +
                "or leave it out of -Knobs entirely to leave it unset.")
        }
        $name = $entry.Substring(0, $separator)
        # Byte for byte. Trimming would silently turn ' ' into '', which is the
        # exact class of conversion this parameter exists to make impossible.
        $value = $entry.Substring($separator + 1)

        if ($name -cnotmatch '^IZARRAVM_[A-Z0-9_]+$') {
            throw ("-Knobs entry '$entry' names '$name'. Only IZARRAVM_* knobs may be " +
                "passed through, spelled in UPPER CASE and matching " +
                "IZARRAVM_[A-Z0-9_]+ exactly. Windows environment names are " +
                "case-insensitive, so a lower-case spelling could shadow a name the " +
                "board sets itself.")
        }
        if ($reserved.ContainsKey($name)) {
            throw ("-Knobs entry '$entry' names '$name', which this board sets itself on " +
                "every row. Overriding it here would make the board report an arm it did " +
                "not run. Use -Arm for the JIT16 pair, -OneLookupStore / -OneLookupLoad " +
                "for the one-lookup pair; the barrier census and the observer overrides " +
                "are held off deliberately because they tax the very runs this times. " +
                "Reserved: " + ((@($ReservedNames) | Sort-Object) -join ', '))
        }
        if ($resolved.Contains($name)) {
            throw ("-Knobs names '$name' more than once. Last-one-wins would silently " +
                "change the arm, so state it exactly once.")
        }
        $resolved[$name] = $value
    }
    return $resolved
}

<#
Parse the -Fixtures selection into a validated list of fixture names.

Pure, for the same reason Resolve-KnobPassthrough is: the self-test can drive
it directly, and a typo fails before the first row rather than after it.

It splits on the comma ITSELF because `pwsh -File ... -Fixtures a,b` binds ONE
string "a,b" to the [string[]] parameter; without the split, the known-name
check would reject the documented comma shape instead of running two rows.
The two-token shape (`-Fixtures a b`) never gets here -- PositionalBinding is
off for the whole script, so the binder rejects the second token first.

Every element is then checked against the fixture table. Whitespace around an
element is trimmed (a name can never gain meaning from padding); an empty
element or a repeated one is refused, because both mean the caller's list and
the board's row count disagree.
#>
function Resolve-FixtureSelection([string[]]$Specification, [string[]]$KnownNames) {
    $entries = @()
    foreach ($element in @($Specification)) {
        if ($null -eq $element) {
            throw "-Fixtures contains a null entry. Name each fixture, comma-separated."
        }
        $entries += ([string]$element).Split(',')
    }

    $selected = @()
    foreach ($entry in $entries) {
        $name = ([string]$entry).Trim()
        if ($name -eq "") {
            throw ("-Fixtures contains an empty entry. A stray comma would silently " +
                "shrink the sweep, so it is refused instead.")
        }
        if (@($KnownNames) -notcontains $name) {
            throw "Unknown fixture '$name'. Known: $(@($KnownNames) -join ', ')"
        }
        if ($selected -contains $name) {
            throw ("-Fixtures names '$name' more than once. The board runs each fixture " +
                "once, so a repeat would report fewer rows than the caller asked for.")
        }
        $selected += $name
    }
    return ,$selected
}

# The child environment for one row: scrub, then the board's own table, then the
# caller's explicit -Knobs passthrough.
#
# The passthrough goes LAST, but it can never overwrite a board-owned name --
# Resolve-KnobPassthrough rejects those outright, using this very table's keys
# as the reserved set. Order is therefore cosmetic, and deliberately so: there
# is no assignment race here to reason about.
#
# $KnobSpecification defaults to the script's -Knobs so every production call
# site stays `Get-RowEnvironment` with no arguments; the parameter exists so the
# self-test can drive the REAL function with synthetic input rather than
# re-implementing it.
function Get-RowEnvironment {
    param([string[]]$KnobSpecification = $Knobs)

    $environment = @{}
    foreach ($name in [Environment]::GetEnvironmentVariables().Keys) {
        if ([string]$name -like "IZARRAVM_*") { $environment[[string]$name] = $null }
    }
    $environment["RUST_LOG"] = $null

    $boardOwned = Get-BoardOwnedEnvironment
    foreach ($entry in $boardOwned.GetEnumerator()) {
        $environment[$entry.Key] = $entry.Value
    }

    $knobValues = Resolve-KnobPassthrough $KnobSpecification @($boardOwned.Keys)
    foreach ($entry in $knobValues.GetEnumerator()) {
        $environment[$entry.Key] = $entry.Value
    }
    return $environment
}

<#
Read a CHILD PROCESS's own environment block back.

This exists because asserting about the hashtable Get-RowEnvironment returns
would be a test that cannot fail for the only thing that matters -- whether the
value reaches the emulator. The campaign's standing rule after seven instruments
read green regardless: prove the mechanism goes RED on a broken input.

`cmd /c set` is the cheapest faithful observer on this host. It lists a variable
that is PRESENT-BUT-EMPTY as `NAME=` and omits an ABSENT one entirely, which is
exactly the distinction the -Knobs contract turns on. Measured 2026-08-24 under
Start-Process -Environment on pwsh 7.4:

    value ""     -> the child block carries `NAME=`      (present, empty)
    value $null  -> the child block has no NAME at all   (absent)

Returned as a case-insensitive hashtable, matching how Windows itself compares
environment names.
#>
function Get-ChildEnvironmentSnapshot($Environment) {
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-knob-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "environment.txt"
        $start = @{
            FilePath               = "cmd.exe"
            ArgumentList           = @("/c", "set")
            NoNewWindow            = $true
            PassThru               = $true
            RedirectStandardOutput = $outputPath
            Environment            = $Environment
        }
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "scoreboard self-test failed: the environment observer child never exited"
        }
        $snapshot = @{}
        foreach ($line in @(Get-Content -LiteralPath $outputPath)) {
            $separator = ([string]$line).IndexOf('=')
            if ($separator -lt 1) { continue }
            $snapshot[([string]$line).Substring(0, $separator)] =
                ([string]$line).Substring($separator + 1)
        }
        return $snapshot
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
}

<#
The arm-passthrough self-test.

Half of it drives Resolve-KnobPassthrough directly -- that half is about which
inputs are REFUSED. The other half spawns a real child and reads the real
environment block, because that is the half that would otherwise pass whether
or not the passthrough was wired up at all.
#>
function Assert-ScoreboardKnobPassthroughSelfTest {
    $reserved = @((Get-BoardOwnedEnvironment).Keys)

    # --- what must be refused, and loudly ------------------------------------
    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_JCC_SHADOW") $reserved
    } "has no '='" "a bare knob name with no '='"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("PATH=c:\windows") $reserved
    } "Only IZARRAVM_* knobs" "a non-IZARRAVM_* name"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("RUST_LOG=trace") $reserved
    } "Only IZARRAVM_* knobs" "RUST_LOG"

    # Windows compares environment names case-insensitively, so this spelling
    # would shadow the board's own IZARRAVM_JIT if the name test let it by.
    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("izarravm_jit=0") $reserved
    } "UPPER CASE" "a lower-case knob name"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_JIT=0") $reserved
    } "which this board sets itself" "the reserved IZARRAVM_JIT"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_ONE_LOOKUP_STORE=0") $reserved
    } "which this board sets itself" "the reserved one-lookup store knob"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_RIP_PROFILE=1") $reserved
    } "which this board sets itself" "a reserved observer override"

    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_JCC_SHADOW=1", "IZARRAVM_JCC_SHADOW=0") $reserved
    } "more than once" "a knob named twice"

    # --- the `pwsh -File` comma-binding trap ---------------------------------
    # ONE string holding two knobs, which is exactly what the binder hands this
    # script under `-File`. Both must be armed; neither may be swallowed into
    # the other's value.
    $swallowed = Resolve-KnobPassthrough `
        @("IZARRAVM_SELFTEST_ONE=off,IZARRAVM_SELFTEST_TWO=1") $reserved
    Assert-ScoreboardSelfTestEqual $swallowed.Count 2 `
        "a comma-joined -Knobs string splitting into two knobs"
    Assert-ScoreboardSelfTestEqual $swallowed["IZARRAVM_SELFTEST_ONE"] "off" `
        "the first knob of a comma-joined string (not 'off,IZARRAVM_SELFTEST_TWO=1')"
    Assert-ScoreboardSelfTestEqual $swallowed["IZARRAVM_SELFTEST_TWO"] "1" `
        "the second knob of a comma-joined string"

    # A comma INSIDE a value is refused, not silently truncated to 'a'.
    Assert-ScoreboardSelfTestThrows {
        Resolve-KnobPassthrough @("IZARRAVM_SELFTEST_COMMA=a,b") $reserved
    } "has no '='" "a comma inside a knob value"

    # --- values survive verbatim ---------------------------------------------
    $spaced = Resolve-KnobPassthrough @("IZARRAVM_SELFTEST_SPACED= ") $reserved
    Assert-ScoreboardSelfTestEqual $spaced["IZARRAVM_SELFTEST_SPACED"] " " `
        "a knob value of one space (trimming it would forge an empty arm)"
    $pair = Resolve-KnobPassthrough @("IZARRAVM_SELFTEST_PAIR=a=b") $reserved
    Assert-ScoreboardSelfTestEqual $pair["IZARRAVM_SELFTEST_PAIR"] "a=b" `
        "a knob value containing '='"

    # --- observed from an actual child process -------------------------------
    # Pollute the parent so the scrub has real work to do: one invented name and
    # one REAL knob the board must still remove. Both restored in the finally.
    $leak = "IZARRAVM_SELFTEST_PARENT_LEAK"
    $realKnob = "IZARRAVM_PIT_BULK_ADVANCE"
    $previousLeak = [Environment]::GetEnvironmentVariable($leak)
    $previousReal = [Environment]::GetEnvironmentVariable($realKnob)
    [Environment]::SetEnvironmentVariable($leak, "leaked-from-parent")
    [Environment]::SetEnvironmentVariable($realKnob, "1")
    try {
        # 1. No -Knobs at all: byte-for-byte the old behaviour. Called with NO
        #    argument on purpose, so this exercises the default binding a
        #    production row uses rather than a synthetic empty array.
        $plain = Get-ChildEnvironmentSnapshot (Get-RowEnvironment)
        if ($plain.ContainsKey($leak)) {
            throw ("scoreboard self-test failed: the default scrub let $leak through to " +
                "the child (value '$($plain[$leak])')")
        }
        if ($plain.ContainsKey($realKnob)) {
            throw ("scoreboard self-test failed: the default scrub let a real parent-shell " +
                "knob $realKnob through to the child (value '$($plain[$realKnob])')")
        }
        if ($plain.ContainsKey("IZARRAVM_RIP_PROFILE")) {
            throw ("scoreboard self-test failed: the observer override " +
                "IZARRAVM_RIP_PROFILE reached the child")
        }
        Assert-ScoreboardSelfTestEqual $plain["IZARRAVM_JIT"] "1" `
            "the board's own IZARRAVM_JIT in the child"

        # 2. THE POINT: a passed-through knob reaches the child, with its value.
        $armed = Get-ChildEnvironmentSnapshot (Get-RowEnvironment -KnobSpecification @(
                "IZARRAVM_SEGMENT_RETIRE_GOVERNOR=off",
                "IZARRAVM_SELFTEST_EMPTY="))
        if (-not $armed.ContainsKey("IZARRAVM_SEGMENT_RETIRE_GOVERNOR")) {
            throw ("scoreboard self-test failed: a passed-through knob never reached the " +
                "child process at all -- the -Knobs passthrough is not wired up")
        }
        Assert-ScoreboardSelfTestEqual $armed["IZARRAVM_SEGMENT_RETIRE_GOVERNOR"] "off" `
            "the passed-through knob's VALUE as the child sees it"

        # 3. EMPTY IS NOT UNSET, observed from the child rather than asserted
        #    about the parent. `NAME=` must arrive PRESENT and empty; for
        #    IZARRAVM_SEGMENT_RETIRE_GOVERNOR that is the difference between OFF
        #    and the default `cap`, i.e. between the two arms of a ladder.
        if (-not $armed.ContainsKey("IZARRAVM_SELFTEST_EMPTY")) {
            throw ("scoreboard self-test failed: 'IZARRAVM_SELFTEST_EMPTY=' arrived UNSET " +
                "in the child; set-to-empty was silently converted to absent")
        }
        Assert-ScoreboardSelfTestEqual $armed["IZARRAVM_SELFTEST_EMPTY"] "" `
            "a knob armed to the empty string, as the child sees it"

        # 4. ... and the other direction. A real knob sitting in the PARENT
        #    shell, not named in -Knobs, must arrive ABSENT -- never inherited,
        #    and never downgraded to empty-but-set.
        if ($armed.ContainsKey($realKnob)) {
            throw ("scoreboard self-test failed: $realKnob was not named in -Knobs but " +
                "reached the child as '$($armed[$realKnob])'; unset was silently " +
                "converted to set")
        }
        if ($armed.ContainsKey($leak)) {
            throw "scoreboard self-test failed: -Knobs weakened the default scrub"
        }
    } finally {
        [Environment]::SetEnvironmentVariable($leak, $previousLeak)
        [Environment]::SetEnvironmentVariable($realKnob, $previousReal)
    }
}

<#
The -Fixtures selection self-test.

Half of it drives Resolve-FixtureSelection directly. The other half spawns this
very script under `pwsh -File` with the MANGLED two-token shape from the
2026-08-27 incident, because that failure happens in the parameter binder --
before any function in this file runs -- and only a real child invocation can
prove the guard fires there. The campaign rule applies: a new guard must be
shown to go RED on the broken input, and a green control must show the child
harness itself works, so the red row cannot pass by being unable to run.
#>
function Assert-ScoreboardFixtureSelectionSelfTest {
    $known = @((Get-FixtureTable).name)

    # --- the `pwsh -File` comma-binding trap, resolved not rejected ----------
    $split = Resolve-FixtureSelection @("doom-486,wolf3d-486") $known
    Assert-ScoreboardSelfTestEqual $split.Count 2 `
        "a comma-joined -Fixtures string splitting into two fixtures"
    Assert-ScoreboardSelfTestEqual $split[0] "doom-486" `
        "the first fixture of a comma-joined string"
    Assert-ScoreboardSelfTestEqual $split[1] "wolf3d-486" `
        "the second fixture of a comma-joined string"

    $padded = Resolve-FixtureSelection @(" doom-486 , wolf3d-486") $known
    Assert-ScoreboardSelfTestEqual $padded.Count 2 `
        "whitespace around comma-joined fixture names"

    # --- what must be refused, and loudly ------------------------------------
    Assert-ScoreboardSelfTestThrows {
        Resolve-FixtureSelection @("doom-486,not-a-fixture") $known
    } "Unknown fixture 'not-a-fixture'" "an unknown name after the comma split"

    Assert-ScoreboardSelfTestThrows {
        Resolve-FixtureSelection @("doom-486,") $known
    } "empty entry" "a stray trailing comma"

    Assert-ScoreboardSelfTestThrows {
        Resolve-FixtureSelection @("doom-486", "doom-486") $known
    } "more than once" "a fixture named twice"

    # --- the binder guard, observed from an actual child process -------------
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-fixsel-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $pwshExecutable = (Get-Process -Id $PID).Path
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"

        # RED: the incident's exact shape. Two tokens after -Fixtures; the
        # second must be a binder error, never a silent one-row board. The
        # child carries -ListFixtures so that even a broken guard costs a
        # listing, not a bench run.
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-Fixtures", "prince-486", "tombraid-loader-586", "-ListFixtures")
            RedirectStandardOutput = $outputPath
            RedirectStandardError  = $failurePath
            PassThru               = $true
            NoNewWindow            = $true
        }
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "scoreboard self-test failed: the mangled -Fixtures child never exited"
        }
        if ($process.ExitCode -eq 0) {
            throw ("scoreboard self-test failed: the mangled two-token -Fixtures " +
                "invocation exited 0. The 2026-08-27 silent-subset hazard is back: " +
                "the second fixture bound positionally instead of failing the binder.")
        }
        $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
        if ($failureText -notmatch 'tombraid-loader-586') {
            throw ("scoreboard self-test failed: the mangled -Fixtures child failed, but " +
                "not on the stray token. stderr: $failureText")
        }

        # GREEN control: the same invocation minus the stray token must work,
        # or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-Fixtures", "prince-486", "-ListFixtures")
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "scoreboard self-test failed: the -ListFixtures control child never exited"
        }
        if ($process.ExitCode -ne 0) {
            $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
            throw ("scoreboard self-test failed: the well-formed -ListFixtures control " +
                "exited $($process.ExitCode); the red row above is therefore " +
                "meaningless. stderr: $failureText")
        }
        $listing = [string](Get-Content -LiteralPath $outputPath -Raw)
        if ($listing -notmatch 'tombraid-loader-586') {
            throw ("scoreboard self-test failed: the -ListFixtures control printed no " +
                "fixture table")
        }
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
}

<#
The ANCHOR run: the same fixture, the same arguments, stopped at the anchor
budget instead of the graded one, for the one exact-frame invariant a
frame-contract row still keeps.

It is a SECOND emulator invocation because the only way to get an intermediate
frame out of one run is `--screen-dump-dir`, whose own help says it slices the
run -- a diagnostic path, not a benchmark path, and this row's wall is a
published metric. Running it AFTER the graded run rather than before is
deliberate for the same reason: the graded numbers are already captured by then,
so nothing the anchor run does to the host's caches can reach them.

It is cheap relative to what it protects: 7 s against tombraid-586's ~250 s,
5 s against nascar-586's ~60 s.

Nothing here is timed. The anchor run contributes NO wall, NO real-time factor
and NO coverage counters to the row -- only a frame hash.
#>
function Invoke-AnchorRun($Fixture, [string]$ExecutablePath, [string]$ScratchRoot,
    [string]$Archive, [string]$InputSha256) {
    $contract = Get-FrameContract $Fixture
    $stamp = [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $stem = "$($Fixture.name)-anchor-$stamp"
    $workingCopy = Join-Path $ScratchRoot $stem
    $profilePath = Join-Path $ScratchRoot "$stem.json"
    $ppmPath = Join-Path $ScratchRoot "$stem.ppm"
    $result = @{ sha256 = $null; display = $null; wall_s = 0.0; failure = $null }
    try {
        Copy-Fixture (Join-Path $benchRoot $Fixture.folder) $workingCopy
        Prepare-FixtureInputs $Fixture $workingCopy
        $descriptor = Get-FixtureDescriptor $Fixture $workingCopy
        $descriptor | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath `
            (Join-Path $ScratchRoot "$stem.inputs.json") -Encoding utf8
        if ((Get-DescriptorSha256 $descriptor) -cne $InputSha256) { throw 'Anchor prelaunch inputs differ from the main run' }
        $start = @{
            FilePath = $ExecutablePath
            ArgumentList = Get-FixtureArguments $Fixture $workingCopy $contract.anchorCycles $profilePath $ppmPath
            NoNewWindow = $true; PassThru = $true; Environment = Get-RowEnvironment
            RedirectStandardOutput = Join-Path $ScratchRoot "$stem.out"
            RedirectStandardError = Join-Path $ScratchRoot "$stem.err"
        }
        $wall = [Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process @start
        if ($ProcessorIndex -ge 0) { $process.ProcessorAffinity = [IntPtr]([int64]1 -shl $ProcessorIndex) }
        if (-not $process.WaitForExit($HostTimeoutSeconds * 1000)) {
            $process.Kill($true); $process.WaitForExit()
            throw "Anchor exceeded $HostTimeoutSeconds seconds"
        }
        $wall.Stop()
        $result.wall_s = [Math]::Round($wall.Elapsed.TotalSeconds, 3)
        $profile = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json
        Assert-FixtureCapture $Fixture $profile $process.ExitCode $contract.anchorCycles $true
        Assert-CaptureStderr $start.RedirectStandardError
        if ($null -eq (Get-PpmFrameStats $ppmPath)) { throw 'Anchor PPM is missing or malformed' }
        $result.sha256 = Get-FileSha256 $ppmPath
        $result.display = $profile.active_display
        if ([string]::IsNullOrWhiteSpace($result.display)) { throw 'Missing anchor active_display' }
        $result.profile = $profile
    } catch { $result.failure = "Anchor: $($_.Exception.Message)" }
    finally {
        Preserve-FixtureArtifacts $Fixture $ScratchRoot $stamp $workingCopy $Archive '-anchor'
        if (Test-Path -LiteralPath $workingCopy) { Remove-Item -LiteralPath $workingCopy -Recurse -Force }
    }
    return $result
}

function Assert-CaptureStderr([string]$Path) {
    $offending = @(Get-Content -LiteralPath $Path | Where-Object { $_ -notmatch '^open-bus: ' -and $_ -notmatch '^\s*$' })
    if ($offending.Count) { throw "Capture wrote diagnostics to stderr: $(($offending | Select-Object -First 3) -join '; ')" }
}

function Invoke-Fixture($Fixture, [string]$ExecutablePath, [string]$ScratchRoot,
    [string]$KeepProfilesIn, $Recorded) {
    $stamp = [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $stem = "$($Fixture.name)-$stamp"
    $workingCopy = Join-Path $ScratchRoot $stem
    $profilePath = Join-Path $ScratchRoot "$stem.json"
    $ppmPath = Join-Path $ScratchRoot "$stem.ppm"
    $stdoutPath = Join-Path $ScratchRoot "$stem.out"
    $result = [ordered]@{
        name = $Fixture.name; arm = $Arm; one_lookup_store = $OneLookupStore; one_lookup_load = $OneLookupLoad
        knobs = Resolve-KnobPassthrough $Knobs @((Get-BoardOwnedEnvironment).Keys)
        exit_code = $null; host_wall_s = 0.0; background_load = 0.0; background_peak = 0.0
        load_samples = 0; contaminated = $false; invariant = 'FAIL'; notes = @(); refused_axes = @()
    }
    try {
        Copy-Fixture (Join-Path $benchRoot $Fixture.folder) $workingCopy
        Prepare-FixtureInputs $Fixture $workingCopy
        $descriptor = Get-FixtureDescriptor $Fixture $workingCopy
        $descriptor | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath `
            (Join-Path $ScratchRoot "$stem.inputs.json") -Encoding utf8
        $result.pin_context = New-PinContext $Fixture $descriptor
        $arguments = Get-FixtureArguments $Fixture $workingCopy $Fixture.cycles $profilePath `
            $(if ($Fixture.resultPpm) { $ppmPath } else { $null })
        $start = @{
            FilePath = $ExecutablePath; ArgumentList = $arguments
            NoNewWindow = $true; PassThru = $true; Environment = Get-RowEnvironment
            RedirectStandardOutput = $stdoutPath
            RedirectStandardError = Join-Path $ScratchRoot "$stem.err"
        }
        $wall = [Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process @start
        if ($ProcessorIndex -ge 0) { $process.ProcessorAffinity = [IntPtr]([int64]1 -shl $ProcessorIndex) }
        $waited = Wait-WithLoadSampling $process $HostTimeoutSeconds
        if ($waited.timedOut) {
            $process.Kill($true); $process.WaitForExit()
            throw "Capture exceeded $HostTimeoutSeconds seconds"
        }
        $wall.Stop()
        $result.exit_code = $process.ExitCode
        $result.host_wall_s = [Math]::Round($wall.Elapsed.TotalSeconds, 3)
        $result.background_load = [Math]::Round((Get-Median ([double[]]$waited.samples)), 2)
        $result.background_peak = if ($waited.samples.Count) {
            [Math]::Round(($waited.samples | Measure-Object -Maximum).Maximum, 2)
        } else { 0.0 }
        $result.load_samples = $waited.samples.Count
        $result.contaminated = $result.background_load -ge $maximumBackgroundLoadPercent
        if ($result.contaminated) { $result.notes += "Background load exceeded $maximumBackgroundLoadPercent%" }
        $profile = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json
        Assert-FixtureCapture $Fixture $profile $process.ExitCode $Fixture.cycles
        Assert-CaptureStderr $start.RedirectStandardError
        $result.real_time_factor = [Math]::Round($profile.real_time_factor, 4)
        $result.guest_seconds = [Math]::Round($profile.guest_seconds, 3)
        $result.wall_seconds = [Math]::Round($profile.wall_seconds, 3)
        $result.timing_model_epoch = $profile.timing_model_epoch
        $result.stop = $profile.stop
        $null = Add-CoverageMetrics $result $profile
        $failures = @()
        if ($null -ne $Fixture.gametics) {
            $realtics = Get-RequiredUInt64Property $profile.timedemo 'realtics' 'profile.timedemo'
            $gametics = Get-RequiredUInt64Property $profile.timedemo 'gametics' 'profile.timedemo'
            $result.realtics = $realtics; $result.gametics = $gametics
            if ($realtics -eq 0 -or $gametics -ne $Fixture.gametics) { $failures += 'Timedemo did not complete the declared gametics with positive realtics' }
            if (Test-RowPin $Recorded $result 'realtics') {
                if ($realtics -lt $Fixture.realticsMinimum -or $realtics -gt $Fixture.realticsMaximum) {
                    $failures += 'Realtics differs from the qualified range'
                }
            }
        }
        if ($Fixture.qconsole) {
            $result.qconsole = Read-ScoreboardQuakeResult (Join-Path $workingCopy 'QUAKE/ID1/QCONSOLE.LOG')
        }
        $contract = Get-FrameContract $Fixture
        $allowed = Get-FixtureOption $Fixture 'frame_sha256_allowed'
        if ($Fixture.resultPpm) {
            $stats = Get-PpmFrameStats $ppmPath
            if ($null -eq $stats) { throw 'Result PPM is missing or malformed' }
            $hash = Get-FileSha256 $ppmPath
            $result.final_frame_width = $stats.width; $result.final_frame_height = $stats.height
            $result.final_display = $profile.active_display
            if ([string]::IsNullOrWhiteSpace($result.final_display)) { throw 'Missing active_display' }
            if ($contract) {
                $result.final_frame_sha256 = $hash
                $result.final_nonblack_pct = $stats.non_black_pct
                $result.final_distinct_colors = $stats.distinct_colors
            } else { $result.frame_sha256 = $hash }
            if ($contract -or $allowed) {
                $geometry = Test-RowPin $Recorded $result 'geometry'
                $expectedWidth = if ($contract) { $contract.width } else { $Fixture.expected_width }
                $expectedHeight = if ($contract) { $contract.height } else { $Fixture.expected_height }
                $expectedDisplay = if ($contract) { $contract.display } else { $Fixture.expected_display }
                $expectedMode = if ($contract) { $contract.mode } else { $Fixture.expected_video_mode }
                if ($geometry -and ($stats.width -ne $expectedWidth -or $stats.height -ne $expectedHeight -or
                    $profile.active_display -ne $expectedDisplay)) { $failures += 'Final geometry differs from the qualified phase' }
                if ($profile.active_display -eq 'MargoLfb') {
                    $result.final_bpp = Get-RequiredPositiveNumber $profile.margo_display 'bpp'
                    $result.final_mode = $profile.margo_display.mode
                    if ($geometry -and $contract -and $result.final_bpp -ne $contract.bpp) { $failures += 'Final bpp differs from the qualified phase' }
                } else { $result.final_mode = $profile.legacy_video_mode }
                if ([string]::IsNullOrWhiteSpace($result.final_mode)) { throw 'Missing final video mode' }
                if ($geometry -and $result.final_mode -ne $expectedMode) { $failures += 'Final video mode differs from the qualified phase' }
            }
            if ($allowed) {
                $result.frame_sha256_allowed = @($allowed)
                if ((Test-RowPin $Recorded $result 'allowed_frames') -and -not (Test-Sha256Allowed $allowed $hash)) {
                    $failures += 'Final frame is outside the qualified allowed set'
                }
            }
        }
        $stdoutMarker = Get-FixtureOption $Fixture 'stdout_contains'
        if ($stdoutMarker) {
            $temporalMarker = $stdoutMarker -like 'video mode:*'
            if (-not $temporalMarker -or (Test-RowPin $Recorded $result 'stdout_phase')) {
                if ((Get-Content -LiteralPath $stdoutPath -Raw) -notlike "*$stdoutMarker*") { $failures += "Missing stdout marker '$stdoutMarker'" }
            }
        }
        if ($contract) {
            $anchor = Invoke-AnchorRun $Fixture $ExecutablePath $ScratchRoot $KeepProfilesIn $result.pin_context.fixture_contract_sha256
            $result.anchor_cycles = $contract.anchorCycles; $result.anchor_wall_s = $anchor.wall_s
            if ($anchor.failure) { $failures += $anchor.failure }
            else {
                $result.anchor_frame_sha256 = $anchor.sha256; $result.anchor_display = $anchor.display
                if ((Test-RowPin $Recorded $result 'anchor_geometry') -and $anchor.display -ne $contract.anchorDisplay) {
                    $failures += 'Anchor display differs from the qualified phase'
                }
            }
        }
        $dukemarkResultPath = if ($Fixture.dukemark) { Get-ContainedPath $workingCopy $Fixture.dukemark.resultFile } else { $null }
        $textResultsProperty = $Fixture.PSObject.Properties['textResults']
        $textResultPaths = [ordered]@{}
        if ($textResultsProperty -and $textResultsProperty.Value) {
            foreach ($file in $textResultsProperty.Value.files) { $textResultPaths[$file] = Get-ContainedPath $workingCopy $file }
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

    # TEXT RESULTS (2026-09-01, added for `mojo-586`). A row whose whole output
    # is a report the guest redirected to a file, graded by sha256 of the bytes.
    #
    # This is the third grading shape in the table and it exists because the
    # other two cannot see what this row is for. A framebuffer hash grades
    # PIXELS, and MOJO draws none worth grading -- it prints text and exits. The
    # DUKEMARK path grades a report too, but it parses ONE known report format
    # for three named fields; here the whole file IS the invariant, because
    # every line of it is a board-identity fact (vendor and device ID, FBI
    # revision and memory, TMU count, revision and memory, DAC colour format,
    # and under `-v` the SST-1 register file). There is nothing in a MOJO report
    # that may drift benignly, which is exactly what makes an exact hash the
    # right instrument rather than the fragile one it is on an animating demo.
    #
    # The stop is graded the same way Duke3D's is: the guest ends the VM itself
    # through EXITVM.COM once the reports are written, so a `cycle_limit` stop
    # means the run never got that far and any file found would be a partial.
    if ($null -ne $textResultsProperty -and $textResultsProperty.Value) {
        $pins = $textResultsProperty.Value
        $stopKind = $profile.stop.kind
        $result.stop_kind = $stopKind
        if ($stopKind -ne "test_exit") {
            $failures += ("the guest did not exit through EXITVM: stop was '$stopKind', " +
                "expected 'test_exit' -- the reports were never finished")
        } else {
            $stopCode = [int]$profile.stop.code
            $result.stop_code = $stopCode
            if ($stopCode -ne $pins.exitCode) {
                $failures += "EXITVM reported exit code $stopCode, expected $($pins.exitCode)"
            }
        }

        $masksProperty = if ($pins -is [hashtable] -and $pins.ContainsKey('masks')) {
            $pins.masks
        } else { $null }

        $hashes = [ordered]@{}
        foreach ($entry in $textResultPaths.GetEnumerator()) {
            $masks = if ($null -ne $masksProperty -and $masksProperty.ContainsKey($entry.Key)) {
                @($masksProperty[$entry.Key])
            } else { @() }
            $hash = Get-TextResultSha256 $entry.Value $masks
            if ($null -eq $hash) {
                $failures += ("no $($entry.Key) was written: the redirection or the " +
                    "host-folder flush failed")
                continue
            }
            $hashes[$entry.Key] = $hash
            # Keep the report itself beside the profile. When the hash moves,
            # the question is always "moved HOW", and a diff of two reports
            # answers it in seconds where a bare pair of hashes answers nothing.
            if (-not [string]::IsNullOrWhiteSpace($KeepProfilesIn)) {
                Copy-Item -LiteralPath $entry.Value `
                    -Destination (Join-Path $KeepProfilesIn "$($Fixture.name).$($entry.Key)")
            }
        }
        if ($hashes.Count -gt 0) { $result.text_result_sha256 = $hashes }
    }

        if ($Fixture.name -eq 'mojo-586') {
            Assert-MojoReports ([IO.File]::ReadAllText($textResultPaths['MOJO.TXT'])) `
                ([IO.File]::ReadAllText($textResultPaths['MOJOV.TXT']))
        }

        $bands = Get-FixtureOption $Fixture 'profileBands'
        if ($bands) {
            $graded = Test-ProfileBands $profile $bands (Test-RowPin $Recorded $result 'profile_bands')
            foreach ($entry in $graded.values.GetEnumerator()) { $result[$entry.Key] = $entry.Value }
            $failures += $graded.failures
        }
        $result.invariant = if ($failures.Count) { 'FAIL' } else { 'pass' }
        $result.notes += $failures
    } catch {
        $result.invariant = 'FAIL'
        $result.notes += $_.Exception.Message
    } finally {
        Preserve-FixtureArtifacts $Fixture $ScratchRoot $stamp $workingCopy $KeepProfilesIn
        if (Test-Path -LiteralPath $workingCopy) { Remove-Item -LiteralPath $workingCopy -Recurse -Force }
    }
    return $result
}


if ($SelfTest) {
    Invoke-ScoreboardSelfTest
    return
}

# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------

# Resolve the -Knobs passthrough FIRST, before a fixture table is read or an
# executable is located. A misspelled knob name is a half-hour board that
# measured the default arm and labelled it something else; failing in the first
# second is the whole point of validating here rather than at the first row.
$resolvedKnobs = Resolve-KnobPassthrough $Knobs @((Get-BoardOwnedEnvironment).Keys)
if ($resolvedKnobs.Count -gt 0) {
    Write-Host ("arm passthrough: " + (($resolvedKnobs.GetEnumerator() |
        ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', '))
}

$executablePath = [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Executable))
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    $executablePath = [IO.Path]::GetFullPath($Executable)
}
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "Executable not found: $Executable"
}

$table = Get-FixtureTable
if ($Fixtures.Count -gt 0) {
    $selected = Resolve-FixtureSelection $Fixtures @($table.name)
    $table = @($table | Where-Object { $selected -contains $_.name })
}

foreach ($fixture in $table) {
    $source = Get-ContainedPath $benchRoot $fixture.folder
    if (-not (Test-Path -LiteralPath $source -PathType Container)) { throw "Missing fixture: $source" }
    $null = Get-FixtureCdIdentity $fixture
}

if ([string]::IsNullOrWhiteSpace($ResultsDirectory)) {
    $suffix = if ([string]::IsNullOrWhiteSpace($Label)) { "" } else { "-$Label" }
    $ResultsDirectory = Join-Path $benchRoot "results" `
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
        $recorded = if ($invariants.Contains($fixture.name)) { $invariants[$fixture.name] } else { $null }
        $row = Invoke-Fixture $fixture $executablePath $scratchRoot $profileArchive $recorded
        Complete-RowPins $fixture $row $invariants $RecordInvariants.IsPresent $Force.IsPresent

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
    knobs            = $resolvedKnobs
    recorded_at      = (Get-Date).ToString("o")
    executable       = $executablePath
    executable_sha256 = Get-FileSha256 $executablePath
    selected_fixtures = @($table.name)
    rows             = $rows
}
$jsonPath = Join-Path $ResultsDirectory "scoreboard.json"
$summary | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath $jsonPath -Encoding utf8

$markdown = @(Get-ScoreboardMarkdown $rows $Label $Arm $OneLookupStore $OneLookupLoad `
        $resolvedKnobs)
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
