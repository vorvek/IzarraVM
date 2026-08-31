# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
Run a prepared DOS directory tree under DOSBox-X as a behavioural ORACLE for IzarraVM.

.DESCRIPTION
DOSBox-X is prior art, not a dependency: nothing it produces is a merge gate. It is
here so a campaign can ask "what does another emulator do with this program, at a
comparable CPU speed?" and get an answer in seconds instead of an afternoon.

The script does four things and nothing else:

  1. Builds a per-run dosbox.conf under $env:IZARRAVM_ORACLE_SCRATCH
     (default D:\ctd\oracle-scratch). NOTHING is written inside the repo.
  2. Mounts -Dir as C:, appends -Command to [autoexec], and terminates the
     autoexec with `exit` so the emulator quits on its own.
  3. Launches DOSBox-X headless (see -Headed to watch it) with a hard
     -time-limit watchdog, and waits.
  4. Writes run.json next to the conf and the log: wall seconds, whether the
     autoexec actually finished, the exact argument vector, the resolved cycles.

DO NOT READ THE EXIT CODE. DOSBox-X returns 0 unconditionally (except when
running its own gtest suite), whether it completed, hit a time limit, or was
killed. Use the `Completed` field instead: the script appends a unique sentinel
`echo` to [autoexec] and looks for it in the captured CON output.

PERSONAS. The cycles numbers are the owner-supplied DOSBox-X equivalents of the
IzarraVM benchmark personas, from
https://dosbox-x.com/wiki/Guide%3ACPU-settings-in-DOSBox%E2%80%90X#_cycles

    386-slow    2000     386SX-class
    386         4300     386DX-class
    486        23880     486DX2/66, the IzarraVM 486 persona
    586        95000     Pentium 166, the IzarraVM 586 persona

Cycles are a THROTTLE, not a model. DOSBox-X retires `cycles` instructions per
emulated millisecond regardless of what those instructions cost on real silicon,
so a cycles figure is a rough speed peg, never an instruction-timing oracle.
Treat any timing comparison against IzarraVM as ballpark evidence only.

.PARAMETER Dir
Host directory mounted as C:. Must already be prepared (see .bench/corpus). The
corpus convention is to copy game files to D:\ctd\corpus-scratch first, because
the source collections are read-only for runs; this script does no copying.

.PARAMETER Command
One or more DOS commands appended to [autoexec] after `C:`. A sentinel echo and
`exit` are added automatically (the latter unless -NoExit is given).

Batch files CHAIN. `-Command 'RUN.BAT','DIR'` runs RUN.BAT and never comes back,
so the DIR, the sentinel and the exit are all dead lines. Write `CALL RUN.BAT`
if anything is supposed to run after it.

.PARAMETER Persona
386-slow | 386 | 486 | 586. Sets [cpu] cycles and a default cputype.

.PARAMETER Cycles
Overrides the persona cycles. Accepts a bare integer (becomes `fixed N`), `max`,
or `auto`. `max` uncaps the instruction budget but does NOT uncap wall-clock
time: use -Turbo for that.

.PARAMETER Turbo
Sets [cpu] turbo=true, DOSBox-X's fast-forward. This is the closest thing to
"run as fast as the host allows"; it accelerates the emulated clock too, so
anything you measure under -Turbo is a throughput number, never a timing one.

.PARAMETER Set
Extra dosbox.conf keys as "section:key=value" strings, applied after the
persona defaults so they win. Example: -Set 'dosbox:machine=svga_s3','mixer:nosound=true'
Key names may contain spaces ("cpu:cputype=pentium", "cpu:enable msr=true").

.PARAMETER LogTypes
[log] categories to raise to debug for this run. Accepts the DOSBox-X category
names: vga vgagfx vgamisc int10 sblaster dma_control fpu cpu paging fcb files
ioctl exec dosmisc pit keyboard pic mouse bios gui misc io pci sst int21 fileio
Use 'io' for port I/O and 'pit'/'pic' for timer and interrupt-controller chatter.
Pass 'all' for every category (very large logs).

.PARAMETER LogCon
Capture the guest's CON (screen) output into the log as "DOS CON:" lines. On by
default; -LogCon:$false turns it off.

.PARAMETER TimeLimit
EMULATED seconds, passed to DOSBox-X's own -time-limit. DOSBox-X checks this
against PIC_FullIndex, i.e. the guest's clock, so it does NOT bound host time and
it does not fire at all if the emulated clock stops advancing. Default 120.

.PARAMETER WallLimit
Real host seconds after which this script kills the process tree. This is the
watchdog that actually works. Default 0 = derive it as TimeLimit*3 + 30.

.PARAMETER Headed
Show the DOSBox-X window instead of running headless. Useful when a program
hangs and you want to see where.

.PARAMETER BreakStart
Pass -break-start, which drops into the built-in debugger before the first
instruction. Implies -Headed (the debugger is interactive).

.PARAMETER RunName
Label for the scratch subdirectory. Defaults to the leaf name of -Dir.

.PARAMETER DosboxExe
Path to dosbox-x.exe. Auto-detected from $env:IZARRAVM_DOSBOX_X, then the
registry uninstall entry, then the usual install paths.

.EXAMPLE
Smoke test, headless, prints the run record:

  .\.bench\oracle\Invoke-DosboxOracle.ps1 -Dir D:\ctd\oracle-scratch\testdir `
      -Command 'HI.BAT','DIR' -Persona 586

.EXAMPLE
Port-I/O trace of a game at the 486 persona:

  .\.bench\oracle\Invoke-DosboxOracle.ps1 -Dir D:\ctd\corpus-scratch\tyrian `
      -Command 'TYRIAN.EXE' -Persona 486 -LogTypes io,pit,pic -TimeLimit 90

.OUTPUTS
A PSCustomObject: Completed, WallSeconds, KilledByWallLimit, Cycles, RunDir,
ConfPath, LogPath, LogBytes, Sentinel, Arguments, and ExitCode (always 0, kept
only so nobody goes looking for it). The same record is written to
<RunDir>\run.json.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Dir,
    [Parameter(Mandatory)][string[]]$Command,
    [ValidateSet('386-slow', '386', '486', '586')][string]$Persona = '586',
    [string]$Cycles,
    [switch]$Turbo,
    [string[]]$Set = @(),
    [string[]]$LogTypes = @(),
    [bool]$LogCon = $true,
    [int]$TimeLimit = 120,
    [int]$WallLimit = 0,
    [switch]$Headed,
    [switch]$BreakStart,
    [switch]$NoExit,
    [string]$RunName,
    [string]$DosboxExe
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------- personas ---
# Owner-supplied DOSBox-X cycles equivalents. cputype is this script's choice of
# a plausible companion, not part of the owner's mapping; override with -Set.
$PersonaMap = @{
    '386-slow' = @{ cycles = 2000;  cputype = '386'     }
    '386'      = @{ cycles = 4300;  cputype = '386'     }
    '486'      = @{ cycles = 23880; cputype = '486'     }
    '586'      = @{ cycles = 95000; cputype = 'pentium' }
}

$LOG_CATEGORIES = @(
    'vga', 'vgagfx', 'vgamisc', 'int10', 'sblaster', 'dma_control', 'fpu', 'cpu',
    'paging', 'fcb', 'files', 'ioctl', 'exec', 'dosmisc', 'pit', 'keyboard',
    'pic', 'mouse', 'bios', 'gui', 'misc', 'io', 'pci', 'sst', 'int21', 'fileio'
)

# ------------------------------------------------------------ locate exe -----
function Resolve-DosboxExe {
    param([string]$Explicit)
    foreach ($c in @($Explicit, $env:IZARRAVM_DOSBOX_X)) {
        if ($c -and (Test-Path -LiteralPath $c)) { return (Resolve-Path -LiteralPath $c).Path }
    }
    $keys = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    $entry = Get-ItemProperty $keys -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -like 'DOSBox-X*' -and $_.InstallLocation } |
        Select-Object -First 1
    if ($entry) {
        $p = Join-Path $entry.InstallLocation 'dosbox-x.exe'
        if (Test-Path -LiteralPath $p) { return (Resolve-Path -LiteralPath $p).Path }
    }
    foreach ($p in @('D:\DOSBox-X\dosbox-x.exe',
                     "$env:ProgramFiles\DOSBox-X\dosbox-x.exe",
                     "${env:ProgramFiles(x86)}\DOSBox-X\dosbox-x.exe")) {
        if (Test-Path -LiteralPath $p) { return (Resolve-Path -LiteralPath $p).Path }
    }
    throw "dosbox-x.exe not found. Install it (winget install joncampbell123.DOSBox-X) or set IZARRAVM_DOSBOX_X."
}

$exe = Resolve-DosboxExe -Explicit $DosboxExe
$headedRequested = [bool]($Headed -or $BreakStart)

if (-not (Test-Path -LiteralPath $Dir -PathType Container)) {
    throw "-Dir '$Dir' is not an existing directory. Prepare the DOS tree first."
}
$mountDir = (Resolve-Path -LiteralPath $Dir).Path

# ------------------------------------------------------------- scratch -------
# Generated confs and logs NEVER live in the repo; .bench data is not tracked.
$scratchRoot = if ($env:IZARRAVM_ORACLE_SCRATCH) { $env:IZARRAVM_ORACLE_SCRATCH } else { 'D:\ctd\oracle-scratch' }
if (-not $RunName) { $RunName = Split-Path -Leaf $mountDir }
$safeName = ($RunName -replace '[^A-Za-z0-9._-]', '_')
$runDir = Join-Path $scratchRoot ("{0}-{1:yyyyMMdd-HHmmss}" -f $safeName, (Get-Date))
$null = New-Item -ItemType Directory -Force -Path $runDir

$confPath = Join-Path $runDir 'dosbox-x.conf'
$logPath = Join-Path $runDir 'dosbox-x.log'

# --------------------------------------------------------------- cycles ------
$p = $PersonaMap[$Persona]
if ($Cycles) {
    $cyclesValue = if ($Cycles -match '^\d+$') { "fixed $Cycles" } else { $Cycles }
} else {
    $cyclesValue = "fixed $($p.cycles)"
}

# --------------------------------------------------------- conf assembly -----
# Ordered section -> ordered key/value. -Set is applied last so it always wins.
$conf = [ordered]@{}
function Set-Conf {
    param([string]$Section, [string]$Key, [string]$Value)
    if (-not $conf.Contains($Section)) { $conf[$Section] = [ordered]@{} }
    $conf[$Section][$Key] = $Value
}

Set-Conf sdl autolock 'false'
Set-Conf sdl output 'surface'          # dummy SDL video driver cannot do direct3d
Set-Conf sdl waitonerror 'false'       # never block on a modal error box
Set-Conf sdl usescancodes 'false'

Set-Conf log logfile $logPath
foreach ($c in $LOG_CATEGORIES) { Set-Conf log $c 'false' }
$wantLogs = if ($LogTypes -contains 'all') { $LOG_CATEGORIES } else { $LogTypes }
foreach ($t in $wantLogs) {
    if ($LOG_CATEGORIES -notcontains $t) {
        throw "Unknown -LogTypes category '$t'. Known: $($LOG_CATEGORIES -join ', ')"
    }
    Set-Conf log $t 'debug'
}

Set-Conf dosbox title "izarravm-oracle-$safeName"
Set-Conf dosbox memsize '16'

Set-Conf cpu core 'auto'
Set-Conf cpu cputype $p.cputype
Set-Conf cpu cycles $cyclesValue
Set-Conf cpu turbo $(if ($Turbo) { 'true' } else { 'false' })
Set-Conf cpu 'stop turbo on key' 'false'

Set-Conf mixer nosound $(if ($Headed) { 'false' } else { 'true' })

foreach ($s in $Set) {
    if ($s -notmatch '^\s*([^:]+?)\s*:\s*(.+?)\s*=\s*(.*)$') {
        throw "-Set entry '$s' is not in 'section:key=value' form."
    }
    # sdlmain.cpp applies [sdl] videodriver via putenv AFTER -silent has set
    # SDL_VIDEODRIVER=dummy, so setting it here silently un-headlesses the run.
    if ($Matches[1] -eq 'sdl' -and $Matches[2] -eq 'videodriver' -and -not $headedRequested) {
        Write-Warning "-Set 'sdl:videodriver=...' overrides -silent's dummy driver; the run will not be headless."
    }
    Set-Conf $Matches[1] $Matches[2] $Matches[3]
}

$sentinel = "IZARRAVM_ORACLE_DONE_$([guid]::NewGuid().ToString('N').Substring(0,8).ToUpper())"

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('# Generated by .bench/oracle/Invoke-DosboxOracle.ps1 -- do not edit, do not commit.')
$lines.Add("# persona=$Persona cycles=$cyclesValue turbo=$([bool]$Turbo) mount=$mountDir")
foreach ($section in $conf.Keys) {
    $lines.Add('')
    $lines.Add("[$section]")
    foreach ($k in $conf[$section].Keys) { $lines.Add("$k = $($conf[$section][$k])") }
}
$lines.Add('')
$lines.Add('[autoexec]')
$lines.Add("mount c `"$mountDir`"")
$lines.Add('c:')
foreach ($c in $Command) { $lines.Add($c) }
# DOSBox-X always exits 0 (sdlmain.cpp: `return saved_opt_test && testerr ? 1 : 0`),
# so the exit code says NOTHING about whether the run completed. The sentinel is the
# only honest completion signal: if it is not in the log, the autoexec did not finish.
# Note a bare `FOO.BAT` in [autoexec] CHAINS (does not return), so the sentinel will
# be absent for batch commands unless they are invoked with CALL.
$lines.Add("echo $sentinel")
if (-not $NoExit) { $lines.Add('exit') }

# ASCII: DOSBox-X's conf parser is byte-oriented and a BOM breaks the first key.
[System.IO.File]::WriteAllLines($confPath, $lines, [System.Text.UTF8Encoding]::new($false))

# ---------------------------------------------------------------- launch -----
$headed = $headedRequested
$argv = [System.Collections.Generic.List[string]]::new()
if (-not $headed) {
    # -silent implies SDL_VIDEODRIVER=dummy + SDL_AUDIODRIVER=dummy and exits
    # after AUTOEXEC. -fastlaunch skips the BIOS logo and welcome banner.
    $argv.Add('-silent')
}
$argv.AddRange([string[]]@('-fastlaunch', '-nogui', '-nomenu', '-nopromptfolder', '-defaultmapper'))
if (-not $NoExit) { $argv.Add('-exit') }
if ($LogCon) { $argv.Add('-log-con') }
if ($BreakStart) { $argv.Add('-break-start') }
if ($TimeLimit -gt 0) { $argv.AddRange([string[]]@('-time-limit', "$TimeLimit")) }
$argv.AddRange([string[]]@('-conf', $confPath))

Write-Verbose "$exe $($argv -join ' ')"

# TWO watchdogs, because DOSBox-X's own one is not enough.
#
# -time-limit counts EMULATED time (PIC_FullIndex), so it cannot rescue a run whose
# emulated clock has stopped advancing. That is not hypothetical: a `:X / goto X`
# loop in [autoexec] is executed by DOSBox-X's native batch interpreter, spins the
# host CPU at 100%, and never trips -time-limit at all. -WallLimit is the real one.
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process -FilePath $exe -ArgumentList $argv -PassThru `
    -WindowStyle $(if ($headed) { 'Normal' } else { 'Hidden' }) `
    -WorkingDirectory $runDir

$wallLimit = if ($WallLimit -gt 0) { $WallLimit } elseif ($TimeLimit -gt 0) { $TimeLimit * 3 + 30 } else { 0 }
$killedByWall = $false
if ($wallLimit -gt 0) {
    if (-not $proc.WaitForExit([int][math]::Min([double]$wallLimit * 1000, [double][int]::MaxValue))) {
        $killedByWall = $true
        try { $proc.Kill($true) } catch { }
        $null = $proc.WaitForExit(10000)
    }
} else {
    $proc.WaitForExit()
}
$sw.Stop()

# Completion is read out of the log, not out of the exit code.
$completed = $false
if ($LogCon -and (Test-Path -LiteralPath $logPath)) {
    $completed = [bool](Select-String -LiteralPath $logPath -Pattern $sentinel -SimpleMatch -Quiet)
}

$record = [pscustomobject]@{
    RunName     = $RunName
    Exe         = $exe
    Persona     = $Persona
    Cycles      = $cyclesValue
    Turbo       = [bool]$Turbo
    MountDir    = $mountDir
    Command     = $Command
    ExitCode    = $(try { $proc.ExitCode } catch { $null })   # always 0; do not trust it
    WallSeconds = [math]::Round($sw.Elapsed.TotalSeconds, 3)
    Completed   = $completed
    KilledByWallLimit = $killedByWall
    Sentinel    = $sentinel
    RunDir      = $runDir
    ConfPath    = $confPath
    LogPath     = $logPath
    LogBytes    = $(if (Test-Path -LiteralPath $logPath) { (Get-Item -LiteralPath $logPath).Length } else { 0 })
    Arguments   = $argv.ToArray()
}
$record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runDir 'run.json')
$record
