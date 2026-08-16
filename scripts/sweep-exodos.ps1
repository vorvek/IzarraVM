# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4
#
# eXoDOS corpus sweep: extract, translate, run headless, archive, delete.
#
# WHY THIS EXISTS. Every perf campaign so far has ranked levers against eight
# fixtures, which cannot say what fraction of the DOS library a lever covers.
# This runs corpus titles one at a time and archives everything each run
# produced, so the classification can be recomputed later without running
# anything again. Design: dev_docs/exodos-sweep-design.md.
#
# WHAT IT IS NOT. No wall figure from this sweep may enter an A/B or a
# scoreboard claim. It runs under variable load, with phase marks armed, with
# the run sliced by key injection and screen sampling, against fixtures that
# have no pinned invariants. .bench/PROTOCOL.md owns every performance claim.
#
# Rules paid for in earlier campaigns and encoded here:
#   - The corpus is READ-ONLY. Nothing under -Corpus is ever opened for write.
#   - Extract-run-delete. Mounting a folder writes into it, and Katea
#     reconciles guest writes to the host during the run, so a run always gets
#     a fresh scratch copy and never reuses a killed run's tree.
#   - Observer variables are REMOVED with $null, never blanked. A set-but-empty
#     variable is not "off": readers using var_os().is_some() arm on "".
#   - No $args assignment inside a function. One battery leg once launched the
#     emulator with an empty argument list, fell back to the owner's real
#     c_drive, and died without a trace.
#   - Liveness, not log greps. A silent death is invisible to a grep-only
#     monitor, so the watchdog watches the process and the output file's
#     modification time.

param(
    # Corpus root. Read-only.
    [string]$Corpus = "E:\eXo\eXo\eXoDOS",
    # Corpus shorts to run, or a file with one per line.
    [string[]]$Games,
    [string]$GameListFile,
    [Parameter(Mandatory)][string]$OutDir,
    [string]$Executable = "D:\dev\IzarraVM\target\release\izarravm.exe",
    [string]$Translator = "D:\dev\IzarraVM\target\release\izarravm-exodos.exe",
    # Scratch lives on D:, which has the free space; C: does not.
    [string]$ScratchRoot = "D:\exo-scratch",
    # Per-game key schedules, `<short>.json`. Missing files fall back to the
    # translator's generic sequence.
    [string]$RecipeDir,
    [string]$Persona = "586",
    [int]$GuestSeconds = 120,
    # Guest ms between phase marks. The classification window is the last 60
    # guest seconds and 2000 ms yields about 30 marks in it.
    [int]$PhaseIntervalMs = 2000,
    # Guest ms between screen samples.
    [int]$ScreenIntervalMs = 5000,
    # Host seconds before a run is killed as HUNG-HOST. This is the BACKSTOP,
    # not the primary kill: MEASURED 2026-08-16, kq1vga runs at rt 0.138 and
    # reached only 112 of its 120 guest seconds before the old 900 s ceiling cut
    # it off, so the wall clock was deciding rows that were working. 1500 s
    # covers 120 guest seconds down to about rt 0.08. The stall detector below
    # is what ends a wedged run, and it ends one in a quarter of the time.
    [int]$WatchdogSeconds = 1500,
    # Host seconds with no growth in the screen index (or stdout/stderr) before
    # a run is killed as STALLED. THE PRIMARY KILL. A 586 run at rt 0.29 samples
    # the screen about every 17 host seconds, so this is many missed samples,
    # not a close call. 0 disables the detector and leaves only the wall-clock
    # watchdog, which is a much blunter instrument.
    [int]$StallSeconds = 300,
    # Refuse a game whose extraction would exceed this, rather than filling the
    # volume. The corpus maximum extracts to about 7 GB.
    [int]$MaxExtractGib = 8,
    [switch]$KeepScratch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Guest clock per persona, for turning guest seconds into a --cycles budget.
$personaClockHz = @{ "586" = 166000000; "486" = 66000000; "386" = 22000000 }

# Stderr lines the emulator emits on a healthy run. Anything NOT matched here
# fails the row loudly, which is the point: an unknown stderr line is a finding.
#
# Katea does NOT get a blanket prefix. Almost everything it writes is a failure
# or a data-loss hold -- a failed materialize, a held rename, a sector that
# could not be read back -- and swallowing those as routine is exactly how a
# corpus row would report RAN while the guest's writes went nowhere. Only the
# one genuinely routine line is allowed, and it is allowed by its whole text.
# The timestamped form carries its level too: an ISO-stamped ERROR is an error.
$benignStderrPatterns = @(
    '^katea: skipping .+ \(>= 4 GiB, not FAT32-representable\)\s*$',
    '^\[DMA\] ',
    '^\s*$',
    '^\s*(INFO|WARN|DEBUG|TRACE)\b',
    '^\d{4}-\d{2}-\d{2}T\S*\s+(INFO|WARN|DEBUG|TRACE)\b',
    # Open-bus port diagnostics are DATA, not failures: a corpus title probing
    # for sound cards or hypervisors touches unclaimed ports as a matter of
    # course, and the emulator's answer (float, log, count) is the hardware
    # answer. The lines stay archived in the row's stderr file and the port
    # set is in the profile; failing rows on them would kill half a corpus of
    # perfectly healthy detection sweeps (first seen: Cataco3D strobing the
    # AT coprocessor latch at 0xF0, 2026-08-16).
    '^open-bus: ',
    '^port-fatal: '
)

# A corpus short names a directory under `!dos` and a directory under the
# scratch root, and the scratch one is DELETED with -Recurse -Force. A list line
# is caller input; `..\..\Windows` would derive a path outside the scratch root
# and delete it. Validate the shape BEFORE any path is built from it: the corpus
# shorts are plain alphanumeric names.
function Test-CorpusShort {
    param([string]$Short)
    if ([string]::IsNullOrWhiteSpace($Short)) { return $false }
    if ($Short -eq '.' -or $Short -eq '..') { return $false }
    return $Short -match '^[A-Za-z0-9][A-Za-z0-9_.-]*$'
}

# Reboot detection. The design proposed scraping stdout for a repeated POST
# banner; MEASURED 2026-08-16, the --hdd-folder path prints NO banner at all,
# so that detector has no signal and this does not pretend otherwise. The
# screen index is the substitute: a machine that resets redraws its first
# screen, so a run whose opening frame hash recurs after having changed away
# from it has been round the loop. Reported as a count, and only two or more
# recurrences call the row a REBOOT-LOOP.
$rebootRecurrenceThreshold = 2

foreach ($required in @($Executable, $Translator)) {
    if (-not (Test-Path -LiteralPath $required)) { throw "Missing executable: $required" }
}
if (-not (Test-Path -LiteralPath $Corpus)) { throw "Missing corpus: $Corpus" }
$dosRoot = Join-Path $Corpus '!dos'
if (-not (Test-Path -LiteralPath $dosRoot)) { throw "Missing corpus !dos directory: $dosRoot" }

if ($GameListFile) {
    if (-not (Test-Path -LiteralPath $GameListFile)) { throw "Missing game list: $GameListFile" }
    $Games = @(Get-Content -LiteralPath $GameListFile |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ -and -not $_.StartsWith('#') })
}
if (-not $Games -or $Games.Count -eq 0) { throw "No games given. Use -Games or -GameListFile." }
if (-not $personaClockHz.ContainsKey($Persona)) { throw "Unknown persona: $Persona" }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
New-Item -ItemType Directory -Force -Path $ScratchRoot | Out-Null
$cycleBudget = [int64]$personaClockHz[$Persona] * [int64]$GuestSeconds

# Set every observer explicitly to OFF by REMOVING it. The barrier census is
# the one that matters most: it only does work when the JIT is active, so it
# taxes exactly the runs this is trying to time, and a first attempt at using
# it in a timing pass once read +72%.
function Clear-ObserverEnvironment {
    $observers = @(
        'IZARRAVM_DIRECT_BARRIER_CENSUS', 'IZARRAVM_CPU_PROFILE', 'IZARRAVM_MACHINE_PROFILE',
        'IZARRAVM_RIP_PROFILE', 'IZARRAVM_AUDIO_WAV', 'IZARRAVM_AUDIO_COST',
        'IZARRAVM_SMC_CENSUS', 'IZARRAVM_VGA_WIPE_CENSUS', 'IZARRAVM_INT13_PROFILE',
        'IZARRAVM_IPE_WINDOW_TRACE', 'IZARRAVM_WATCH_WRITE', 'IZARRAVM_DIFF_TRACE',
        'IZARRAVM_SMC_TRACE', 'IZARRAVM_DUMP_LINEAR', 'IZARRAVM_DOSROOT'
    )
    foreach ($name in $observers) {
        Remove-Item -Path "Env:$name" -ErrorAction SilentlyContinue
    }
}

# Non-title .bat names inside `!dos\<short>\`. MEASURED over the whole corpus
# 2026-08-16: 7666 directories ship `install.bat` and 108 also ship
# `exception.bat`; every other .bat name in the corpus occurs exactly once and
# is a title marker. `exception.bat` sorts before most titles under the NTFS
# upcased ordering (EXCEPTION < KING'S < PRINCE), so taking the first .bat
# derived `exception.zip` for Ppersia and kq1vga and the row died as
# "no zip resolved".
$nonTitleBatNames = @('install.bat', 'exception.bat')

# Find `<Title (Year)>.zip` for a corpus short. The mapping runs through the
# `<Full Title (Year)>.bat` file inside `!dos\<short>\`; the corpus is dense
# with near neighbours (DOOM / DOOMII / DOOM2D / DOOM4), so the short is
# matched EXACTLY and never fuzzily.
#
# The name list is not the only guard: every candidate is checked against the
# zip that must sit beside it, and the first candidate WITH a zip wins. A future
# non-title marker therefore costs nothing as long as no zip is named after it.
function Resolve-GameZip {
    param([string]$DosRoot, [string]$CorpusRoot, [string]$Short, [string[]]$Excluded)
    $confDir = Join-Path $DosRoot $Short
    if (-not (Test-Path -LiteralPath $confDir)) { return $null }
    $leaf = Split-Path -Leaf ((Get-Item -LiteralPath $confDir).FullName)
    if ($leaf -cne $Short) { return $null }
    $candidates = @(Get-ChildItem -LiteralPath $confDir -Filter '*.bat' -File |
        Where-Object { $Excluded -notcontains $_.Name.ToLowerInvariant() })
    foreach ($marker in $candidates) {
        $zip = Join-Path $CorpusRoot ($marker.BaseName + '.zip')
        if (Test-Path -LiteralPath $zip) { return $zip }
    }
    return $null
}

function Expand-GameZip {
    param([string]$Zip, [string]$Destination, [int]$MaxGib)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        $bytes = 0L
        foreach ($entry in $archive.Entries) { $bytes += $entry.Length }
        if ($bytes -gt ([int64]$MaxGib * 1GB)) {
            return [pscustomobject]@{ Ok = $false; Reason = "extract-too-large"; Bytes = $bytes }
        }
        [IO.Compression.ZipFileExtensions]::ExtractToDirectory($archive, $Destination)
        return [pscustomobject]@{ Ok = $true; Reason = ""; Bytes = $bytes }
    } finally {
        $archive.Dispose()
    }
}

# Run the emulator with a watchdog that watches the PROCESS and the files the
# run writes as it goes, so a silent death is visible rather than merely absent
# from a log.
#
# WHAT THE PROGRESS SIGNAL IS. MEASURED 2026-08-16: the --hdd-folder path prints
# nothing to stdout during a run, and --profile-json is written ONCE, at exit.
# Neither can carry liveness. `screens.jsonl` can: the dumper flushes an index
# line per sample, so its length grows on the sweep's own screen-sampling
# schedule. That is the signal, with stdout and the stderr file watched
# alongside for the runs that do write them.
#
# THE LIMITATION, STATED. With screen dumping off there is no periodic writer
# at all and the stall detector has nothing to read; it then degrades to the
# plain wall-clock watchdog, which is why the caller passes the screens index
# and why the sweep always arms screen dumps.
# Quote one argument the way CommandLineToArgvW reads it back: wrap when the
# text is empty or carries whitespace or a quote, double every backslash run
# that touches the closing quote, and escape embedded quotes.
function ConvertTo-WindowsArgument {
    param([string]$Value)
    if ($Value -ne '' -and $Value -notmatch '[\s"]') { return $Value }
    $out = '"'
    $backslashes = 0
    foreach ($ch in $Value.ToCharArray()) {
        if ($ch -eq '\') {
            $backslashes++
            continue
        }
        if ($ch -eq '"') {
            $out += '\' * (2 * $backslashes + 1) + '"'
        } else {
            $out += '\' * $backslashes + $ch
        }
        $backslashes = 0
    }
    return $out + ('\' * (2 * $backslashes)) + '"'
}

function Invoke-EmulatorRun {
    param(
        [string]$Exe,
        [string[]]$Arguments,
        [string]$StdoutPath,
        [string]$StderrPath,
        [int]$TimeoutSeconds,
        # Files whose growth means the run is still working.
        [string[]]$ProgressPaths = @(),
        # Host seconds of no growth in ANY progress path before the run is
        # called stalled. Zero disables the stall detector.
        [int]$StallSeconds = 0
    )
    if ($Arguments.Count -lt 8) {
        throw "refusing to launch with a short argument list ($($Arguments.Count))"
    }
    # An empty command line is the failure mode that once ran a whole leg
    # against the owner's real drive, hence the argument-count refusal above.
    #
    # -ArgumentList joins an array with spaces and quotes NOTHING, so a corpus
    # path with a space in it ("Death Rallye.cue", "UFO Enemy Unknown.iso")
    # arrives at the emulator as two arguments and clap rejects the row. Each
    # element is quoted here, to the CommandLineToArgvW rules the Rust runtime
    # parses back.
    $quoted = @($Arguments | ForEach-Object { ConvertTo-WindowsArgument $_ })
    $process = Start-Process -FilePath $Exe -ArgumentList $quoted -NoNewWindow -PassThru `
        -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath
    $watched = @($StdoutPath, $StderrPath) + $ProgressPaths
    $started = Get-Date
    $timedOut = $false
    $stalled = $false
    $lastTotal = -1L
    $lastProgress = Get-Date
    while (-not $process.HasExited) {
        Start-Sleep -Milliseconds 500
        if (((Get-Date) - $started).TotalSeconds -gt $TimeoutSeconds) {
            $timedOut = $true
            break
        }
        $total = 0L
        foreach ($path in $watched) {
            if ($path -and (Test-Path -LiteralPath $path)) {
                $total += (Get-Item -LiteralPath $path).Length
            }
        }
        if ($total -ne $lastTotal) {
            $lastTotal = $total
            $lastProgress = Get-Date
        } elseif ($StallSeconds -gt 0 -and ((Get-Date) - $lastProgress).TotalSeconds -gt $StallSeconds) {
            $stalled = $true
            break
        }
    }
    if ($timedOut -or $stalled) {
        try { $process.Kill($true) } catch { Write-Warning "kill failed: $_" }
        $process.WaitForExit(30000) | Out-Null
    }
    return [pscustomobject]@{
        ExitCode        = if ($process.HasExited) { $process.ExitCode } else { -1 }
        TimedOut        = $timedOut
        Stalled         = $stalled
        WallSeconds     = ((Get-Date) - $started).TotalSeconds
        QuietSeconds    = [math]::Round(((Get-Date) - $lastProgress).TotalSeconds, 1)
    }
}

function Test-StderrIsBenign {
    param([string]$Path, [string[]]$Patterns)
    if (-not (Test-Path -LiteralPath $Path)) { return @() }
    $offending = @()
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line.Trim().Length -eq 0) { continue }
        $matched = $false
        foreach ($pattern in $Patterns) {
            if ($line -match $pattern) { $matched = $true; break }
        }
        if (-not $matched) { $offending += $line }
    }
    # A bare `return @()` unrolls to $null, and a caller reading .Count on it
    # dies under Set-StrictMode. Comma-wrap keeps the array an array.
    return ,$offending
}

# Count returns to the opening frame. See the reboot note above.
function Measure-ScreenRecurrence {
    param($Screens)
    if ($Screens.Count -lt 3) { return 0 }
    $first = $Screens[0].hash
    $returns = 0
    $left = $false
    foreach ($sample in $Screens) {
        if ($sample.hash -ne $first) { $left = $true; continue }
        if ($left) { $returns++; $left = $false }
    }
    return $returns
}

# Read `stop.kind` from the PROFILE, never from the process exit code:
# --hdd-folder does not propagate the guest's fate and a cpu_error run still
# exits 0 unless --expect-test-exit was passed, which this sweep does not pass.
function Get-Outcome {
    param($Profile, $Screens, [int]$ScreenRecurrences, [bool]$TimedOut, [bool]$Stalled,
        [int]$MinMarks, [int]$RebootThreshold)
    if ($TimedOut) { return "HUNG-HOST" }
    # Killed for writing nothing at all, which the wall watchdog would only have
    # caught much later, and a crashed-and-wedged run not at all.
    if ($Stalled) { return "STALLED" }
    if ($null -eq $Profile) { return "NO-PROFILE" }
    $kind = $Profile.stop.kind
    if ($kind -eq "cpu_error") { return "CRASHED" }
    if ($ScreenRecurrences -ge $RebootThreshold) { return "REBOOT-LOOP" }
    $marks = 0
    if ($Profile.PSObject.Properties.Name -contains 'phase_marks' -and $Profile.phase_marks) {
        $marks = @($Profile.phase_marks).Count
    }
    if ($marks -lt $MinMarks) { return "SHORT-RUN" }
    if ($kind -eq "halted") { return "HALTED" }
    if ($kind -eq "test_exit" -or $kind -eq "dos_exit") { return "EXITED" }
    # The screen index answers the question no counter does: did the picture
    # ever change. A run whose last two thirds of samples share one hash is
    # parked, however busy its counters look.
    if ($Screens.Count -ge 6) {
        $tail = @($Screens[[int]($Screens.Count / 3)..($Screens.Count - 1)])
        $distinct = @($tail | Select-Object -ExpandProperty hash -Unique).Count
        if ($distinct -le 1) {
            $mode = $tail[-1].video_mode
            if ($null -eq $mode -or $mode -eq "text") { return "IDLE-TEXT" }
            return "IDLE-AT-MENU"
        }
    }
    return "RAN"
}

Clear-ObserverEnvironment
$env:IZARRAVM_PHASE_INTERVAL_MS = "$PhaseIntervalMs"

# Delete a scratch tree, retrying a bounded number of times. A game that just
# exited can still hold a handle open for a moment (Katea's reconcile writes,
# the antivirus filter, an Explorer preview), and a single SilentlyContinue
# leaves a multi-gigabyte tree behind that nothing ever cleans up. Returns the
# error text when the tree is still there afterwards, so the caller can say so
# instead of filling the volume quietly.
function Remove-ScratchTree {
    param([string]$Path, [int]$Attempts = 5)
    $last = $null
    for ($try = 1; $try -le $Attempts; $try++) {
        if (-not (Test-Path -LiteralPath $Path)) { return $null }
        try {
            Remove-Item -Recurse -Force -LiteralPath $Path -ErrorAction Stop
            return $null
        } catch {
            $last = "$_"
            Start-Sleep -Milliseconds (500 * $try)
        }
    }
    if (Test-Path -LiteralPath $Path) { return $last }
    return $null
}

function Get-FreeGib {
    param([string]$Path)
    $root = [IO.Path]::GetPathRoot((Resolve-Path -LiteralPath $Path).Path)
    return [math]::Round(([IO.DriveInfo]::new($root)).AvailableFreeSpace / 1GB, 2)
}

$rows = @()
$index = 0
foreach ($short in $Games) {
    $index++
    Write-Host "[$index/$($Games.Count)] $short"
    $scratch = $null

    # Every column is declared here. Set-StrictMode makes reading an absent
    # property a terminating error, and a row that took an early exit would
    # otherwise take the whole sweep down at the summary line.
    $row = [ordered]@{
        short                        = $short
        persona                      = $Persona
        guest_budget                 = $GuestSeconds
        cycle_budget                 = $cycleBudget
        outcome                      = "UNKNOWN"
        reasons                      = @()
        flags                        = @()
        wall_seconds                 = 0.0
        stderr_lines                 = 0
        stderr_sample                = $null
        error                        = $null
        zip_bytes                    = 0
        translate_class              = $null
        launch                       = $null
        launch_resolved              = $null
        resolved_by_search           = $false
        config_sys_shape             = $null
        cd_image                     = $null
        inject_mouse                 = $null
        memory_mib                   = 0
        choices                      = @()
        exit_code                    = $null
        screen_recurrences           = 0
        phase_mark_count             = 0
        screen_samples               = 0
        screen_distinct              = 0
        stop_kind                    = $null
        real_time_factor             = $null
        guest_seconds                = $null
        machine_phase_timing_enabled = $null
    }

    try {
        # Validate BEFORE deriving any path. `$short` comes from a caller-
        # supplied list and the scratch path built from it is deleted with
        # -Recurse -Force; a line of `..\..\Windows` must never reach Join-Path.
        if (-not (Test-CorpusShort $short)) {
            throw "refusing a corpus short that is not a plain name: '$short'"
        }
        $gameOut = Join-Path $OutDir $short
        New-Item -ItemType Directory -Force -Path $gameOut | Out-Null
        $scratch = Join-Path $ScratchRoot $short
        $stale = Remove-ScratchTree -Path $scratch
        if ($stale) { throw "could not clear a stale scratch tree at ${scratch}: $stale" }

        # Refuse to start a game that cannot fit. The extract guard bounds ONE
        # game; this bounds the volume, which is what a leftover tree eats into.
        $freeGib = Get-FreeGib -Path $ScratchRoot
        if ($freeGib -lt ($MaxExtractGib + 2)) {
            throw "only $freeGib GiB free under ${ScratchRoot}; need $($MaxExtractGib + 2) GiB"
        }

        $conf = Join-Path (Join-Path $dosRoot $short) 'dosbox.conf'
        if (-not (Test-Path -LiteralPath $conf)) { throw "no dosbox.conf for $short" }
        $zip = Resolve-GameZip -DosRoot $dosRoot -CorpusRoot $Corpus -Short $short `
            -Excluded $nonTitleBatNames
        if (-not $zip) { throw "no zip resolved for $short" }

        New-Item -ItemType Directory -Force -Path $scratch | Out-Null
        $extract = Expand-GameZip -Zip $zip -Destination $scratch -MaxGib $MaxExtractGib
        if (-not $extract.Ok) {
            $row.outcome = "UNTRANSLATABLE"
            $row.reasons = @($extract.Reason)
            continue
        }
        $row.zip_bytes = $extract.Bytes

        $translateJson = Join-Path $gameOut 'translate.json'
        $translateArgs = @(
            'translate',
            '--conf', $conf,
            '--extract-root', $scratch,
            '--short', $short,
            '--persona', $Persona,
            '--cycles', "$cycleBudget",
            '--output', $translateJson
        )
        if ($RecipeDir) { $translateArgs += @('--recipe-dir', $RecipeDir) }
        & $Translator @translateArgs | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "translator exited $LASTEXITCODE" }
        $plan = Get-Content -LiteralPath $translateJson -Raw | ConvertFrom-Json

        $row.reasons = @($plan.reasons)
        $row.flags = @($plan.flags)
        $row.translate_class = $plan.class
        $row.launch = $plan.launch_command
        $row.launch_resolved = $plan.launch_resolved
        $row.resolved_by_search = $plan.resolved_by_search
        $row.config_sys_shape = $plan.config_sys_shape
        $row.cd_image = $plan.cd_image
        $row.inject_mouse = $plan.inject_mouse
        $row.memory_mib = $plan.memory_mib
        $row.choices = @($plan.choices)

        if ($plan.class -eq 'UNTRANSLATABLE') {
            $row.outcome = "UNTRANSLATABLE"
            continue
        }

        # Archive the exact configuration the run used, so the row can be
        # re-read without the scratch tree it came from.
        Copy-Item -LiteralPath (Join-Path $plan.hdd_folder 'AUTOEXEC.BAT') -Destination $gameOut -Force
        Copy-Item -LiteralPath (Join-Path $plan.hdd_folder 'CONFIG.SYS') -Destination $gameOut -Force

        $profileJson = Join-Path $gameOut 'profile.json'
        $screensDir = Join-Path $gameOut 'screens'
        $stdoutPath = Join-Path $gameOut 'run.stdout'
        $stderrPath = Join-Path $gameOut 'run.stderr'
        $runArgs = @($plan.invocation)
        $runArgs += @(
            '--profile-json', $profileJson,
            '--result-ppm', (Join-Path $gameOut 'final.ppm'),
            '--screen-dump-dir', $screensDir,
            '--screen-dump-interval-ms', "$ScreenIntervalMs"
        )

        $screenIndex = Join-Path $screensDir 'screens.jsonl'
        Clear-ObserverEnvironment
        $env:IZARRAVM_PHASE_INTERVAL_MS = "$PhaseIntervalMs"
        # The screens index is the run's only periodic writer: stdout does not
        # grow on the --hdd-folder path and the profile JSON is written once, at
        # exit. See the note on Invoke-EmulatorRun.
        $run = Invoke-EmulatorRun -Exe $Executable -Arguments $runArgs -StdoutPath $stdoutPath `
            -StderrPath $stderrPath -TimeoutSeconds $WatchdogSeconds `
            -ProgressPaths @($screenIndex) -StallSeconds $StallSeconds
        $row.wall_seconds = [math]::Round($run.WallSeconds, 3)
        $row.exit_code = $run.ExitCode

        $offending = Test-StderrIsBenign -Path $stderrPath -Patterns $benignStderrPatterns
        $row.stderr_lines = $offending.Count
        if ($offending.Count -gt 0) {
            $row.stderr_sample = $offending[0]
            Write-Error "row $short wrote unrecognised stderr: $($offending[0])" -ErrorAction Continue
        }

        $profile = $null
        if (Test-Path -LiteralPath $profileJson) {
            try { $profile = Get-Content -LiteralPath $profileJson -Raw | ConvertFrom-Json }
            catch { Write-Warning "$short profile JSON will not parse: $_" }
        }
        $screens = @()
        if (Test-Path -LiteralPath $screenIndex) {
            $screens = @(Get-Content -LiteralPath $screenIndex |
                Where-Object { $_.Trim() } |
                ForEach-Object { $_ | ConvertFrom-Json })
        }
        $recurrences = Measure-ScreenRecurrence -Screens $screens
        $row.screen_recurrences = $recurrences
        $row.phase_mark_count = if ($profile -and $profile.PSObject.Properties.Name -contains 'phase_marks' -and $profile.phase_marks) { @($profile.phase_marks).Count } else { 0 }
        $row.screen_samples = $screens.Count
        $row.screen_distinct = @($screens | Select-Object -ExpandProperty hash -Unique).Count
        if ($profile) {
            $row.stop_kind = $profile.stop.kind
            $row.real_time_factor = $profile.real_time_factor
            $row.guest_seconds = $profile.guest_seconds
            # A contaminated row: arming marks must not arm phase timing.
            $row.machine_phase_timing_enabled = $profile.machine_phase_timing_enabled
        }
        $row.outcome = Get-Outcome -Profile $profile -Screens $screens -ScreenRecurrences $recurrences `
            -TimedOut $run.TimedOut -Stalled $run.Stalled -MinMarks 31 `
            -RebootThreshold $rebootRecurrenceThreshold
        if ($run.Stalled) {
            Write-Error "row $short wrote nothing for $($run.QuietSeconds)s and was killed" -ErrorAction Continue
        }
    } catch {
        $row.outcome = "HARNESS-ERROR"
        $row.error = "$_"
        Write-Error "row $short failed: $_" -ErrorAction Continue
    } finally {
        # Delete the GAME FILES, never the collected data.
        if (-not $KeepScratch -and $scratch -and (Test-Path -LiteralPath $scratch)) {
            $left = Remove-ScratchTree -Path $scratch
            if ($left) { Write-Error "row ${short}: scratch tree survives at ${scratch}: $left" -ErrorAction Continue }
        }
        # THE ONE EMIT POINT. Every outcome leaves through here, including the
        # `continue`s above (PowerShell runs `finally` on the way out of a try),
        # so a sweep killed mid-corpus still has every row it finished on disk.
        $emitted = [pscustomobject]$row
        $rows += $emitted
        ($emitted | ConvertTo-Json -Depth 6 -Compress) |
            Add-Content -LiteralPath (Join-Path $OutDir 'rows.jsonl')
        Write-Host "    $($row.outcome) wall=$($row.wall_seconds)s marks=$($row.phase_mark_count) screens=$($row.screen_samples)/$($row.screen_distinct)"
    }
}

$rows | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutDir 'sweep.json')
$rows | Select-Object short, outcome, translate_class, wall_seconds, real_time_factor,
    phase_mark_count, screen_samples, screen_distinct, stderr_lines |
    Export-Csv -LiteralPath (Join-Path $OutDir 'sweep.csv') -NoTypeInformation
$rows | Group-Object outcome | Sort-Object Count -Descending |
    ForEach-Object { "{0,-18} {1}" -f $_.Name, $_.Count }
