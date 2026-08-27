# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The chain-requirement entry-check ladder: THREE cells, EIGHT rows, one binary.

.DESCRIPTION
`run-fixture-scoreboard.ps1` REMOVES every IZARRAVM_* variable from the child, so
it can only run a knob's DEFAULT arm. This ladder needs three explicit arms, so it
invokes the emulator directly.

THE THREE CELLS. A fourth (slice armed with the knob off) is deliberately absent:
its job is to prove the slice inert when disarmed, which is a COUNTER question
answered by identity against cell 1, not a wall question worth legs.

  cell    SEGMENT_RETIRE_GOVERNOR   CHAIN_ENTRY_CHECK
  cap     cap                       0     what we are replacing
  off     off                       0     THE GATE
  armed   cap                       1     the slice, fixing the SHIPPED config

THE GATE: the slice must BEAT the `off` arm on every row. `off` is the fallback we
take if the slice fails, so it, not `cap`, is the competitor. tombraid-loader's
floor is its own `off` figure from THIS binary; retaining its `cap` win is the
goal, not the gate.

BAR 5 IS READ FIRST, BEFORE ANY WALL NUMBER. The narrowing is deliberately NOT
knob-gated (design 7, override in 16), so cells `cap` and `off` are NOT
byte-identical to main on the compile side. `link_refusals.segment_layout` is the
ONE counter allowed to differ, and only DOWNWARD, by no more than
`chain_requirement_narrowed` can account for. Two-directional or unexplained stops
the slice.

DO NOT baseline against `.bench/results/retire-governor-blast-20260825/`. That
survey was measured on `main` and ranks the effect; it does not grade it. All three
cells come fresh from this binary and the two sets are never quoted across.

CONTAMINATION IS MEASURED, NOT ASSUMED. The 2026-08-24 board was contaminated by an
agent and read four rows 8-16% slow; the PIT ladder of the same night came back
24/24 dirty on one row with a 2x outlier. Every leg records whether a builder was
alive in its window.
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` (the shape
# an outer PowerShell produces from `-Fixtures @('a','b')`) ran ONE row of a
# two-row sweep and EXITED 0. With positional binding off, the stray token is a
# binder error before one line of this script runs. The safe multi-row spelling
# is the COMMA string: `-Rows doom-486,wolf3d-486`. Resolve-RowSelection splits
# it and validates every name against the fixture table.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory = $true, ParameterSetName = "Run")][string]$Executable,
    [Parameter(Mandatory = $true, ParameterSetName = "Run")][string]$OutDir,
    [int]$Rounds = 2,
    [string[]]$Rows = @(
        "prince-486", "tombraid-loader-586", "doom-486", "wolf3d-486",
        "wolf3d-586", "duke3d-586-short", "nascar-586", "gp2-586"
    ),
    # Resolve -Rows, print the selection, exit 0. Exists so the self-test's
    # green control can prove a well-formed invocation binds without running a
    # leg. Run-set arguments still have to be supplied; dummies are fine.
    [switch]$BindCheck,
    [Parameter(Mandatory = $true, ParameterSetName = "SelfTest")][switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))

# The benchmark workspace. See scripts/run-fixture-scoreboard.ps1's Resolve-BenchRoot
# for the full rule: IZARRAVM_BENCH_ROOT overrides <repo>/.bench, unset and empty both
# mean the default, and a set-but-missing directory is a hard error rather than a
# silent fallback to the wrong fixture set.
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
# The fixture table sits ABOVE the -SelfTest / -BindCheck dispatch because the
# row resolver validates against its keys, and the dispatch has to exit before
# the OutDir side effects below so a self-test child with a dummy -OutDir never
# creates a stray directory.
#
# Arguments and cycle budgets are the scoreboard's, verbatim. The recorded
# invariants were measured under exactly these, so a changed persona or memory
# size invalidates them silently instead of failing.
$fixtures = @{
    "prince-486"          = @{ folder = "prince_c"; cycles = "4000000000"
        arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
        injection = @("--inject-keys", ("400000000:{shift};600000000:{shift};" +
                "800000000:{shift};1000000000:{shift};1200000000:{shift};" +
                "1400000000:{shift};1600000000:{+right}"))
    }
    "tombraid-loader-586" = @{ folder = "tombraid_loader_c"; cycles = "500000000"
        arguments = @("--cpu", "586", "--memory-mib", "64"); injection = @()
    }
    "doom-486"            = @{ folder = "jemmex_doom_c"; cycles = "8000000000"
        arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega"); injection = @()
    }
    # The wolf3d Enter is a LITERAL NEWLINE, not "{enter}": the scoreboard's
    # string is "2000000000:" followed by a bare LF (0x0A, verified with od -c,
    # no CR). PowerShell's `n is LF, so this reproduces it byte for byte. One
    # Enter at the signon's "Press a key" is what gets the game past its signon;
    # without it every wolf3d number measures an out-of-memory CRASH LOOP.
    "wolf3d-486"          = @{ folder = "wolf3d_c"; cycles = "8000000000"
        arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
        injection = @("--inject-keys", "2000000000:`n")
    }
    # 12e9 = 72 guest seconds, so the end lands INSIDE demo playback, past the
    # ~35 guest seconds of startup plus rotation.
    "wolf3d-586"          = @{ folder = "wolf3d_c"; cycles = "12000000000"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        injection = @("--inject-keys", "2000000000:`n")
    }
    "duke3d-586-short"    = @{ folder = "duke3d_short_c"; cycles = "33200000000"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega"); injection = @()
    }
    # The LONG duke row. Excluded from the exploratory ladder at ~205 s a leg, but
    # the standing rule is that it runs before any merge decision -- the short row
    # ladders candidates, it does not replace this one. It scores itself through
    # DUKEMARK, so its report is an invariant in its own right.
    "duke3d-586"          = @{ folder = "duke3d_c"; cycles = "79680000000"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega"); injection = @()
    }
    "nascar-586"          = @{ folder = "nascar1_c"; cycles = "4980000000"
        arguments = @("--cpu", "586", "--memory-mib", "64"); injection = @()
    }
    "gp2-586"             = @{ folder = "gp2_c"; cycles = "13280000000"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        injection = @("--inject-mouse", ("3320000000:home;3652000000:move:320,386;" +
                "3984000000:click;4648000000:move:0,-115;5146000000:click;" +
                "5976000000:move:-273,181;6474000000:click"))
    }
}

# Parse -Rows into a validated list of row names. It splits on the comma ITSELF
# because `pwsh -File ... -Rows a,b` binds ONE string "a,b" to the [string[]]
# parameter. The two-token shape (`-Rows a b`) never gets here: PositionalBinding
# is off for the whole script, so the binder rejects the second token first.
# Copied from scripts/run-fixture-scoreboard.ps1's Resolve-FixtureSelection.
function Resolve-RowSelection([string[]]$Specification, [string[]]$KnownNames) {
    $entries = @()
    foreach ($element in @($Specification)) {
        if ($null -eq $element) {
            throw "-Rows contains a null entry. Name each row, comma-separated."
        }
        $entries += ([string]$element).Split(',')
    }
    $selected = @()
    foreach ($entry in $entries) {
        $name = ([string]$entry).Trim()
        if ($name -eq "") {
            throw ("-Rows contains an empty entry. A stray comma would silently " +
                "shrink the sweep, so it is refused instead.")
        }
        if (@($KnownNames) -notcontains $name) {
            throw "Unknown row '$name'. Known: $(@($KnownNames) -join ', ')"
        }
        if ($selected -contains $name) {
            throw ("-Rows names '$name' more than once. The ladder runs each row " +
                "once, so a repeat would report fewer rows than the caller asked for.")
        }
        $selected += $name
    }
    return ,$selected
}

function Assert-BinderSelfTestEqual($Actual, $Expected, [string]$Message) {
    if ($Actual -ne $Expected) {
        throw "self-test failed: $Message (expected $Expected, got $Actual)"
    }
}

function Assert-BinderSelfTestThrows([scriptblock]$Action, [string]$Expected,
    [string]$Message) {
    $failure = $null
    try { $null = & $Action } catch { $failure = $_.Exception.Message }
    if ($null -eq $failure) { throw "self-test failed: $Message did not throw" }
    if (-not $failure.Contains($Expected, [StringComparison]::Ordinal)) {
        throw "self-test failed: $Message threw '$failure', expected '$Expected'"
    }
}

# Half drives Resolve-RowSelection directly. The other half spawns this very
# script under `pwsh -File` with the mangled two-token shape, because that
# failure happens in the parameter binder -- before any function here runs --
# and only a real child invocation can prove the guard fires there. The
# campaign rule applies: the guard must go RED on the broken input, and a green
# control must show the child harness works.
function Invoke-BinderGuardSelfTest {
    $known = @($fixtures.Keys)

    $split = Resolve-RowSelection @("doom-486,wolf3d-486") $known
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "doom-486" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "wolf3d-486" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" doom-486 , wolf3d-486") $known
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486,no-such-row") $known } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486,") $known } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("doom-486", "doom-486") $known } `
        "more than once" "a row named twice"

    $pwshExecutable = (Get-Process -Id $PID).Path
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-bindsel-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
                "-Rows", "doom-486", "wolf3d-486", "-BindCheck")
            RedirectStandardOutput = $outputPath
            RedirectStandardError  = $failurePath
            PassThru               = $true
            NoNewWindow            = $true
        }
        # RED: the two-token shape must be a binder error, never a one-row run.
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "self-test failed: the mangled -Rows child never exited"
        }
        if ($process.ExitCode -eq 0) {
            throw ("self-test failed: the mangled two-token -Rows invocation exited 0. " +
                "The silent-subset hazard is back: the second row bound positionally.")
        }
        $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
        if ($failureText -notmatch 'wolf3d-486') {
            throw ("self-test failed: the mangled -Rows child failed, but not on the " +
                "stray token. stderr: $failureText")
        }

        # GREEN control: the comma spelling of the same selection must bind and
        # resolve, or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
            "-Rows", "doom-486,wolf3d-486", "-BindCheck")
        $process = Start-Process @start
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill($true) } catch { }
            throw "self-test failed: the -BindCheck control child never exited"
        }
        if ($process.ExitCode -ne 0) {
            $failureText = [string](Get-Content -LiteralPath $failurePath -Raw)
            throw ("self-test failed: the well-formed -BindCheck control exited " +
                "$($process.ExitCode); the red row above is therefore meaningless. " +
                "stderr: $failureText")
        }
        $listing = [string](Get-Content -LiteralPath $outputPath -Raw)
        if ($listing -notmatch 'wolf3d-486') {
            throw "self-test failed: the -BindCheck control did not echo the selection"
        }
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-chain-entry-check self-test passed"
}

if ($SelfTest) {
    Invoke-BinderGuardSelfTest
    exit 0
}

$Rows = Resolve-RowSelection $Rows @($fixtures.Keys)
if ($BindCheck) {
    Write-Host ("bind-check ok: rows " + ($Rows -join ", "))
    exit 0
}

$benchRoot = Resolve-BenchRoot $repositoryRoot

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$cells = @{
    "cap"   = @{ governor = "cap"; chain = "0" }
    "off"   = @{ governor = "off"; chain = "0" }
    "armed" = @{ governor = "cap"; chain = "1" }
}

function Get-BuilderCount {
    $builders = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -match '^(cargo|rustc|link|lld-link)$' }
    if ($null -eq $builders) { return 0 }
    return @($builders).Count
}

# The builder check above only sees OUR OWN toolchain. It is blind to everything
# else on the machine -- a browser, a mail client, an indexer, anything the person
# at the keyboard happens to open. That gap was found the hard way on 2026-08-25,
# when the owner checked email during a leg and the leg came back 5.5 s slow with
# `dirty = false`.
#
# This samples the machine's own CPU instead of guessing at process names, so it
# catches load whatever its source. Sampled once per leg, cheaply, and recorded
# rather than acted on: a leg is FLAGGED for judgement, never silently dropped.
function Get-HostCpuPercent {
    try {
        $sample = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -ErrorAction Stop |
            Where-Object Name -eq '_Total'
        if ($null -eq $sample) { return -1 }
        return [int]$sample.PercentProcessorTime
    }
    catch { return -1 }
}

# Every knob the board sets explicitly. The governor MUST be set explicitly and
# non-empty: unset lands on `cap` and `""` is OFF, so nulling it is the dangerous
# direction. The observers are REMOVED, not emptied: an empty value leaves the
# variable SET and several readers arm on var_os()/is_some().
function Set-CellEnvironment([string]$Governor, [string]$Chain) {
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
    $env:IZARRAVM_SEGMENT_RETIRE_GOVERNOR = $Governor
    $env:IZARRAVM_CHAIN_ENTRY_CHECK = $Chain
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

function Get-Field($Object, [string]$Name) {
    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

# `direct_stalls.link_refusals` is a LIST of {reason, count} objects, not a map.
# Selecting by reason keeps this working if the enum gains a variant, where a
# positional index would silently read the wrong row.
function Get-LinkRefusal($Stalls, [string]$Reason) {
    $list = Get-Field $Stalls "link_refusals"
    if ($null -eq $list) { return $null }
    foreach ($entry in $list) {
        if ((Get-Field $entry "reason") -eq $Reason) { return Get-Field $entry "count" }
    }
    return $null
}

function Invoke-Leg([string]$Row, [string]$Cell, [int]$Round) {
    $fixture = $fixtures[$Row]
    $stamp = "$Row-$Cell-r$Round"
    $copy = Join-Path $OutDir "work-$stamp"
    if (Test-Path $copy) { Remove-Item -Recurse -Force $copy }
    Copy-Item -Recurse (Join-Path $benchRoot $fixture.folder) $copy
    $profilePath = Join-Path $OutDir "$stamp.json"

    Set-CellEnvironment $cells[$Cell].governor $cells[$Cell].chain

    $arguments = @()
    $arguments += $fixture.arguments
    $arguments += @("--hdd-folder", $copy)
    $arguments += @("--cycles", $fixture.cycles)
    $arguments += @("--profile-json", $profilePath)
    $arguments += $fixture.injection

    $before = Get-BuilderCount
    $cpuBefore = Get-HostCpuPercent
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $Executable @arguments | Out-Null
    $watch.Stop()
    $after = Get-BuilderCount
    $cpuAfter = Get-HostCpuPercent
    if ($LASTEXITCODE -ne 0) { throw "$stamp exited $LASTEXITCODE" }

    Remove-Item -Recurse -Force $copy
    $report = Get-Content $profilePath -Raw | ConvertFrom-Json
    $perf = Get-Field $report "perf"
    $stalls = Get-Field $report "direct_stalls"
    $total = Get-Field $perf "instructions"
    $direct = Get-Field $perf "jit_direct_insns"

    [pscustomobject]@{
        row        = $Row
        cell       = $Cell
        round      = $Round
        dirty      = ($before -gt 0 -or $after -gt 0)
        # The emulator is single-threaded, so on a 32-core host its own share is
        # ~3%. Anything much above that is somebody else's work. Recorded, not
        # acted on -- min-wall already discards a slow leg, and a leg flagged
        # here is a prompt to look, not a verdict.
        cpu_before = $cpuBefore
        cpu_after  = $cpuAfter
        wall_s     = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        guest_s    = Get-Field $report "guest_seconds"
        insns      = $total
        bus        = Get-Field $report "raw_bus_clocks"
        entries    = Get-Field $perf "jit_direct_entries"
        native_pct = if ($total) { [math]::Round(100.0 * $direct / $total, 3) } else { $null }
        # BAR 5: the one counter allowed to differ between cap/off and main, and
        # only downward, bounded by what the narrowing can account for.
        #
        # `link_refusals` is a LIST of {reason, count}, not an object, so the
        # row is selected by reason. Measured on main's wolf3d-586 board profile:
        # segment_layout = 136,608,976.
        refus_seg  = Get-LinkRefusal $stalls "segment_layout"
        # New in the slice; ABSENT on main, where Get-Field returns $null. That
        # is the correct reading, not a failure.
        narrowed   = Get-Field $stalls "chain_requirement_narrowed"
        declines   = Get-Field $stalls "data_segment_link_declines"
        suppressed = Get-Field $stalls "data_segment_retires_suppressed"
    }
}

$legs = [Collections.Generic.List[object]]::new()
foreach ($row in $Rows) {
    for ($round = 1; $round -le $Rounds; $round++) {
        # Rotate the cell order per round so a monotone host drift does not land
        # on the same cell every time.
        $order = switch ($round % 3) {
            1 { @("cap", "off", "armed") }
            2 { @("armed", "off", "cap") }
            0 { @("off", "armed", "cap") }
        }
        foreach ($cell in $order) {
            $leg = Invoke-Leg $row $cell $round
            $legs.Add($leg)
            $flag = if ($leg.dirty) { "  <-- BUILDER ACTIVE, leg suspect" } else { "" }
            "{0,-20} {1,-6} r{2}  wall {3,9:N3}s  native {4,7:N2}%  entries {5,12:N0}{6}" -f `
                $leg.row, $leg.cell, $leg.round, $leg.wall_s, $leg.native_pct, $leg.entries, $flag |
                Write-Host
            $legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")
        }
    }
}

Write-Host ""
Write-Host "=== BAR 5 FIRST: jit_direct_reject_data_segment, may move DOWN only ==="
foreach ($row in $Rows) {
    foreach ($cell in @("cap", "off", "armed")) {
        $sub = @($legs | Where-Object { $_.row -eq $row -and $_.cell -eq $cell })
        if ($sub.Count -eq 0) { continue }
        # `chain_requirement_narrowed` is a LIST split by LinkClearCause, so it
        # renders as an object unless the causes are summed and named.
        $narrowedText = "absent"
        if ($null -ne $sub[0].narrowed) {
            $parts = @($sub[0].narrowed | Where-Object { (Get-Field $_ "count") -gt 0 } |
                ForEach-Object { "{0}={1:N0}" -f (Get-Field $_ "cause"), (Get-Field $_ "count") })
            $narrowedText = if ($parts.Count -gt 0) { $parts -join "," } else { "all-zero" }
        }
        "{0,-20} {1,-6} link_refusals[segment_layout] {2,16:N0}  narrowed {3,14}  declines {4,12:N0}" -f `
            $row, $cell, $sub[0].refus_seg, $narrowedText, $sub[0].declines | Write-Host
    }
}

Write-Host ""
Write-Host "=== THE GATE: armed must BEAT off on every row ==="
foreach ($row in $Rows) {
    $best = @{}
    foreach ($cell in @("cap", "off", "armed")) {
        $clean = @($legs | Where-Object { $_.row -eq $row -and $_.cell -eq $cell -and -not $_.dirty })
        $pool = if ($clean.Count -gt 0) { $clean } else { @($legs | Where-Object { $_.row -eq $row -and $_.cell -eq $cell }) }
        if ($pool.Count -eq 0) { $best[$cell] = $null; continue }
        $best[$cell] = ($pool | Measure-Object wall_s -Minimum).Minimum
    }
    if ($null -eq $best["off"] -or $null -eq $best["armed"]) {
        "{0,-20} INCOMPLETE" -f $row | Write-Host
        continue
    }
    $verdict = if ($best["armed"] -lt $best["off"]) { "PASS" } else { "FAIL" }
    "{0,-20} cap {1,9:N3}  off {2,9:N3}  armed {3,9:N3}   armed/off {4,6:N4}  {5}" -f `
        $row, $best["cap"], $best["off"], $best["armed"], ($best["armed"] / $best["off"]), $verdict |
        Write-Host
}

$dirtyLegs = @($legs | Where-Object dirty)
Write-Host ""
if ($dirtyLegs.Count -gt 0) {
    "CONTAMINATION: {0} of {1} legs ran with a builder active." -f $dirtyLegs.Count, $legs.Count | Write-Host
    $dirtyLegs | ForEach-Object { "  {0} {1} r{2}" -f $_.row, $_.cell, $_.round } | Write-Host
}
else {
    Write-Host "No builder was active during any leg."
}
