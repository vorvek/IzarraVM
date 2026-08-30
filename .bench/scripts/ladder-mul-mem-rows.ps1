# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
A two-arm knob ladder: one binary, both arms, with the null measured in-session.

.DESCRIPTION
THE NAME IS HISTORICAL. This began as the MUL-memory ladder and is now the
general two-arm ladder; `-Knob` names which knob the arms select and defaults
to that first slice. The file keeps its name so the commits and memories that
cite it do not become dangling references -- the same reason the classify arm's
repaired test citations matter.

`run-fixture-scoreboard.ps1` REMOVES every IZARRAVM_* variable from the child
(see its Get-RowEnvironment), so it can only ever run a knob's DEFAULT arm. Arm
work therefore needs a direct invocation, and this is it.

WHY ONE BINARY. Layout variance between two builds of this workspace has been
measured at 3.7% on this box -- larger than most levers. A two-binary comparison
cannot carry a claim that size, so the knob exists and both arms come out of the
same executable.

THE KNOB CONTRACT. `-Knob` must name a knob spelling its arms "0" and "1" and
DEFAULTING OFF, because arm 0 is always the A/B base here. A knob defaulting ON
would make arm 0 the candidate and silently invert every ratio printed below.
Both slice knobs are pinned OFF in `Set-BaseEnvironment`, so a stray
parent-shell value for the one NOT under test cannot ride along in both arms.

THREE GATES, and each answers a different way of being wrong.

* IDENTITY. `guest_s`, `insns`, `bus` and `ticks` must be single-valued across
  every leg. Both slices so far predict this: their emitted forms charge what
  the interpreter charges, so the arms execute the same guest stream. A
  divergence is a real finding, not noise.
* NON-VACUITY. `decode_probes` moving proves the arm reached block formation.
  Probes that did NOT move mean EITHER the knob never took OR this fixture has
  no population for the row -- and wall data cannot separate those, so the gate
  reports both and says a census settles it.
* THE IN-SESSION NULL. Every A/B/B/A round runs each arm TWICE, so the spread
  between one arm's own two legs is a null pair from that round, with zero
  effect by construction. `-NullThreshold` excludes a round that exceeds it.
  This exists because two A/A controls on gp2-586, same binary and estimator,
  two hours apart, read 1.0290 and 0.9993: ONE A/A IS ONE SAMPLE OF THE NULL
  DISTRIBUTION, NOT A FLOOR.

PROVING AN OFF ARM IS OFF. Not from this script, and not from a counter reading
zero -- that is equally consistent with "the knob is off" and "the instrument is
unwired". Take a barrier-census leg per arm (`IZARRAVM_DIRECT_BARRIER_CENSUS=1`,
plain release build) and read the row: present in one arm, ABSENT in the other.
That instrument answers differently under each hypothesis.

ESTIMATOR: min-wall. Process CPU time was TESTED as an alternative on
2026-08-30 and REJECTED -- it tracked wall within 1% (the emulator was never
descheduled, so there was nothing to remove) and its floor was worse. `cpu_s`
and `foreign_s` are still recorded: `foreign_s` is what explains a 15%
between-run difference that the four-name builder count called clean.

READ A NULL RESULT AS A PARK, NOT A FAIL. The most likely null for an admission
slice is RELOCATION -- the exits moving onto the next census head instead of
disappearing, so the block still terminates. Check that row by row against a
two-arm census before calling a null a mystery.
#>

# POSITIONAL BINDING IS OFF for the whole param block. Under `pwsh -File`, a
# [string[]] parameter takes exactly ONE argument token; a second token becomes
# a POSITIONAL argument and lands in the next unbound parameter. Measured
# 2026-08-27 on scripts/run-fixture-scoreboard.ps1: `-Fixtures a b` ran ONE row
# of a two-row sweep and EXITED 0. With positional binding off, the stray token
# is a binder error before one line of this script runs. The safe multi-row
# spelling is the COMMA string: `-Rows gp2-586,nascar-586`.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    [Parameter(Mandatory = $true, ParameterSetName = "Run")][string]$Executable,
    [string]$OutDir = "",
    [int]$Rounds = 4,
    [string[]]$Rows = @("gp2-586"),
    # A/A NULL CONTROL. Both arms run with IZARRAVM_MUL_MEM_ROWS=0 while keeping
    # their 0/1 LABELS, so every leg executes byte-identical code and the ratio
    # this script reports is the box's NOISE FLOOR for this fixture, this leg
    # count and this estimator.
    #
    # It exists because the 2026-08-29 control rows measured that floor by
    # accident and it was larger than the effect: duke3d-586-short read 0.9705
    # over two rounds with the arms provably identical. A ladder result must be
    # compared against a floor measured the SAME WAY, not against a remembered
    # +/-2%.
    #
    # The non-vacuity gate INVERTS under this switch: identical decode_probes
    # becomes REQUIRED, and probes that moved mean the switch failed to pin the
    # arm.
    [switch]$NullControl,
    # Per-round validity gate, in PERCENT. A round whose WITHIN-ARM spread exceeds this is
    # excluded from the gated result and should be re-run. Pick it BEFORE the run, from the
    # size of the effect expected: a round where one arm disagrees with itself by more than
    # the effect has not measured the effect.
    #
    # The default is 1.0 because that is roughly what a quiet round on this box delivers --
    # the 2026-08-29 MUL ladder's round 1 read 0.45% and 0.54%, while its round 3 read 16.12%.
    # Gating on this CANNOT bias the answer: the within-arm spread is zero-effect by
    # construction and blind to the arm comparison.
    [double]$NullThreshold = 1.0,
    # WHICH KNOB the arms select. Defaults to the slice this script was written for; naming
    # another makes it a general two-arm ladder rather than a copy-paste per slice.
    #
    # The knob must spell its arms "0" and "1". Its DEFAULT is irrelevant here and that is a
    # property of this script, not an accident: the variable is set EXPLICITLY on every leg
    # (Set-BaseEnvironment pins both slice knobs to "0", then the per-leg set overrides the one
    # under test), so arm 0 is the pre-slice base whatever ships. IZARRAVM_LOOP_ROWS defaulted
    # OFF when measured here and flipped ON on 2026-08-30; the recorded arms did not change
    # meaning.
    [ValidateSet("IZARRAVM_MUL_MEM_ROWS", "IZARRAVM_LOOP_ROWS", "IZARRAVM_RETRY_LIFT",
        "IZARRAVM_OUT_IMM8_ROWS")]
    [string]$Knob = "IZARRAVM_MUL_MEM_ROWS",
    # Resolve -Rows, print the selection, exit 0. Exists so the self-test's
    # green control can prove a well-formed invocation binds without running a
    # leg. Run-set arguments still have to be supplied; dummies are fine.
    [switch]$BindCheck,
    [Parameter(Mandatory = $true, ParameterSetName = "SelfTest")][switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".." ".."))

# The benchmark workspace. See scripts/run-fixture-scoreboard.ps1's
# Resolve-BenchRoot for the full rule: IZARRAVM_BENCH_ROOT overrides
# <repo>/.bench, unset and empty both mean the default, and a set-but-missing
# directory is a hard error rather than a silent fallback to the wrong fixtures.
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
# creates a stray directory. Arguments, cycles and the gp2 injection are copied
# from scripts/run-fixture-scoreboard.ps1's fixture table. nascar-586 has no
# --video there and must not gain one here, because its recorded invariants were
# measured without it.
$fixtures = @{
    # THE MAGNITUDE ROW. The mouse schedule is not optional decoration: GP2's
    # menus are mouse only, four Enter presses leave the framebuffer
    # bit-identical to no input at all, and without the three clicks the run
    # never reaches the race this row exists to measure. GP2 sets its own INT
    # 33h ratio at 1 pixel per mickey on BOTH axes, which is NOT the TOKAMOUS
    # default; a schedule built on the driver default overshoots vertically by
    # 2x and clicks nothing.
    "gp2-586"          = @{
        folder    = "gp2_c"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        cycles    = "13280000000"
        injection = @("--inject-mouse", ("3320000000:home;3652000000:move:320,386;" +
                "3984000000:click;4648000000:move:0,-115;5146000000:click;" +
                "5976000000:move:-273,181;6474000000:click"))
    }
    "nascar-586"       = @{
        folder    = "nascar1_c"
        arguments = @("--cpu", "586", "--memory-mib", "64")
        cycles    = "4980000000"
        injection = @()
    }
    "duke3d-586-short" = @{
        folder    = "duke3d_short_c"
        arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
        cycles    = "33200000000"
        injection = @()
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
    return , $selected
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

    $split = Resolve-RowSelection @("gp2-586,nascar-586") $known
    Assert-BinderSelfTestEqual $split.Count 2 "a comma-joined -Rows string splitting"
    Assert-BinderSelfTestEqual $split[0] "gp2-586" "the first row of a comma string"
    Assert-BinderSelfTestEqual $split[1] "nascar-586" "the second row of a comma string"
    $padded = Resolve-RowSelection @(" gp2-586 , nascar-586") $known
    Assert-BinderSelfTestEqual $padded.Count 2 "whitespace around comma-joined rows"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586,no-such-row") $known } `
        "Unknown row 'no-such-row'" "an unknown name after the comma split"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586,") $known } `
        "empty entry" "a stray trailing comma"
    Assert-BinderSelfTestThrows { Resolve-RowSelection @("gp2-586", "gp2-586") $known } `
        "more than once" "a row named twice"

    $pwshExecutable = (Get-Process -Id $PID).Path
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-mulmem-" +
        [Guid]::NewGuid().ToString("N").Substring(0, 10))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    try {
        $outputPath = Join-Path $scratch "stdout.txt"
        $failurePath = Join-Path $scratch "stderr.txt"
        $start = @{
            FilePath               = $pwshExecutable
            ArgumentList           = @("-NoProfile", "-File", $PSCommandPath,
                "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
                "-Rows", "gp2-586", "nascar-586", "-BindCheck")
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
        if ($failureText -notmatch 'nascar-586') {
            throw ("self-test failed: the mangled -Rows child failed, but not on the " +
                "stray token. stderr: $failureText")
        }

        # GREEN control: the comma spelling of the same selection must bind and
        # resolve, or the red row above proves nothing about the guard.
        $start.ArgumentList = @("-NoProfile", "-File", $PSCommandPath,
            "-Executable", "self-test-dummy", "-OutDir", "self-test-dummy",
            "-Rows", "gp2-586,nascar-586", "-BindCheck")
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
        if ($listing -notmatch 'nascar-586') {
            throw "self-test failed: the -BindCheck control did not echo the selection"
        }
    }
    finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "ladder-mul-mem-rows self-test passed"
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

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    throw ("OutDir must be pinned explicitly. A results script that defaults its " +
        "OutDir has already overwritten another campaign's profile once.")
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# Every knob the board sets explicitly, so a stray parent-shell value cannot
# turn an observation into a different arm. The knob named by -Knob is set per
# leg below and is the ONLY variable that differs between arms.
function Set-BaseEnvironment {
    $env:IZARRAVM_JIT = "1"
    $env:IZARRAVM_JIT16 = "1"
    $env:IZARRAVM_JIT16_486 = "1"
    $env:IZARRAVM_ONE_LOOKUP_STORE = "1"
    $env:IZARRAVM_ONE_LOOKUP_LOAD = "1"
    $env:IZARRAVM_DIRECT_BARRIER_CENSUS = "0"
    # Both slice knobs pinned OFF here; the one under test is set per leg AFTER this.
    # Without this a stray parent-shell value for the OTHER slice would ride along in
    # both arms and quietly change what the base is.
    # Pins follow the SHIPPED defaults so arm 0 is the base a user runs. LOOP flipped ON in
    # PR #771 (main a836b309), so its pin moved 0 -> 1 with it on 2026-08-30. The knob under
    # test still gets an explicit 0/1 per leg AFTER this, overriding its pin.
    $env:IZARRAVM_MUL_MEM_ROWS = "0"
    $env:IZARRAVM_LOOP_ROWS = "1"
    $env:IZARRAVM_RETRY_LIFT = "0"
    $env:IZARRAVM_OUT_IMM8_ROWS = "0"
    foreach ($observer in @(
            "IZARRAVM_CPU_PROFILE", "IZARRAVM_CPU_PROFILE_ADDRS",
            "IZARRAVM_MACHINE_PROFILE", "IZARRAVM_RIP_PROFILE",
            "IZARRAVM_PHASE_INTERVAL_MS", "IZARRAVM_AUDIO_WAV", "IZARRAVM_AUDIO_WAV_WALL",
            "IZARRAVM_AUDIO_COST", "IZARRAVM_AUDIO_COST_SLICE_MS",
            "IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION", "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION")) {
        # REMOVAL, not the empty string: an empty value leaves the variable SET,
        # and several readers arm on var_os()/is_some(). The knob under test is
        # one of them in the other direction -- for it, empty means OFF, which
        # AGREES with its default -- which is why it is always set to an explicit
        # "0" or "1" below and never cleared.
        if (Test-Path "Env:$observer") { Remove-Item "Env:$observer" }
    }
}

# The confounder this campaign actually suffers from, MEASURED per leg rather
# than assumed absent. The 2026-08-24 board was taken with an agent running and
# read four rows 8-16% slow; the emulator is single-threaded but the workload is
# L3- and memory-bandwidth sensitive, so a concurrent `cargo -j8` moves it even
# on a 32-core host. A leg whose window contained a builder is marked and can be
# re-run rather than silently averaged in. Copied from
# ladder-extended-ram-screen.ps1, which is where it earned its place: it let a
# peer session re-run one round instead of distrusting sixteen.
function Get-BuilderCount {
    $builders = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -match '^(cargo|rustc|link|lld-link)$' }
    if ($null -eq $builders) { return 0 }
    return @($builders).Count
}

# KNOWN GAP, measured 2026-08-29 rather than supposed. This detector watches FOUR PROCESS
# NAMES, and two wall excursions that evening went unattributed by it: nascar-586 round 2
# (both OFF legs ~64.5 s against 54-58 s everywhere else) and the gp2 A/A null control's
# round 2 (three legs at 114 / 109 / 104 s against ~85 s). Neither had a builder running,
# and a peer session confirmed by direct process scan that it owned nothing on the box.
#
# A NAME LIST CANNOT SEE antivirus, a search indexer, a Windows Update worker, a thermal or
# turbo excursion, or any child process with a different image name. So it answers the SAME
# WAY whether such a thing is present or absent, which is the definition of a non-instrument
# ([[asymmetric-probes-are-not-evidence]]).
#
# ADDED 2026-08-29 in answer to that gap: `Get-SystemCpuSeconds` below, and the per-leg
# `cpu_s` / `foreign_s` columns. The builder count is kept because it names WHAT was running
# when it fires; the CPU columns say HOW MUCH ran when nothing is named.

# Total CPU seconds consumed by every process this session can see. Sampled either side of a
# leg, the DELTA is the whole box's CPU consumption across that leg; subtract the emulator's
# own `cpu_s` and what is left is FOREIGN CPU -- an indexer, an antivirus pass, an update
# worker, a differently-named child, anything.
#
# Protected processes (System, Idle, and anything this session cannot open) throw on the
# property read and are skipped. They are skipped in BOTH samples, so the delta stays
# meaningful for everything visible; it is a lower bound on foreign work, never an upper one.
function Get-SystemCpuSeconds {
    $total = 0.0
    foreach ($process in (Get-Process -ErrorAction SilentlyContinue)) {
        try { $total += $process.TotalProcessorTime.TotalSeconds } catch { }
    }
    return $total
}

function Invoke-Leg([string]$Row, [string]$Arm, [int]$Round) {
    $fixture = $fixtures[$Row]
    $stamp = "$Row-arm$Arm-r$Round"
    $copy = Join-Path $OutDir "work-$stamp"
    if (Test-Path $copy) { Remove-Item -Recurse -Force $copy }
    # Copy fresh per run: several fixtures mutate their own tree, and a leg that
    # started from a previous leg's mutated tree is a different workload.
    Copy-Item -Recurse (Join-Path $benchRoot $fixture.folder) $copy
    $profilePath = Join-Path $OutDir "$stamp.json"

    Set-BaseEnvironment
    # Under -NullControl both arms get "0" while keeping their labels, so the
    # reported ratio is the noise floor rather than an effect.
    Set-Item -Path "Env:$Knob" -Value $(if ($NullControl) { "0" } else { $Arm })

    $arguments = @()
    $arguments += $fixture.arguments
    $arguments += @("--hdd-folder", $copy)
    $arguments += @("--cycles", $fixture.cycles)
    $arguments += @("--profile-json", $profilePath)
    $arguments += $fixture.injection

    # LAUNCHED THROUGH ProcessStartInfo RATHER THAN `& $Executable`, and the reason is the
    # measurement rather than style: only a retained `Process` object exposes
    # `TotalProcessorTime` after exit, and that is the estimator this run exists to test.
    # `ArgumentList` (not a joined string) so the gp2 mouse schedule's semicolons and colons
    # need no quoting rules.
    #
    # stdout goes to a FILE, never to an undrained pipe. A redirected pipe nobody reads
    # deadlocks the child as soon as it fills, and the emulator does write.
    $stdoutPath = Join-Path $OutDir "$stamp.stdout.txt"
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    foreach ($argument in $arguments) { $startInfo.ArgumentList.Add([string]$argument) }
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true

    $buildersBefore = Get-BuilderCount
    $systemBefore = Get-SystemCpuSeconds
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($startInfo)
    # Drain on a background task so a chatty child cannot fill the pipe and block.
    $drain = $process.StandardOutput.ReadToEndAsync()
    $process.WaitForExit()
    $watch.Stop()
    $cpuSeconds = $process.TotalProcessorTime.TotalSeconds
    $exitCode = $process.ExitCode
    $systemAfter = Get-SystemCpuSeconds
    $buildersAfter = Get-BuilderCount
    Set-Content -LiteralPath $stdoutPath -Value $drain.GetAwaiter().GetResult()
    $process.Dispose()
    if ($exitCode -ne 0) { throw "$stamp exited $exitCode" }
    # A lower bound on foreign CPU: everything the box burned across the leg, less the
    # emulator's own. Clamped at 0 because the two system samples are taken microseconds
    # outside the stopwatch and can round the wrong way on a quiet box.
    $foreignSeconds = [math]::Max(0.0, ($systemAfter - $systemBefore) - $cpuSeconds)

    # duke3d writes its own scorecard into the working copy and then ends the VM
    # with EXITVM. Keep it before the copy goes: its sample count and Info String
    # are invariants of the run, so a leg whose scorecard differs is a leg that
    # did not run the same demo, whatever the wall says.
    $scorecard = Join-Path $copy "DUKEMARK.TXT"
    if (Test-Path -LiteralPath $scorecard) {
        Copy-Item -LiteralPath $scorecard (Join-Path $OutDir "$stamp.dukemark.txt")
    }
    Remove-Item -Recurse -Force $copy
    $report = Get-Content $profilePath -Raw | ConvertFrom-Json
    [pscustomobject]@{
        row      = $Row
        arm      = $Arm
        round    = $Round
        dirty    = ($buildersBefore -gt 0 -or $buildersAfter -gt 0)
        wall_s   = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        # The candidate estimator. The emulator is single-threaded, so this is wall MINUS
        # time the scheduler gave to something else -- which is exactly what an indexer or an
        # antivirus pass takes and what no name-list detector can see.
        cpu_s    = [math]::Round($cpuSeconds, 3)
        # Foreign CPU seconds across the leg, a lower bound. Non-zero means something else ran.
        foreign_s = [math]::Round($foreignSeconds, 3)
        guest_s  = $report.guest_seconds
        insns    = $report.perf.instructions
        bus      = $report.raw_bus_clocks
        ticks    = $report.master_ticks
        probes   = $report.perf.decode_probes
        rt       = $report.real_time_factor
    }
}

$legs = [Collections.Generic.List[object]]::new()
foreach ($row in $Rows) {
    for ($round = 1; $round -le $Rounds; $round++) {
        # A/B/B/A: the inner pair is order-reversed so a monotone drift across
        # the round cancels rather than accumulating onto one arm.
        $order = if ($round % 2 -eq 1) { @("0", "1", "1", "0") } else { @("1", "0", "0", "1") }
        foreach ($arm in $order) {
            $leg = Invoke-Leg $row $arm $round
            $legs.Add($leg)
            $flag = if ($leg.dirty) { "  <-- BUILDER ACTIVE, leg suspect" } else { "" }
            "{0,-16} arm {1}  r{2}  wall {3,8:N3}s  cpu {4,8:N3}s  foreign {5,7:N1}s  rt {6,6:N3}{7}" -f `
                $leg.row, $leg.arm, $leg.round, $leg.wall_s, $leg.cpu_s, $leg.foreign_s, $leg.rt, $flag | Write-Host
        }
    }
}

$legs | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "legs.json")

Write-Host ""
Write-Host "=== IDENTITY GATE (this slice predicts identity: see the header) ==="
$identityFailed = $false
foreach ($row in $Rows) {
    foreach ($field in @("guest_s", "insns", "bus", "ticks")) {
        # @(...) forces an array: Sort-Object -Unique returns a SCALAR when the
        # values are all equal, which is the passing case, and a bare .Count on
        # a scalar throws. The gate must not crash on success.
        $values = @($legs | Where-Object row -eq $row | ForEach-Object { $_.$field } | Sort-Object -Unique)
        $verdict = if ($values.Count -eq 1) { "OK" } else { "DIVERGED" }
        if ($values.Count -ne 1) { $identityFailed = $true }
        "{0,-16} {1,-8} {2,-9} {3}" -f $row, $field, $verdict, ($values -join " / ") | Write-Host
    }
}

Write-Host ""
if ($NullControl) {
    Write-Host "=== NON-VACUITY, INVERTED (-NullControl: probes MUST be identical) ==="
}
else {
    Write-Host "=== NON-VACUITY (decode_probes moving proves the arm reached block formation) ==="
}
$gateFailed = $false
foreach ($row in $Rows) {
    $offProbes = @($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" } | ForEach-Object probes | Sort-Object -Unique)
    $onProbes = @($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" } | ForEach-Object probes | Sort-Object -Unique)
    $moved = -not (($offProbes -join ",") -eq ($onProbes -join ","))
    # TWO HYPOTHESES, and the 2026-08-29 control rows are why this no longer
    # asserts the first one. Identical probes means EITHER the knob never
    # reached the child, OR the fixture has no population for the row to admit.
    # On the row under test the first is a harness bug; on a CONTROL row the
    # second is the expected and desired answer. The script cannot tell them
    # apart from wall data, so it REPORTS both and refuses to pick -- a barrier
    # census on the row settles it, because the census either shows the row or
    # does not.
    if ($NullControl) {
        if ($moved) { $gateFailed = $true }
        $verdict = if ($moved) { "MOVED -- -NullControl failed to pin the arm" } else { "identical, as required" }
    }
    else {
        $verdict = if ($moved) {
            "moved -- the arm reached block formation"
        }
        else {
            "IDENTICAL -- either the knob never took, OR this fixture has no population. NOT DECIDABLE HERE: take a census."
        }
    }
    "{0,-16} probes OFF {1}  ON {2}  {3}" -f $row, ($offProbes -join "/"), ($onProbes -join "/"),
    $verdict | Write-Host
}

Write-Host ""
Write-Host "=== RESULT, BOTH ESTIMATORS (OFF = main's refusal, ON = MulMemAcc) ==="
Write-Host "    ratio > 1 means ON is faster. Under -NullControl BOTH numbers are FLOORS,"
Write-Host "    not effects, and the smaller one names the better estimator."
foreach ($row in $Rows) {
    foreach ($metric in @("wall_s", "cpu_s")) {
        $off = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" } | Measure-Object $metric -Minimum).Minimum
        $on = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" } | Measure-Object $metric -Minimum).Minimum
        $pairsAbove = 0
        for ($round = 1; $round -le $Rounds; $round++) {
            $o = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" -and $_.round -eq $round } | Measure-Object $metric -Minimum).Minimum
            $n = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" -and $_.round -eq $round } | Measure-Object $metric -Minimum).Minimum
            if ($o -gt $n) { $pairsAbove++ }
        }
        $ratio = if ($on -gt 0) { $off / $on } else { [double]::NaN }
        "{0,-16} min-{1,-6} OFF {2,8:N3}s  ON {3,8:N3}s  ratio {4,6:N4}  pairs above 1: {5}/{6}" -f `
            $row, $metric.Replace("_s", ""), $off, $on, $ratio, $pairsAbove, $Rounds | Write-Host
    }
    foreach ($metric in @("wall_s", "cpu_s")) {
        $values = @($legs | Where-Object row -eq $row | ForEach-Object { $_.$metric })
        $low = ($values | Measure-Object -Minimum).Minimum
        $high = ($values | Measure-Object -Maximum).Maximum
        "{0,-16} {1,-6} spread over all legs: {2,8:N3} .. {3,8:N3}  = {4,6:N2}%" -f `
            $row, $metric.Replace("_s", ""), $low, $high, (100 * ($high - $low) / $low) | Write-Host
    }
    $foreign = @($legs | Where-Object row -eq $row | ForEach-Object foreign_s | Measure-Object -Maximum).Maximum
    "{0,-16} worst foreign CPU on any leg: {1,7:N1}s" -f $row, $foreign | Write-Host
}

# THE IN-SESSION NULL, and it costs nothing because every A/B/B/A round already contains it.
#
# WHY THIS EXISTS. Two A/A null controls on gp2-586 -- same fixture, same estimator, same
# binary, two hours apart on 2026-08-29 -- returned min-wall ratios of 1.0290 and 0.9993. A
# SINGLE A/A IS ONE SAMPLE OF THE NULL DISTRIBUTION, NOT A FLOOR, and running one before or
# after the effect measures a different box from the one the effect was measured on.
#
# Each round runs each arm TWICE. The spread between one arm's two legs is a NULL PAIR taken
# under exactly the conditions of that round: same minute, same box state, same everything,
# and by construction zero effect. That is the number the between-arm ratio has to beat.
#
# READ IT THIS WAY: if the between-arm effect is not comfortably larger than the worst
# within-arm null in the same run, the ladder has not resolved anything, whatever the
# headline ratio says. On the 2026-08-29 MUL slice the within-arm nulls ran 0.45% to 16.1%
# with a median near 2%, against a 2.3% effect -- which is the park verdict, reached without
# spending one extra leg.
Write-Host ""
Write-Host "=== IN-SESSION NULL: within-arm spread per round (zero effect by construction) ==="
foreach ($row in $Rows) {
    $worst = 0.0
    for ($round = 1; $round -le $Rounds; $round++) {
        $line = "  round {0}:" -f $round
        foreach ($arm in @("0", "1")) {
            $values = @($legs | Where-Object {
                    $_.row -eq $row -and $_.arm -eq $arm -and $_.round -eq $round
                } | ForEach-Object wall_s)
            if ($values.Count -lt 2) { continue }
            $low = ($values | Measure-Object -Minimum).Minimum
            $high = ($values | Measure-Object -Maximum).Maximum
            $spread = 100 * ($high - $low) / $low
            if ($spread -gt $worst) { $worst = $spread }
            $line += "  arm {0} null {1,6:N2}%" -f $arm, $spread
        }
        Write-Host $line
    }
    "{0,-16} WORST within-arm null: {1,6:N2}%   <-- the between-arm effect must beat this" -f `
        $row, $worst | Write-Host
}

# PER-ROUND VALIDITY, and this is the part that turns the null into a usable protocol rather
# than a warning label.
#
# The within-arm null is ZERO EFFECT BY CONSTRUCTION, so gating a round on it CANNOT leak the
# effect into the selection. That is what separates this from choosing the sample after seeing
# it, which this campaign's memory rightly forbids: the criterion is blind to the arm
# comparison. A round where one arm disagrees with ITSELF by more than the effect being
# measured has not measured the effect, whatever its between-arm ratio says.
#
# Measured on the 2026-08-29 MUL ladder, the four rounds' worst within-arm nulls were 0.54%,
# 4.38%, 16.12% and 2.26%. The box is not uniformly noisy -- round 1 was quiet enough to
# resolve well under 1% -- so the failure was mixing quiet and contaminated rounds into one
# min-wall figure, not the rig being hopeless.
#
# HOW TO USE IT: pick -NullThreshold BEFORE the run, from the size of the effect you expect;
# a round whose null exceeds it is excluded and RE-RUN, not averaged in. Report how many
# rounds survived. Fewer than three surviving rounds is not a result.
Write-Host ""
"=== PER-ROUND EFFECT, gated on the null (-NullThreshold {0:N2}%) ===" -f $NullThreshold | Write-Host
foreach ($row in $Rows) {
    $valid = @()
    for ($round = 1; $round -le $Rounds; $round++) {
        $nulls = @()
        foreach ($arm in @("0", "1")) {
            $values = @($legs | Where-Object {
                    $_.row -eq $row -and $_.arm -eq $arm -and $_.round -eq $round
                } | ForEach-Object wall_s)
            if ($values.Count -lt 2) { continue }
            $low = ($values | Measure-Object -Minimum).Minimum
            $nulls += 100 * (($values | Measure-Object -Maximum).Maximum - $low) / $low
        }
        $worstNull = if ($nulls.Count -gt 0) { ($nulls | Measure-Object -Maximum).Maximum } else { 0 }
        $o = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "0" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
        $n = ($legs | Where-Object { $_.row -eq $row -and $_.arm -eq "1" -and $_.round -eq $round } | Measure-Object wall_s -Minimum).Minimum
        $ratio = if ($n -gt 0) { $o / $n } else { [double]::NaN }
        $ok = $worstNull -le $NullThreshold
        if ($ok) { $valid += $ratio }
        "  round {0}  ratio {1,6:N4}   worst null {2,6:N2}%   {3}" -f `
            $round, $ratio, $worstNull, $(if ($ok) { "VALID" } else { "EXCLUDED -- re-run this round" }) | Write-Host
    }
    if ($valid.Count -eq 0) {
        "{0,-16} NO VALID ROUNDS. This ladder measured nothing; re-run on a quieter box." -f $row | Write-Host
    }
    else {
        $mean = ($valid | Measure-Object -Average).Average
        $above = @($valid | Where-Object { $_ -gt 1 }).Count
        "{0,-16} {1} of {2} rounds valid   mean ratio {3,6:N4}   above 1: {4}/{5}{6}" -f `
            $row, $valid.Count, $Rounds, $mean, $above, $valid.Count,
        $(if ($valid.Count -lt 3) { "   <-- FEWER THAN THREE VALID ROUNDS IS NOT A RESULT" } else { "" }) | Write-Host
    }
}

$dirtyLegs = @($legs | Where-Object dirty)
if ($dirtyLegs.Count -gt 0) {
    Write-Host ""
    "CONTAMINATION: {0} of {1} legs ran with a builder active. Those legs are SUSPECT." -f `
        $dirtyLegs.Count, $legs.Count | Write-Host
    $dirtyLegs | ForEach-Object { "  {0} arm {1} r{2}" -f $_.row, $_.arm, $_.round } | Write-Host
    Write-Host "Re-run the affected rounds on a quiet machine before quoting any wall number."
}
else {
    Write-Host ""
    Write-Host "No builder was active during any leg."
}

if ($gateFailed) {
    Write-Host ""
    Write-Host "-NullControl FAILED: decode_probes moved, so the switch did not pin both arms"
    Write-Host "to IZARRAVM_MUL_MEM_ROWS=0. The ratio above is not a noise floor."
    exit 1
}

if ($identityFailed) {
    Write-Host ""
    Write-Host "IDENTITY DIVERGED. Two hypotheses, and this script cannot pick between them:"
    Write-Host "(a) a charging defect -- the emitted form charges differently from the"
    Write-Host "interpreter; or (b) the PROTOCOL.md:199 class -- block formation moved"
    Write-Host "fetch-run charging, so a fixed cycle budget cuts off ppm later. (b) is"
    Write-Host "DETERMINISTIC: every leg of one arm shows the SAME totals, the magnitude is"
    Write-Host "1e-7-class, and it has an accepted precedent (the +4,465 case, and"
    Write-Host "IZARRAVM_LOOP_ROWS' +1,466). (a) is neither. Check determinism and magnitude"
    Write-Host "in legs.json, explain the divergence in the slice's design doc before quoting"
    Write-Host "any wall number, and expect cutoff-phase frame pins (gp2's hash, nascar's"
    Write-Host "contract) to move legitimately -- judge those by band signature, not hex."
    exit 1
}
