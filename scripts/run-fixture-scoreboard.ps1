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
function Test-ProfileBands($Profile, $Bands) {
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
        $values["band_" + ($band.path -replace '\.', '_')] = $number
        if ($number -lt $band.min -or $number -gt $band.max) {
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
            frame_sha256_allowed = @(
                "e446305c30949f54a3089e24bc5db274158f7290203c8ad54b62c42897ed32f7"
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

    $width = [int]$tokens[1]
    $height = [int]$tokens[2]
    $maxValue = [int]$tokens[3]
    if ($width -le 0 -or $height -le 0 -or $maxValue -ne 255) { return $null }
    $pixels = $width * $height
    if ($bytes.Length - $cursor -lt $pixels * 3) { return $null }

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
        $arguments += @("--result-ppm", $PpmPath)
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
function Invoke-AnchorRun($Fixture, [string]$ExecutablePath, [string]$ScratchRoot) {
    $contract = Get-FrameContract $Fixture
    $stamp = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $workingCopy = Join-Path $ScratchRoot "$($Fixture.name)-anchor-$stamp"
    $profilePath = Join-Path $ScratchRoot "$($Fixture.name)-anchor-$stamp.json"
    $ppmPath = Join-Path $ScratchRoot "$($Fixture.name)-anchor-$stamp.ppm"

    $result = @{ sha256 = $null; display = $null; wall_s = 0.0; failure = $null }
    Copy-Fixture (Join-Path $benchRoot $Fixture.folder) $workingCopy
    try {
        $start = @{
            FilePath               = $ExecutablePath
            ArgumentList           = (Get-FixtureArguments $Fixture $workingCopy `
                    $contract.anchorCycles $profilePath $ppmPath)
            NoNewWindow            = $true
            PassThru               = $true
            RedirectStandardOutput = (Join-Path $ScratchRoot "$($Fixture.name)-anchor-$stamp.out")
            RedirectStandardError  = (Join-Path $ScratchRoot "$($Fixture.name)-anchor-$stamp.err")
            Environment            = Get-RowEnvironment
        }
        $wall = [Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process @start
        if ($ProcessorIndex -ge 0) {
            try { $process.ProcessorAffinity = [IntPtr]([int64]1 -shl $ProcessorIndex) } catch { }
        }
        if (-not $process.WaitForExit($HostTimeoutSeconds * 1000)) {
            try { $process.Kill($true) } catch { }
            $result.failure = "the anchor run exceeded $HostTimeoutSeconds seconds"
            return $result
        }
        $wall.Stop()
        $result.wall_s = [math]::Round($wall.Elapsed.TotalSeconds, 3)

        $result.sha256 = Get-FileSha256 $ppmPath
        if ($null -eq $result.sha256) {
            $result.failure = ("the anchor run wrote no PPM at " +
                "$($contract.anchorCycles) cycles (the emulator crashed, or never started)")
            return $result
        }
        # The display PATH at the anchor is part of the contract: tombraid's
        # anchor is a VGA text page and nascar's is a Margo LFB frame, and a row
        # that produced the right bytes through the wrong path has still changed.
        if (Test-Path -LiteralPath $profilePath) {
            try {
                $anchorProfile = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json
                $result.display = $anchorProfile.active_display
            } catch { }
        }
    } finally {
        Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
    }
    return $result
}

function Invoke-Fixture($Fixture, [string]$ExecutablePath, [string]$ScratchRoot,
    [string]$KeepProfilesIn) {
    $fixtureSource = Join-Path $benchRoot $Fixture.folder
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

    $arguments = Get-FixtureArguments $Fixture $workingCopy $Fixture.cycles `
        $profilePath $(if ($Fixture.resultPpm) { $ppmPath } else { $null })
    $environment = Get-RowEnvironment

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
        # The -Knobs passthrough exactly as it was armed for THIS row, resolved
        # rather than raw, so a leg can be audited after the fact instead of
        # taken on trust. `{}` means nothing was passed through. A knob armed to
        # the empty string appears as `"NAME": ""` and one that was never named
        # is absent from the object -- the same distinction the child sees.
        knobs            = (Resolve-KnobPassthrough $Knobs `
                                @((Get-BoardOwnedEnvironment).Keys))
        exit_code        = $exitCode
        host_wall_s      = [math]::Round($wallStart.Elapsed.TotalSeconds, 3)
        background_load  = $backgroundLoad
        background_peak  = $peakLoad
        load_samples     = $waited.samples.Count
        contaminated     = $false
        invariant        = "unchecked"
        notes            = @()
    }

    # A board row must run with NO instrument armed. Every instrument announces
    # itself on stderr (e.g. "riprofile: sampling armed"), so any stderr output
    # at all fails the row loudly instead of silently taxing the measurement.
    # This is the regression guard for the 2026-08-15 empty-env-string incident:
    # an empty IZARRAVM_RIP_PROFILE armed the sampler on every row.
    #
    # One exception, shared with the corpus sweep's policy: open-bus port
    # diagnostics are DATA, not failures. A guest probing for hardware
    # touches unclaimed ports as a matter of course, and the emulator's
    # answer (float, log) is the hardware answer. First seen on a board row
    # 2026-08-17: the DSP settle fix let Prince of Persia detect the Sound
    # Blaster, and its sound init strobes 0x8E00-0x8E03.
    $stderrPath = $start.RedirectStandardError
    if (Test-Path -LiteralPath $stderrPath) {
        $offending = @(Get-Content -LiteralPath $stderrPath |
            Where-Object { $_ -notmatch '^open-bus: ' -and $_ -notmatch '^\s*$' })
        if ($offending.Count -gt 0) {
            $stderrHead = ($offending | Select-Object -First 3) -join "; "
            $result.invariant = "FAIL"
            $result.notes += ("row wrote to stderr (an instrument armed, or the emulator " +
                "complained): $stderrHead")
            Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
            return $result
        }
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

    $contract = Get-FrameContract $Fixture
    if ($Fixture.resultPpm -and $null -eq $contract) {
        $hash = Get-FileSha256 $ppmPath
        if ($null -eq $hash) {
            $failures += "no result PPM was written"
        } else {
            $result.frame_sha256 = $hash
        }

        $allowedProperty = $Fixture.PSObject.Properties['frame_sha256_allowed']
        if ($null -ne $allowedProperty) {
            $allowed = @($allowedProperty.Value)
            $result.frame_sha256_allowed = $allowed
            if ($exitCode -ne 0) { $failures += "emulator exit code is $exitCode, expected 0" }
            if ($null -ne $hash -and -not (Test-Sha256Allowed $allowed $hash)) {
                $failures += "frame hash $hash is not in the allowed set"
            }

            $stats = Get-PpmFrameStats $ppmPath
            if ($null -eq $stats) {
                $failures += "the result PPM did not parse as a P6 frame"
            } else {
                $result.final_frame_width = $stats.width
                $result.final_frame_height = $stats.height
                if ($stats.width -ne $Fixture.expected_width -or
                    $stats.height -ne $Fixture.expected_height) {
                    $failures += ("the final frame is {0}x{1}, expected {2}x{3}" -f
                        $stats.width, $stats.height, $Fixture.expected_width,
                        $Fixture.expected_height)
                }
            }

            $result.final_display = $profile.active_display
            $result.final_video_mode = $profile.legacy_video_mode
            if ($profile.active_display -ne $Fixture.expected_display) {
                $failures += ("the final display is '$($profile.active_display)', expected " +
                    "'$($Fixture.expected_display)'")
            }
            if ($profile.legacy_video_mode -ne $Fixture.expected_video_mode) {
                $failures += ("the final video mode is '$($profile.legacy_video_mode)', " +
                    "expected '$($Fixture.expected_video_mode)'")
            }
            if ($profile.stop.kind -ne "cycle_limit" -or
                [uint64]$profile.stop.requested -ne $Fixture.cycles) {
                $failures += ("the run stopped as '$($profile.stop.kind)' at " +
                    "$($profile.stop.requested), expected cycle_limit at $($Fixture.cycles)")
            }
            $stdout = if (Test-Path -LiteralPath $stdoutPath) {
                Get-Content -LiteralPath $stdoutPath -Raw
            } else { "" }
            if ($stdout -notlike "*$($Fixture.stdout_contains)*") {
                $failures += "stdout did not contain '$($Fixture.stdout_contains)'"
            }
        }
    }

    # FRAME CONTRACT rows. See New-FrameContract for the whole argument; the
    # short version is that the end-of-budget frame hash is GONE and what
    # replaces it is a cadence-stable anchor hash plus content bands.
    #
    # Everything measured here is graded in the driver against the sidecar,
    # except the display class and the stop reason, which are fixture constants
    # and are graded right here.
    if ($null -ne $contract) {
        $hash = Get-FileSha256 $ppmPath
        if ($null -eq $hash) {
            $failures += "no result PPM was written"
        } else {
            # A MEASUREMENT, never asserted. It is what an attribution cycle
            # starts from when a band does move, and it is what makes the
            # scoreboard.json of two builds diffable by eye -- but grading it is
            # exactly the treadmill this row was rewritten to escape.
            $result.final_frame_sha256 = $hash
        }

        $stats = Get-PpmFrameStats $ppmPath
        if ($null -eq $stats) {
            $failures += "the result PPM did not parse as a P6 frame"
        } else {
            $result.final_nonblack_pct = $stats.non_black_pct
            $result.final_distinct_colors = $stats.distinct_colors
            $result.final_frame_width = $stats.width
            $result.final_frame_height = $stats.height
            if ($stats.width -ne $contract.width -or $stats.height -ne $contract.height) {
                $failures += ("the final frame is $($stats.width)x$($stats.height), " +
                    "expected $($contract.width)x$($contract.height)")
            }
        }

        # The display CLASS: which path painted the frame, at what depth and in
        # what mode. Cadence cannot move any of it, and it is what separates
        # "the game is rendering" from "the guest fell back to a text page" --
        # a distinction the pixel bands alone do not draw, because a DOS screen
        # full of error text is neither black nor single-coloured.
        $result.final_display = $profile.active_display
        if ($profile.active_display -ne $contract.display) {
            $failures += ("the final frame came from display path " +
                "'$($profile.active_display)', expected '$($contract.display)'")
        }
        $margo = $profile.PSObject.Properties['margo_display']
        if ($null -eq $margo -or $null -eq $margo.Value) {
            $failures += "the profile carries no margo_display block for the final frame"
        } else {
            $result.final_bpp = $margo.Value.bpp
            $result.final_mode = $margo.Value.mode
            if ([int]$margo.Value.bpp -ne $contract.bpp -or
                $margo.Value.mode -ne $contract.mode) {
                $failures += ("the final frame is bpp $($margo.Value.bpp) mode " +
                    "$($margo.Value.mode), expected bpp $($contract.bpp) mode " +
                    "$($contract.mode)")
            }
        }

        # The budget is what ends these two rows. A `test_exit` here would mean
        # the guest quit to DOS and poked the exit port, i.e. the game died;
        # anything else means it never reached the budget at all.
        $result.stop_kind = $profile.stop.kind
        if ($profile.stop.kind -ne "cycle_limit") {
            $failures += ("the run stopped as '$($profile.stop.kind)', expected " +
                "'cycle_limit' -- the guest did not run the whole budget")
        } elseif ([uint64]$profile.stop.requested -ne $Fixture.cycles) {
            $failures += ("the run stopped at $($profile.stop.requested) cycles, " +
                "expected the fixture's $($Fixture.cycles)")
        }

        $anchor = Invoke-AnchorRun $Fixture $ExecutablePath $ScratchRoot
        $result.anchor_cycles = $contract.anchorCycles
        $result.anchor_wall_s = $anchor.wall_s
        if ($null -ne $anchor.failure) {
            $failures += $anchor.failure
        } else {
            $result.anchor_frame_sha256 = $anchor.sha256
            $result.anchor_display = $anchor.display
            if ($anchor.display -ne $contract.anchorDisplay) {
                $failures += ("the anchor frame came from display path " +
                    "'$($anchor.display)', expected '$($contract.anchorDisplay)'")
            }
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

    # PROFILE BANDS. A row may pin dotted profile-JSON fields to [min, max]
    # ranges. The Tyrian rows use them for the guest's own audio-clock
    # liveness: MPU-401 MIDI byte counts and IRQ0 edge counts collapse to
    # near-zero when the 70 Hz tick starves (the 2026-08-28 PIT write-edge
    # bug), and no frame hash can see that. Bands are deliberately WIDE --
    # they exist to catch collapse and runaway, not cadence drift; the
    # derivation of each row's numbers is beside the row in the table.
    # A row with bands also demands the cycle_limit stop: every band was
    # derived over the full budget, so a short run would read as collapse.
    $bandsProperty = $Fixture.PSObject.Properties['profileBands']
    if ($null -ne $bandsProperty -and $bandsProperty.Value) {
        if ($profile.stop.kind -ne "cycle_limit") {
            $failures += ("the run stopped as '$($profile.stop.kind)', expected " +
                "'cycle_limit' -- the profile bands were derived over the full budget")
        }
        $graded = Test-ProfileBands $profile $bandsProperty.Value
        foreach ($entry in $graded.values.GetEnumerator()) {
            $result | Add-Member -Force -NotePropertyName $entry.Key -NotePropertyValue $entry.Value
        }
        $failures += $graded.failures
    }

    $result.invariant = if ($failures.Count -eq 0) { "pass" } else { "FAIL" }
    $result.notes += $failures

    Remove-Item -LiteralPath $workingCopy -Recurse -Force -ErrorAction SilentlyContinue
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
        $row = Invoke-Fixture $fixture $executablePath $scratchRoot $profileArchive

        # Compare or record the framebuffer hash. `$row` is still an ordered
        # hashtable here, so membership is Contains and NOT
        # `PSObject.Properties.Name`, which on a hashtable enumerates Count and
        # Keys rather than the entries and silently answers false for every
        # lookup. That mistake made this whole comparison dead code once already.
        $allowedHashProperty = $fixture.PSObject.Properties['frame_sha256_allowed']
        if ($row.Contains("frame_sha256") -and $null -eq $allowedHashProperty) {
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
        } elseif ($RecordInvariants -and $null -ne $allowedHashProperty) {
            $row.notes += "allowed frame set is hand-curated and was not re-recorded"
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

        # FRAME CONTRACT grading. Same sidecar, same -RecordInvariants / -Force
        # machinery and the same three outcomes as the two blocks above.
        #
        # The split between what -RecordInvariants will write and what it will
        # not is the whole discipline of this design:
        #
        #   RECORDED from a run: the anchor hash (an exact value has an obvious
        #   first observation) and the instruction centre (a point plus a fixed
        #   tolerance, exactly like the DUKEMARK sample pin).
        #
        #   NEVER recorded from a run: the coverage and colour BANDS. A band
        #   derived from a single sample is a band of width zero around whatever
        #   that run happened to do, which is the fragile invariant this row was
        #   rewritten to escape wearing a different hat. They are derived by hand
        #   from a phase-spread measurement -- several budgets either side of the
        #   pinned one -- and the derivation for the current numbers is written
        #   out in the sidecar note. A row missing them reads `unpinned` and says
        #   so, which is a loud, correct, non-vacuous state.
        if ($row.Contains("anchor_frame_sha256") -or $row.Contains("final_nonblack_pct")) {
            $contract = Get-FrameContract $fixture
            $recorded = if ($invariants.Contains($fixture.name)) {
                $invariants[$fixture.name]
            } else { $null }
            $key = "anchor_frame_sha256"
            # @() around the WHOLE if-expression. An if used as an expression
            # sends its result down the pipeline, and the pipeline unrolls a
            # one-element array into the element itself -- so the inner @() is
            # not enough and a single recorded anchor arrives here as a bare
            # string with no .Count. Only the multi-phase row would have looked
            # right, which is the worst way for this to be wrong.
            $pinnedAnchors = @(if ($null -ne $recorded -and $recorded.Contains($key)) {
                    $recorded[$key]
                } else { @() })

            if ($RecordInvariants) {
                if (-not $invariants.Contains($fixture.name)) {
                    $invariants[$fixture.name] = @{}
                }
                if ($row.Contains("anchor_frame_sha256")) {
                    if ($pinnedAnchors -contains $row.anchor_frame_sha256) {
                        $row.notes += "anchor frame hash already recorded"
                    } elseif ($pinnedAnchors.Count -eq 0) {
                        $invariants[$fixture.name][$key] = @($row.anchor_frame_sha256)
                        $row.notes += "anchor frame hash recorded"
                    } elseif (-not $Force) {
                        throw ("$($fixture.name) already has $($pinnedAnchors.Count) " +
                            "recorded anchor frame hash(es) and this run produced a new " +
                            "one. The anchor is the row's only exact-frame invariant; " +
                            "re-recording would erase the evidence of a real change. Pass " +
                            "-Force only if you have established that the move is legitimate.")
                    } elseif ($pinnedAnchors.Count -ge $contract.anchorPhases) {
                        throw ("$($fixture.name) already holds its declared " +
                            "$($contract.anchorPhases) anchor phase(s) and this run " +
                            "produced yet another. The extra phases are enumerated and " +
                            "explained in New-FrameContract; an unexplained one is a real " +
                            "change and -Force will not absorb it. Re-derive the anchor.")
                    } else {
                        $invariants[$fixture.name][$key] = @($pinnedAnchors + $row.anchor_frame_sha256)
                        $row.notes += ("anchor frame hash recorded as phase " +
                            "$($pinnedAnchors.Count + 1) of $($contract.anchorPhases)")
                    }
                }
                if ($row.Contains("instructions")) {
                    $invariants[$fixture.name].final_instructions = $row.instructions
                    if (-not $invariants[$fixture.name].Contains("final_instructions_tolerance")) {
                        $invariants[$fixture.name].final_instructions_tolerance =
                            $frameInstructionTolerance
                    }
                    $row.notes += "final instruction pin recorded ($($row.instructions))"
                }
                # @() around the PIPELINE, not just its source: under StrictMode a
                # one-element Where-Object result is a scalar with no .Count.
                $missing = @(@("final_nonblack_percent_min", "final_nonblack_percent_max",
                        "final_distinct_colors_min", "final_distinct_colors_max") |
                    Where-Object { -not $invariants[$fixture.name].Contains($_) })
                if ($missing.Count -gt 0) {
                    $row.notes += ("content bands NOT recorded (by design): " +
                        ($missing -join ", ") + " must be derived from a phase-spread " +
                        "measurement and written into the sidecar by hand")
                }
            } else {
                if ($row.Contains("anchor_frame_sha256")) {
                    if ($pinnedAnchors.Count -eq 0) {
                        $row.notes += "no recorded anchor frame hash to compare against"
                        if ($row.invariant -eq "pass") { $row.invariant = "unpinned" }
                    } elseif ($pinnedAnchors -notcontains $row.anchor_frame_sha256) {
                        $row.invariant = "FAIL"
                        $row.notes += ("anchor frame hash moved: got " +
                            "$($row.anchor_frame_sha256), expected one of " +
                            ($pinnedAnchors -join ", "))
                    }
                }

                foreach ($band in @(
                        @{ value = "final_nonblack_pct"; min = "final_nonblack_percent_min"
                            max = "final_nonblack_percent_max"; label = "non-black coverage %" }
                        @{ value = "final_distinct_colors"; min = "final_distinct_colors_min"
                            max = "final_distinct_colors_max"; label = "distinct colours" })) {
                    if (-not $row.Contains($band.value)) { continue }
                    $hasBand = $null -ne $recorded -and $recorded.Contains($band.min) -and
                        $recorded.Contains($band.max)
                    if (-not $hasBand) {
                        $row.notes += "no recorded band for $($band.label)"
                        if ($row.invariant -eq "pass") { $row.invariant = "unpinned" }
                        continue
                    }
                    $low = [double]$recorded[$band.min]
                    $high = [double]$recorded[$band.max]
                    $observed = [double]$row[$band.value]
                    if ($observed -lt $low -or $observed -gt $high) {
                        $row.invariant = "FAIL"
                        $row.notes += ("$($band.label) is $observed, outside the band " +
                            "[$low, $high] -- the end-of-budget picture is not the scene " +
                            "this fixture renders")
                    }
                }

                $instructionPin = if ($null -ne $recorded -and
                    $recorded.Contains("final_instructions")) {
                    [double]$recorded.final_instructions
                } else { $null }
                if ($row.Contains("instructions") -and $null -eq $instructionPin) {
                    $row.notes += "no recorded final instruction pin to compare against"
                    if ($row.invariant -eq "pass") { $row.invariant = "unpinned" }
                } elseif ($row.Contains("instructions")) {
                    $tolerance = if ($recorded.Contains("final_instructions_tolerance")) {
                        [double]$recorded.final_instructions_tolerance
                    } else { $frameInstructionTolerance }
                    $drift = [math]::Abs([double]$row.instructions - $instructionPin) /
                        $instructionPin
                    $row.final_instructions_pin = [uint64]$instructionPin
                    $row.final_instructions_drift = [math]::Round($drift, 5)
                    if ($drift -gt $tolerance) {
                        $row.invariant = "FAIL"
                        $row.notes += ("retired instructions $($row.instructions) drifted " +
                            ("{0:P3}" -f $drift) + " from the pin of $([uint64]$instructionPin) " +
                            "against a tolerance of " + ("{0:P1}" -f $tolerance) +
                            " -- the guest did not do this fixture's work")
                    }
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
    knobs            = $resolvedKnobs
    recorded_at      = (Get-Date).ToString("o")
    executable       = $executablePath
    rows             = $rows
}
$jsonPath = Join-Path $ResultsDirectory "scoreboard.json"
$summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $jsonPath -Encoding utf8

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
