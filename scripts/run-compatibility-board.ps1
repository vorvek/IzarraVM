# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#Requires -Version 7.4

<#
.SYNOPSIS
The compatibility board: one pass over the game fixtures chosen for the graphics
modes the performance scoreboard never exercises.

.DESCRIPTION
WHY THIS EXISTS. A defect that erased the raster once a frame lived in the VGA
core while every row of `scripts/run-fixture-scoreboard.ps1` stayed green,
because no row replayed its CRTC per frame. `psycho-486` was added afterwards to
close that one hole. The board still had no CGA row, no EGA row, no Hercules
row, no unusual-line-count row, and nothing that exercised Distira at all.

WHY THIS IS NOT run-fixture-scoreboard.ps1. That board measures performance and
grades one frame as a side effect. This one grades pictures and modes and
reports no performance figure at all. NO NUMBER FROM THIS SCRIPT MAY ENTER AN
A/B OR A SCOREBOARD CLAIM: it runs under whatever load the box has.

WHAT A ROW GRADES. Four checks, and all four must pass:
  1. a published frame exists at all
  2. its content sits inside the pinned colour and non-black bands
  3. the mode census holds the row's target mode -- THE ACCEPTANCE RULE
  4. the retired instruction count is within 5%

Check 3 is the one this board adds. Every game here shows a menu before it
shows the mode it was chosen for, so a row graded at an arbitrary budget grades
a menu and proves nothing. Jazz Jackrabbit measured 320x400 in the 2026-08-29
sweep, which was its menu, not the aspect-defeating mode it is here for.

Check 4 catches a run that never launched the game. A run parked at the DOS
prompt retires a wildly different count, and it misses by far more than 5%.

THE PUBLISHED FRAME, NEVER A RE-RENDER. `--result-ppm` re-renders the whole
frame at stop-time register state, so it reports what video memory holds. With
the raster-erase defect restored, Psycho Pinball reads 82.9% non-black through
`--result-ppm` and 0.0% through `--presented-ppm` on the same run. A board built
on the re-render would have graded that defect green.

.EXAMPLE
pwsh -File scripts/run-compatibility-board.ps1 -SelfTest

.EXAMPLE
pwsh -File scripts/run-compatibility-board.ps1 -Rows jazz-486,keen4-486
#>

# POSITIONAL BINDING IS OFF for the whole param block, for the reason recorded
# in scripts/sweep-exodos.ps1: under `pwsh -File` a [string[]] parameter takes
# exactly ONE token, and a second token binds POSITIONALLY to the next free
# parameter. Measured 2026-08-27 on run-fixture-scoreboard.ps1, that shape ran
# ONE row of a two-row sweep and EXITED 0. With positional binding off, the
# stray token is a binder error before one line of this script runs.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = "Run")]
param(
    # Rows to run. The safe multi-row spelling is the COMMA string: `-Rows a,b`.
    [string[]]$Rows,
    [string]$Label = "board",
    [string]$Executable = "D:\dev\IzarraVM\target\release\izarravm.exe",
    # The fixture trees. NOT in the repository: the games are commercial installs
    # that cannot be redistributed.
    [string]$FixtureRoot = "D:\dev\IzarraVM\.bench",
    [string]$OutRoot = "D:\dev\IzarraVM\compatibility-output",
    # Record the measured values as the new pins instead of grading against them.
    [switch]$RecordInvariants,
    [Parameter(ParameterSetName = "SelfTest")][switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# The row table.
#
# Budgets, personas and key schedules are per-row and are MEASURED during
# bring-up, never guessed. A row whose `cycles` is $null has not been brought up
# and the board refuses to run it rather than inventing a budget.
# ---------------------------------------------------------------------------
function Get-RowTable {
    @(
        [pscustomobject]@{
            name = "pinball-fantasies-486"; folder = "pinbllf_c"
            # 32 MiB, not 64: its conf says memsize=32 and that is what the
            # packager chose. `machine=vgaonly`.
            arguments = @("--cpu", "486", "--memory-mib", "32", "--video", "vega")
            # 6.6e9 clocks is 100 guest seconds at 486/66 MHz. MEASURED
            # timeline: boot to 2.5 s, mode X intro 2.5-23.5 s, planar 640x480
            # title and attract loop 23.5-41.5 s, and the table in play from
            # 41.5 s to the end. The end frame lands about a minute into a
            # table, the way psycho-486 does.
            cycles = [uint64]6600000000
            # Enter through the intro and the title, F1 to pick Party Land at
            # 40 s, then the plunger and flippers on space every 3 s. WITHOUT
            # the F1 the run parks on the attract loop forever: measured over
            # 121 guest seconds with no input, it never leaves it.
            injectKeys = "330000000:{enter};462000000:{enter};594000000:{enter};726000000:{enter};858000000:{enter};990000000:{enter};1122000000:{enter};1254000000:{enter};1386000000:{enter};1518000000:{enter};1650000000:{enter};1782000000:{enter};1914000000:{enter};2640000000:{f1};2970000000:{space};3168000000:{space};3366000000:{space};3564000000:{space};3762000000:{space};3960000000:{space};4158000000:{space};4356000000:{space};4554000000:{space};4752000000:{space};4950000000:{space};5148000000:{space};5346000000:{space};5544000000:{space};5742000000:{space};5940000000:{space};6138000000:{space}"
            injectMouse = $null
            # THE SURVEY'S 256-WIDE CLAIM DOES NOT REPRODUCE. It named "mode X
            # 320x480, 640x480 and 256x480" and that is why this title was the
            # board's strongest candidate. MEASURED over three runs, including
            # into gameplay, every presented frame is 720x400, 320x480, 640x480
            # or 320x350. No 256-wide frame exists and the census agrees.
            #
            # What the row DOES carry, and why it stays:
            #   - a 350-line mode X, NOT double scanned. 350 source rows is not
            #     200, 240 or 480, and it only exists once a table is loaded, so
            #     this target cannot be satisfied by a menu.
            #   - a PLANAR per-frame CRTC replayer. Its 640x480 attract loop
            #     reprograms the CRTC 5164 times in 121 guest seconds. psycho-486
            #     replays in mode X; a planar replayer is new coverage.
            #   - mode X programmed 360 dots wide, which nothing else reaches.
            targetMode = "ModeX"; targetNote = "source_lines 350"
        }
        [pscustomobject]@{
            name = "keen4-486"; folder = "ckeen4_c"
            # 16 MiB, from its own conf.
            arguments = @("--cpu", "486", "--memory-mib", "16", "--video", "vega")
            # 10e9 clocks is 151 guest seconds. MEASURED: one key clears
            # "Ready - Press a Key" at 5 s, the title cycles, and the ATTRACT
            # DEMO plays a real level well before the budget.
            cycles = [uint64]10000000000
            # ONE key, then nothing. Walking to a level entrance was tried twice
            # -- a square with {ctrl} presses, then long single-direction legs --
            # and both left Keen on the world map. Sending nothing lets the title
            # cycle to its demo, which plays a level by itself.
            injectKeys = "330000000:{enter}"
            injectMouse = $null
            # NOT line_compare_active. The survey calls this "EGA smooth scroll
            # with a split-screen status panel"; MEASURED at the title, on the
            # world map and inside a demo level, line compare is never active.
            # The Galaxy engine draws its level full-screen and its status into
            # the same buffer.
            #
            # THIS ROW'S CENSUS TARGET IS THE WEAKEST ON THE BOARD, and that is
            # stated rather than hidden: Planar is set ONCE for the whole run, so
            # geometry cannot separate the title from gameplay. Its gameplay
            # evidence is the content bands and the instruction count, not the
            # census. It earns its place as the only row whose gameplay is
            # reached with a single keystroke and no schedule to rot.
            targetMode = "Planar"; targetNote = ""
        }
        [pscustomobject]@{
            name = "jazz-486"; folder = "jazzjack_c"
            # 16 MiB, from its own conf.
            arguments = @("--cpu", "486", "--memory-mib", "16", "--video", "vega")
            # 8e9 clocks is 121 guest seconds. NO INPUT SCHEDULE AT ALL: this
            # title reaches its target geometry within 76 guest seconds on its
            # own, which makes it the one row with nothing to rot.
            cycles = [uint64]8000000000
            injectKeys = $null; injectMouse = $null
            # MEASURED 2026-08-30: the mode is ModeX, not Mode13h, and it reads
            # 320x398 with double_scan set -- 199 SOURCE rows. The survey reported
            # "mode 13h 320x400" for this title and concluded the non-standard
            # line count was NOT reached; both halves of that are wrong. It is
            # reached, in mode X, within 76 guest seconds and with no input.
            #
            # 199 SOURCE rows, not 199 raster lines. vdisp_end counts raster
            # lines, so this geometry reads vdisp_end 398 and a rule written
            # against vdisp_end could never match.
            targetMode = "ModeX"; targetNote = "source_lines 199"
        }
        [pscustomobject]@{
            name = "koreatetris-486"; folder = "koreatet_c"
            # 64 MiB: its conf asks for 61, which translate clamps up to 64.
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            # 8e9 clocks is 121 guest seconds, well inside a played board.
            cycles = [uint64]8000000000
            # THE ARROW KEYS, and nothing else works. {enter}, {space}, 1,
            # {ctrl}, {alt}, s, g, {f1} and y were each tried five seconds apart
            # and all nine left the menu exactly where it was. {down} and {up}
            # move the cursor and start the game.
            injectKeys = "660000000:{down};924000000:{down};1188000000:{down};1452000000:{up};1716000000:{up};1980000000:{up};2244000000:{enter};2508000000:{space}"
            injectMouse = $null
            # The corpus records machine=hercules and the 2026-08-29 sweep
            # measured CGA 320x200. The corpus records what DOSBox was TOLD, not
            # what the guest chose. If the census reports Hercules instead, that
            # is a finding to write down, not a failure.
            targetMode = "Cga"; targetNote = ""
        }
        [pscustomobject]@{
            name = "zone66-486"; folder = "zone66_c"
            # memsize=8 in its conf, and translate clamps to 4..64, so this row
            # gets 8 MiB where the others get 64. That is what the packager
            # chose. An out-of-memory exit and a crash look alike on a screen
            # dump, so check this before blaming the emulator.
            arguments = @("--cpu", "486", "--memory-mib", "8", "--video", "vega")
            # 12e9 clocks is 182 guest seconds: the story pages take a while to
            # skip even with ESC, and the mission itself starts around 60 s.
            cycles = [uint64]12000000000
            # ESC skips the story pages, Enter takes the default ship and answers
            # the launch prompt, then space fires through the mission.
            injectKeys = "264000000:{esc};363000000:{esc};462000000:{esc};561000000:{esc};660000000:{esc};759000000:{esc};858000000:{esc};957000000:{esc};1056000000:{esc};1155000000:{esc};1320000000:{enter};1452000000:{enter};1584000000:{enter};1716000000:{enter};1848000000:{enter};1980000000:{enter};2112000000:{enter};2244000000:{enter};2376000000:{space};2508000000:{space};2640000000:{space};2772000000:{space};2904000000:{space};3036000000:{space};3300000000:{space};3630000000:{space};3960000000:{space};4290000000:{space};4620000000:{space};4950000000:{space};5280000000:{space};5610000000:{space};5940000000:{space};6270000000:{space};6600000000:{space};6930000000:{space};7260000000:{space};7590000000:{space};7920000000:{space};8250000000:{space};8580000000:{space};8910000000:{space};9240000000:{space};9570000000:{space};9900000000:{space};10230000000:{space};10560000000:{space};10890000000:{space};11220000000:{space}"
            injectMouse = $null
            # NOT ModeX. The survey calls this title "unchained mode X with its
            # own rasteriser" and that is why it was shortlisted. MEASURED over
            # four runs, one of them into a flown mission: the census holds
            # exactly ONE graphics geometry, Mode13h 320x400 double-scanned, 200
            # source rows. Chained mode 13h. The custom rasteriser is real but it
            # is software, which no census can see.
            #
            # THE ROW STAYS FOR A DIFFERENT REASON, and a better one. Zone 66 is
            # the only fixture anywhere that runs a guest under its OWN
            # descriptor tables: it enters protected mode through VCPI, builds a
            # 7-entry GDT and a 49-vector IDT at ring 0, and runs a V86 task
            # beneath them. That path crashed the emulator at 3 guest seconds
            # until commit 081293d8, and this row is what would catch it coming
            # back.
            targetMode = "Mode13h"; targetNote = ""
        }
        [pscustomobject]@{
            name = "cabal-486"; folder = "cabal_c"
            # 16 MiB, from its own conf.
            arguments = @("--cpu", "486", "--memory-mib", "16", "--video", "vega")
            # 8e9 clocks is 121 guest seconds, inside real play.
            cycles = [uint64]8000000000
            # Its FIRST screen is a video-mode prompt: "Press F1 for CGA video
            # mode, or any other key for default mode". The default is EGA, which
            # is this row's target, so any key will do and Enter is the key sent.
            # This was read as a launch failure before it was looked at; it was
            # never a defect.
            injectKeys = "528000000:{enter};726000000:{enter};924000000:{enter};1122000000:{enter};1320000000:{enter};1518000000:{enter};1716000000:{enter};1914000000:{enter};2112000000:{enter};2640000000:{space};2904000000:{space};3168000000:{space};3432000000:{space};3696000000:{space};3960000000:{space};4224000000:{space};4488000000:{space};4752000000:{space};5016000000:{space};5280000000:{space};5544000000:{space};5808000000:{space};6072000000:{space};6336000000:{space};6600000000:{space};6864000000:{space};7128000000:{space}"
            injectMouse = $null
            # RECOVERABLE for `pause-prompt`: its AUTOEXEC prints "Press M for
            # music in game" and calls pause. The schedule has to answer it.
            targetMode = "Planar"; targetNote = ""
        }
        [pscustomobject]@{
            name = "tombraid3d-586"; folder = "tombraid3d_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
            # Glide, dynamic link. TOMB3D ships its own glide2x.ovl and a local
            # OVL takes priority over the global one, so this row needs no file
            # the repository cannot supply.
            targetMode = "Distira"; targetNote = ""
            cdImage = "tombraid3d_cd\tombeng.cue"
        }
        [pscustomobject]@{
            name = "descent2-3dfx-586"; folder = "descent2_c"
            arguments = @("--cpu", "586", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
            # Glide. run.bat is a two-level menu: choice /C:1234567 for the sound
            # device, then choice /C:123 for the renderer, so 3DFX is the key
            # pair `2` then `2`. Both link models are in the tree.
            targetMode = "Distira"; targetNote = ""
            cdImage = "descent2_cd\DESCENT_II.cue"
        }
    )
}

# A row's optional field, probed by name. Under StrictMode, reading `.Value` off
# a property that does not exist THROWS. Doing this inline cost a scoreboard run
# once: two contract rows passed and a third died on the property access.
function Get-RowField($Row, [string]$Name) {
    $property = $Row.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

<#
Parse the -Rows selection into a validated list of row names.

A null element, an unknown name or a repeat is a hard error, not a warning. An
empty selection means every row.
#>
function Resolve-RowSelection {
    param([string[]]$Selection, [string[]]$KnownNames)
    if ($null -eq $Selection -or $Selection.Count -eq 0) { return $KnownNames }
    $names = @()
    foreach ($element in $Selection) {
        if ($null -eq $element) {
            throw "-Rows contains a null entry. Name each row, comma-separated."
        }
        foreach ($name in ($element -split ",")) {
            $name = $name.Trim()
            if ($name -eq "") { continue }
            if ($KnownNames -notcontains $name) {
                throw "Unknown row '$name'. Known: $($KnownNames -join ', ')"
            }
            if ($names -contains $name) {
                throw "-Rows names '$name' more than once. The board runs each row once."
            }
            $names += $name
        }
    }
    if ($names.Count -eq 0) {
        throw "-Rows selected nothing. Omit it to run every row."
    }
    return $names
}

function Assert-BoardEqual($Actual, $Expected, [string]$What) {
    if ($Actual -ne $Expected) {
        throw "compatibility board self-test failed: $What is $Actual, expected $Expected"
    }
}

# ---------------------------------------------------------------------------
# Frame statistics.
#
# COPIED VERBATIM from scripts/run-fixture-scoreboard.ps1, comments included,
# because the [int] casts below are load-bearing and were paid for once already.
# Do not simplify. The duplication is deliberate: extracting a shared module
# means editing that script while another session is measuring out of it.
# ---------------------------------------------------------------------------
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

function New-SelfTestPpm([string]$Path, [int]$Width, [int]$Height, [scriptblock]$Pixel) {
    $header = [Text.Encoding]::ASCII.GetBytes("P6`n$Width $Height`n255`n")
    $body = [byte[]]::new($Width * $Height * 3)
    for ($i = 0; $i -lt $Width * $Height; $i++) {
        $rgb = & $Pixel $i
        $body[$i * 3] = $rgb[0]; $body[$i * 3 + 1] = $rgb[1]; $body[$i * 3 + 2] = $rgb[2]
    }
    [IO.File]::WriteAllBytes($Path, ($header + $body))
}

# The frame-stats self-test, also carried over. It exists because the colour
# counter was WRONG on its first outing in a way no reviewer caught and no crash
# reported: the channel packing dropped red and green, so it counted distinct
# BLUE values and read 49 where the frame held 174.
function Assert-BoardFrameStatsSelfTest {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-compat-selftest-" +
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
        Assert-BoardEqual $sharedStats.distinct_colors 4 "colours differing only in red and green"
        Assert-BoardEqual $sharedStats.non_black_pct 100.0 "all-non-black percent"

        # Black is all three channels zero, and only that. A pixel with a single
        # channel set counts as painted.
        $black = Join-Path $directory "black.ppm"
        New-SelfTestPpm $black 4 1 { param($i) @(@(0, 0, 0), @(0, 0, 0), @(0, 0, 1), @(0, 0, 0))[$i] }
        $blackStats = Get-PpmFrameStats $black
        Assert-BoardEqual $blackStats.non_black_pct 25.0 "quarter coverage"
        Assert-BoardEqual $blackStats.distinct_colors 2 "black plus one colour"

        $solid = Join-Path $directory "solid.ppm"
        New-SelfTestPpm $solid 8 8 { param($i) @(85, 93, 93) }
        $solidStats = Get-PpmFrameStats $solid
        Assert-BoardEqual $solidStats.distinct_colors 1 "solid fill colours"
        Assert-BoardEqual $solidStats.width 8 "parsed width"

        # A truncated frame is not a frame. It must return $null so the row FAILS
        # rather than grading a partial picture.
        $truncated = Join-Path $directory "truncated.ppm"
        $bytes = [IO.File]::ReadAllBytes($solid)
        [IO.File]::WriteAllBytes($truncated, $bytes[0..($bytes.Length - 40)])
        if ($null -ne (Get-PpmFrameStats $truncated)) {
            throw "compatibility board self-test failed: a truncated PPM parsed as a frame"
        }
        if ($null -ne (Get-PpmFrameStats (Join-Path $directory "absent.ppm"))) {
            throw "compatibility board self-test failed: a missing PPM parsed as a frame"
        }
    } finally {
        Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

<#
The environment every row runs under.

WHY THIS EXISTS. A board that does not scrub the environment pins invariants
measured under whatever the parent shell happened to hold. That failure is
silent, it is DURABLE -- the pins outlive the shell that made them -- and
nothing downstream can detect it, because every later run reproduces the wrong
number faithfully. This board sets no knobs of its own, so the correct
environment is simply "no observer at all".

AN ALLOWLIST, NOT A DENYLIST. Every `IZARRAVM_*` name found in the parent is
cleared, rather than a named list of the ones this script's author happened to
know about. A named list only removes what was already known, so the next knob
somebody adds is exactly the one that rides through. `RUST_LOG` is named
explicitly because it does not carry the prefix.

$null, NEVER "". A set-but-empty variable is not "off": readers that use
`var_os().is_some()` arm on the empty string. `Start-Process -Environment`
omits a $null entry from the child block entirely, which is what "absent"
means; see `Get-ChildEnvironmentSnapshot`.
#>
function Get-BoardEnvironment {
    $environment = @{}
    foreach ($name in [Environment]::GetEnvironmentVariables().Keys) {
        if ([string]$name -like "IZARRAVM_*") { $environment[[string]$name] = $null }
    }
    $environment["RUST_LOG"] = $null
    return $environment
}

<#
Read a CHILD PROCESS's own environment block back.

Asserting about the hashtable `Get-BoardEnvironment` returns would be a test
that cannot fail for the only thing that matters -- whether the variable
actually reaches the emulator. `cmd /c set` is the cheapest faithful observer on
this host: it lists a PRESENT-BUT-EMPTY variable as `NAME=` and omits an ABSENT
one entirely, which is exactly the distinction that matters here.

Copied from scripts/run-fixture-scoreboard.ps1, which paid for the technique.
#>
function Get-ChildEnvironmentSnapshot($Environment) {
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ("izarravm-compat-env-" +
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
            throw "compatibility board self-test: the environment observer never exited"
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

# The scrub self-test. It plants a variable in THIS process, then reads a real
# child's block back, so it proves the mechanism rather than the intent. The
# negative half is the point: an unscrubbed name must be OBSERVED in the child,
# or the positive half is a gate that cannot fail.
function Assert-BoardEnvironmentSelfTest {
    $planted = "IZARRAVM_COMPAT_BOARD_SELFTEST"
    $control = "IZARRAVM_CONTROL_SELFTEST"
    [Environment]::SetEnvironmentVariable($planted, "1")
    [Environment]::SetEnvironmentVariable($control, "1")
    try {
        $environment = Get-BoardEnvironment
        if (-not $environment.ContainsKey($planted)) {
            throw "self-test: the scrub did not notice $planted"
        }
        if ($null -ne $environment[$planted]) {
            throw "self-test: $planted must be cleared with `$null, never an empty string"
        }

        $child = Get-ChildEnvironmentSnapshot $environment
        if ($child.ContainsKey($planted)) {
            throw "self-test: $planted survived into the child block"
        }
        if ($child.ContainsKey("RUST_LOG")) {
            throw "self-test: RUST_LOG survived into the child block"
        }

        # THE NEGATIVE HALF. Hand the same child an environment that does NOT
        # clear the control name, and it must be observed. Without this the
        # assertion above passes just as happily against a `cmd /c set` that
        # printed nothing at all.
        $leaky = @{}
        foreach ($entry in $environment.GetEnumerator()) { $leaky[$entry.Key] = $entry.Value }
        $leaky.Remove($control)
        $leakyChild = Get-ChildEnvironmentSnapshot $leaky
        if (-not $leakyChild.ContainsKey($control)) {
            throw ("self-test: the observer cannot see an UNSCRUBBED variable, so it " +
                "cannot prove a scrubbed one is gone")
        }
    } finally {
        [Environment]::SetEnvironmentVariable($planted, $null)
        [Environment]::SetEnvironmentVariable($control, $null)
    }
}

<#
Grade a published frame's content against a row's pinned bands.

Bands, not a hash, for the reason Duke3D and Tomb Raider lost their hashes: a
picture that animates continuously moves legitimately on any cadence-adjacent
change. A band still fails hard on the one thing a graphics defect does, which
is to stop putting content on the raster.
#>
function Test-ContentBands {
    param(
        [Parameter(Mandatory)][int]$Colors,
        [Parameter(Mandatory)][double]$NonBlackPercent,
        [Parameter(Mandatory)][hashtable]$Pin
    )
    $reasons = @()
    if ($Colors -lt $Pin.colors_min -or $Colors -gt $Pin.colors_max) {
        $reasons += ("distinct colours is $Colors, outside the band " +
            "[$($Pin.colors_min), $($Pin.colors_max)]")
    }
    if ($NonBlackPercent -lt $Pin.nonblack_min -or $NonBlackPercent -gt $Pin.nonblack_max) {
        $reasons += ("non-black coverage % is $NonBlackPercent, outside the band " +
            "[$($Pin.nonblack_min), $($Pin.nonblack_max)]")
    }
    [pscustomobject]@{ Pass = ($reasons.Count -eq 0); Reasons = $reasons }
}

<#
Does the census hold the mode this row exists to exercise?

THE ACCEPTANCE RULE. A row that grades a menu grades nothing, and every game on
this board shows a menu before it shows the mode it was chosen for. Jazz
Jackrabbit measured 320x400 in the 2026-08-29 sweep, which was its menu.

TargetNote is the extra condition, spelled exactly as one of:
  ""                     any geometry of that mode
  "hdisp_end <n>"        that horizontal display end
  "source_lines <n>"     that SOURCE row count, double scan already divided out
  "line_compare_active"  a split screen
#>
function Test-TargetMode {
    param(
        [Parameter(Mandatory)]$Census,
        [Parameter(Mandatory)][string]$TargetMode,
        [string]$TargetNote = ""
    )
    if ($TargetMode -eq "Distira") {
        return (@($Census.distira).Count -gt 0)
    }
    # The note is validated BEFORE the loop, so a typo in the row table throws
    # even when the census is empty. Validating it inside the loop would let a
    # misspelled note read as "mode not reached" and fail the row for the wrong
    # reason, which is the harder bug to find.
    $field = $null
    $want = 0
    if ($TargetNote -ne "" -and $TargetNote -ne "line_compare_active") {
        $parts = $TargetNote -split "\s+"
        if ($parts.Count -ne 2 -or $parts[0] -notin @("hdisp_end", "source_lines")) {
            throw "Unreadable targetNote '$TargetNote'"
        }
        $field = $parts[0]
        $want = [int]$parts[1]
    }

    foreach ($row in @($Census.vga)) {
        if ($row.mode -ne $TargetMode) { continue }
        if ($TargetNote -eq "") { return $true }
        if ($TargetNote -eq "line_compare_active") {
            if ($row.line_compare_active) { return $true }
            continue
        }
        if ($field -eq "hdisp_end" -and $row.hdisp_end -eq $want) { return $true }
        # source_lines, never vdisp_end. MEASURED 2026-08-29: vdisp_end counts
        # RASTER lines, so standard mode 13h reads 400 there with double_scan
        # set, not 200. The emulator divides the scan factor out and reports the
        # source row count, which is what a game's own resolution means.
        if ($field -eq "source_lines" -and $row.source_lines -eq $want) { return $true }
    }
    return $false
}

<#
Grade one row's result against its pin.

Four checks, and every one of them has to pass:
  1. a published frame exists at all
  2. its content is inside the colour and non-black bands
  3. the census holds the row's target mode (THE ACCEPTANCE RULE)
  4. the instruction count is within tolerance

Check 4 is what catches a run that never launched the game. A run parked at the
DOS prompt retires a very different number from one that reached gameplay, and
it misses by far more than the tolerance.
#>
function Test-BoardRow {
    param([Parameter(Mandatory)]$Result, [Parameter(Mandatory)]$Row, $Pin)
    if ($null -eq $Pin) {
        # Never pass by default. A board that grades a row it has no pin for is
        # a gate that cannot fail.
        return [pscustomobject]@{ Pass = $false; Reasons = @("no pin recorded for this row") }
    }
    if ($null -eq $Result.stats) {
        return [pscustomobject]@{ Pass = $false; Reasons = @("no published frame") }
    }

    $bands = Test-ContentBands -Colors $Result.stats.distinct_colors `
        -NonBlackPercent $Result.stats.non_black_pct -Pin $Pin
    $reasons = @($bands.Reasons)

    if (-not (Test-TargetMode -Census $Result.census -TargetMode $Row.targetMode `
                -TargetNote $Row.targetNote)) {
        $note = if ($Row.targetNote -eq "") { "" } else { " ($($Row.targetNote))" }
        $reasons += "the census never shows $($Row.targetMode)$note"
    }

    if (-not $Pin.ContainsKey("instructions")) {
        $reasons += "the pin carries no instruction count"
    }
    else {
        $want = [double]$Pin.instructions
        $tolerance = 0.05
        $low = $want * (1.0 - $tolerance)
        $high = $want * (1.0 + $tolerance)
        if ($Result.instructions -lt $low -or $Result.instructions -gt $high) {
            $reasons += ("instructions $($Result.instructions) is outside " +
                "$([math]::Round($low)) to $([math]::Round($high))")
        }
    }

    [pscustomobject]@{ Pass = ($reasons.Count -eq 0); Reasons = $reasons }
}

<#
Build a row's pin from a measured result.

Bands are set from the measurement with HEADROOM, the way psycho-486's [60, 95]
sits around a measured 82.9%. The asymmetry is deliberate: a graphics defect
drives coverage DOWN (a wiped raster reads 0.0%) far more often than up, so the
lower skirt is wider than the upper one. Colours only get a lower bound worth
having; the upper bound is the palette size, because "more colours than measured"
is not a defect shape.
#>
function New-BoardPin($Stats, [uint64]$Instructions, $Census) {
    [ordered]@{
        colors_min   = [math]::Max(1, [int]($Stats.distinct_colors * 0.5))
        colors_max   = 256
        nonblack_min = [math]::Round([math]::Max(0.0, $Stats.non_black_pct - 20.0), 1)
        nonblack_max = [math]::Round([math]::Min(100.0, $Stats.non_black_pct + 12.0), 1)
        instructions = $Instructions
        census       = $Census
    }
}

<#
Run one row and grade it.

Copy the tree FRESH every run. Several fixtures mutate their own tree, so a
reused copy grades the previous run's leftovers.
#>
function Invoke-BoardRow {
    param(
        [Parameter(Mandatory)]$Row,
        [Parameter(Mandatory)][string]$WorkRoot,
        [Parameter(Mandatory)][string]$OutDir,
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string]$FixtureRoot
    )
    if ($null -eq $Row.cycles) {
        throw ("Row '$($Row.name)' has no budget yet. It has not been brought up: see " +
            "Phase 4 of dev_docs/plans/2026-08-29-compatibility-board.md.")
    }

    $source = Join-Path $FixtureRoot $Row.folder
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Row '$($Row.name)' has no fixture tree at $source"
    }
    $work = Join-Path $WorkRoot "work-$($Row.name)"
    if (Test-Path -LiteralPath $work) {
        throw "A board work path was reused: $work"
    }
    Copy-Item -LiteralPath $source -Destination $work -Recurse

    $ppm = Join-Path $OutDir "$($Row.name).ppm"
    $censusPath = Join-Path $OutDir "$($Row.name)-census.json"
    $profilePath = Join-Path $OutDir "$($Row.name)-profile.json"

    $arguments = @("--hdd-folder", $work) + $Row.arguments + @(
        "--cycles", $Row.cycles.ToString(),
        # The PUBLISHED frame, never a re-render. With the raster-erase defect
        # restored, Psycho Pinball reads 82.9% non-black through --result-ppm and
        # 0.0% through this one on the same run, so --result-ppm would have
        # graded that defect green.
        "--presented-ppm", $ppm,
        "--mode-census", $censusPath,
        "--profile-json", $profilePath
    )
    $keys = Get-RowField -Row $Row -Name "injectKeys"
    if ($null -ne $keys) { $arguments += @("--inject-keys", $keys) }
    $mouse = Get-RowField -Row $Row -Name "injectMouse"
    if ($null -ne $mouse) { $arguments += @("--inject-mouse", $mouse) }
    $cd = Get-RowField -Row $Row -Name "cdImage"
    if ($null -ne $cd) { $arguments += @("--cd-image", (Join-Path $FixtureRoot $cd)) }

    # Start-Process, not `& $Executable`, because only the former takes an
    # explicit environment block. See Get-BoardEnvironment for why running under
    # the parent's environment would silently poison every pin.
    $log = Join-Path $OutDir "$($Row.name)-run.log"
    $start = @{
        FilePath               = $Executable
        ArgumentList           = $arguments
        NoNewWindow            = $true
        PassThru               = $true
        RedirectStandardOutput = $log
        Environment            = Get-BoardEnvironment
    }
    $process = Start-Process @start
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "Row '$($Row.name)' exited $($process.ExitCode); see $log"
    }

    $stats = Get-PpmFrameStats $ppm
    if ($null -eq $stats) {
        # No published frame at all. That is a RESULT, not an error: it is what a
        # guest whose raster never completed looks like, and it has to reach the
        # table as a failure rather than as a crash.
        return [pscustomobject]@{
            name = $Row.name; stats = $null; census = $null; instructions = $null
        }
    }
    $census = Get-Content -LiteralPath $censusPath -Raw | ConvertFrom-Json
    $report = Get-Content -LiteralPath $profilePath -Raw | ConvertFrom-Json

    [pscustomobject]@{
        name         = $Row.name
        stats        = $stats
        census       = $census
        instructions = [uint64]$report.perf.instructions
    }
}

function Get-BoardMarkdown($Results, [string]$BoardLabel) {
    $markdown = @("# Compatibility board: $BoardLabel", "")
    $markdown += "| row | modes reached | colours | non-black % | instructions | invariant |"
    $markdown += "|---|---|---|---|---|---|"
    foreach ($result in $Results) {
        $stats = $result.stats
        $colors = if ($null -eq $stats) { "-" } else { $stats.distinct_colors }
        $nonBlack = if ($null -eq $stats) { "-" } else { $stats.non_black_pct }
        $insns = if ($null -eq $result.instructions) { "-" } else { $result.instructions }
        $verdict = if ($result.verdict.Pass) { "PASS" }
        else { "FAIL: " + ($result.verdict.Reasons -join "; ") }
        $markdown += ("| $($result.name) | $($result.reached) | $colors | $nonBlack | " +
            "$insns | $verdict |")
    }
    $markdown += ""
    $markdown += ("NO FIGURE IN THIS TABLE IS A PERFORMANCE MEASUREMENT. This board runs " +
        "under whatever load the box has and reports no wall time on purpose.")
    return $markdown -join "`n"
}

if ($SelfTest) {
    $table = Get-RowTable
    $known = @($table | ForEach-Object { $_.name })

    Assert-BoardEqual $known.Count 8 "row count"

    $duplicates = $known | Group-Object | Where-Object { $_.Count -gt 1 }
    if ($duplicates) { throw "self-test: duplicate row name $($duplicates[0].Name)" }

    $folders = $table | ForEach-Object { $_.folder } | Group-Object | Where-Object { $_.Count -gt 1 }
    if ($folders) { throw "self-test: two rows share the folder $($folders[0].Name)" }

    Assert-BoardEqual (Resolve-RowSelection -Selection $null -KnownNames $known).Count 8 `
        "an empty selection means every row"
    Assert-BoardEqual (Resolve-RowSelection -Selection @("jazz-486,keen4-486") -KnownNames $known).Count 2 `
        "the comma string splits"
    try {
        Resolve-RowSelection -Selection @("nosuchrow") -KnownNames $known | Out-Null
        throw "self-test: an unknown row must throw"
    } catch { if ($_.Exception.Message -notlike "Unknown row*") { throw } }
    try {
        Resolve-RowSelection -Selection @("jazz-486,jazz-486") -KnownNames $known | Out-Null
        throw "self-test: a repeated row must throw"
    } catch { if ($_.Exception.Message -notlike "*more than once*") { throw } }

    if ($null -ne (Get-RowField -Row $table[0] -Name "cdImage")) {
        throw "self-test: pinball-fantasies-486 has no CD and must probe as null"
    }
    if ($null -eq (Get-RowField -Row $table[6] -Name "cdImage")) {
        throw "self-test: tombraid3d-586 must carry a cdImage"
    }

    # ---- Grading, proved on synthetic values so no board run is needed. ----
    #
    # Reasons are JOINED before every -match. PowerShell's -match and -notmatch
    # FILTER an array rather than returning a boolean, so `$reasons -notmatch "x"`
    # yields every reason that lacks "x" and is truthy whenever a second reason
    # exists. Caught here on the first run of this block.

    $band = @{ colors_min = 40; colors_max = 256; nonblack_min = 60.0; nonblack_max = 95.0 }

    # The measured psycho-486 frame, which must pass.
    if (-not (Test-ContentBands -Colors 127 -NonBlackPercent 82.9 -Pin $band).Pass) {
        throw "self-test: a good frame must pass its bands"
    }

    # THE DEFECT THE BOARD EXISTS FOR. With the raster wipe restored, the same
    # Psycho Pinball run reads 0.0% and 1 colour through the PUBLISHED frame.
    $wiped = Test-ContentBands -Colors 1 -NonBlackPercent 0.0 -Pin $band
    if ($wiped.Pass) { throw "self-test: a wiped frame must FAIL its bands" }
    if (($wiped.Reasons -join "; ") -notmatch "colour") {
        throw "self-test: the failure must name the colour count"
    }
    if (($wiped.Reasons -join "; ") -notmatch "non-black") {
        throw "self-test: the failure must name the coverage"
    }

    # A solid fill and a two-colour wipeout are the other shapes a band rejects.
    if ((Test-ContentBands -Colors 1 -NonBlackPercent 100.0 -Pin $band).Pass) {
        throw "self-test: a solid fill must FAIL"
    }
    if ((Test-ContentBands -Colors 2 -NonBlackPercent 100.0 -Pin $band).Pass) {
        throw "self-test: a two-colour wipeout must FAIL"
    }

    # ---- The acceptance rule. ----

    $census = @{
        vga = @(
            @{ mode = "Mode13h"; hdisp_end = 320; vdisp_end = 400; source_lines = 200
                double_scan = $true; line_compare_active = $false; entries = 3 }
        )
        distira = @()
    }
    if (Test-TargetMode -Census $census -TargetMode "Mode13h" -TargetNote "source_lines 199") {
        throw "self-test: 200 source rows must NOT satisfy a 199-line target"
    }
    # MEASURED on Psycho Pinball 2026-08-29: vdisp_end counts RASTER lines, so
    # standard mode 13h reports 400 there with double_scan set. source_lines is
    # the derived source row count and is the only field this rule can read.
    $census.vga[0].vdisp_end = 199
    $census.vga[0].source_lines = 199
    $census.vga[0].double_scan = $false
    if (-not (Test-TargetMode -Census $census -TargetMode "Mode13h" -TargetNote "source_lines 199")) {
        throw "self-test: 199 source rows must satisfy the target"
    }
    if (Test-TargetMode -Census $census -TargetMode "ModeX" -TargetNote "") {
        throw "self-test: a mode the guest never entered must NOT satisfy a target"
    }
    if (Test-TargetMode -Census $census -TargetMode "Distira" -TargetNote "") {
        throw "self-test: an empty distira list must NOT satisfy a Distira target"
    }
    $census.distira = @(@{ width = 640; height = 480; entries = 2 })
    if (-not (Test-TargetMode -Census $census -TargetMode "Distira" -TargetNote "")) {
        throw "self-test: a distira entry must satisfy a Distira target"
    }

    # hdisp_end, the Pinball Fantasies rule.
    $wide = @{ vga = @(
            @{ mode = "ModeX"; hdisp_end = 320; vdisp_end = 480; source_lines = 480
                double_scan = $false; line_compare_active = $false; entries = 9 }
        ); distira = @() }
    if (Test-TargetMode -Census $wide -TargetMode "ModeX" -TargetNote "hdisp_end 256") {
        throw "self-test: a 320-wide mode X must NOT satisfy a 256-wide target"
    }
    $wide.vga[0].hdisp_end = 256
    if (-not (Test-TargetMode -Census $wide -TargetMode "ModeX" -TargetNote "hdisp_end 256")) {
        throw "self-test: a 256-wide mode X must satisfy the target"
    }

    # line_compare_active, the Commander Keen 4 rule.
    $panel = @{ vga = @(
            @{ mode = "Planar"; hdisp_end = 320; vdisp_end = 400; source_lines = 400
                double_scan = $false; line_compare_active = $false; entries = 5 }
        ); distira = @() }
    if (Test-TargetMode -Census $panel -TargetMode "Planar" -TargetNote "line_compare_active") {
        throw "self-test: a full-screen planar mode must NOT satisfy a split-screen target"
    }
    $panel.vga[0].line_compare_active = $true
    if (-not (Test-TargetMode -Census $panel -TargetMode "Planar" -TargetNote "line_compare_active")) {
        throw "self-test: a split-screen planar mode must satisfy the target"
    }

    # An unreadable note is a typo in the row table, and it must throw rather
    # than silently answering false and failing the row for the wrong reason.
    try {
        Test-TargetMode -Census $panel -TargetMode "Planar" -TargetNote "nonsense" | Out-Null
        throw "self-test: an unreadable targetNote must throw"
    } catch { if ($_.Exception.Message -notlike "Unreadable targetNote*") { throw } }

    # ---- The whole verdict, including the two checks bands cannot make. ----

    $goodRow = [pscustomobject]@{ targetMode = "Mode13h"; targetNote = "source_lines 199" }
    $goodPin = @{
        colors_min = 40; colors_max = 256; nonblack_min = 60.0; nonblack_max = 95.0
        instructions = 1000000
    }
    $goodResult = [pscustomobject]@{
        stats = [pscustomobject]@{ distinct_colors = 127; non_black_pct = 82.9 }
        census = $census; instructions = [uint64]1000000
    }
    if (-not (Test-BoardRow -Result $goodResult -Row $goodRow -Pin $goodPin).Pass) {
        throw "self-test: a good row must pass"
    }

    # A run that never launched the game. It retires a very different count, and
    # this is the check that catches it when the picture happens to look fine.
    $wrongInsns = [pscustomobject]@{
        stats = $goodResult.stats; census = $census; instructions = [uint64]400000
    }
    $verdict = Test-BoardRow -Result $wrongInsns -Row $goodRow -Pin $goodPin
    if ($verdict.Pass) { throw "self-test: a 60% instruction miss must FAIL" }
    if (($verdict.Reasons -join "; ") -notmatch "instructions") {
        throw "self-test: the failure must name the instruction count"
    }
    # 4% is inside the 5% tolerance and must pass, or the check is a hair
    # trigger that fires on ordinary run-to-run drift.
    $nearMiss = [pscustomobject]@{
        stats = $goodResult.stats; census = $census; instructions = [uint64]1040000
    }
    if (-not (Test-BoardRow -Result $nearMiss -Row $goodRow -Pin $goodPin).Pass) {
        throw "self-test: a 4% instruction drift must PASS"
    }

    # No published frame at all: a result, not a crash, and it must FAIL.
    $noFrame = [pscustomobject]@{ stats = $null; census = $null; instructions = $null }
    $verdict = Test-BoardRow -Result $noFrame -Row $goodRow -Pin $goodPin
    if ($verdict.Pass) { throw "self-test: a row with no published frame must FAIL" }
    if (($verdict.Reasons -join "; ") -notmatch "no published frame") {
        throw "self-test: the failure must say there was no frame"
    }

    # An unpinned row must FAIL, never pass by default. A board that grades a
    # row it has no pin for is a gate that cannot fail.
    if ((Test-BoardRow -Result $goodResult -Row $goodRow -Pin $null).Pass) {
        throw "self-test: a row with no pin must FAIL"
    }

    # A picture inside its bands whose census never reached the target mode. This
    # is the whole reason the board carries a census: the frame looks fine and
    # the row proves nothing.
    $menuOnly = [pscustomobject]@{
        stats = $goodResult.stats
        census = @{ vga = @(
                @{ mode = "Mode13h"; hdisp_end = 320; vdisp_end = 400; source_lines = 200
                    double_scan = $true; line_compare_active = $false; entries = 3 }
            ); distira = @() }
        instructions = [uint64]1000000
    }
    $verdict = Test-BoardRow -Result $menuOnly -Row $goodRow -Pin $goodPin
    if ($verdict.Pass) { throw "self-test: a row that never reached its mode must FAIL" }
    if (($verdict.Reasons -join "; ") -notmatch "census") {
        throw "self-test: the failure must say the census never showed the mode"
    }

    # ---- The pin shape, which has never run until the first row is pinned. ----

    $measured = [pscustomobject]@{ distinct_colors = 127; non_black_pct = 82.9 }
    $pin = New-BoardPin $measured ([uint64]3734683259) $census
    # The psycho-486 row's own recorded band is [60, 95] around 82.9%, so a pin
    # built from the same measurement must contain that measurement comfortably.
    if (-not (Test-ContentBands -Colors 127 -NonBlackPercent 82.9 -Pin $pin).Pass) {
        throw "self-test: a pin must accept the measurement it was built from"
    }
    # And must still reject the shapes the bands exist for.
    if ((Test-ContentBands -Colors 1 -NonBlackPercent 0.0 -Pin $pin).Pass) {
        throw "self-test: a pin must still reject a wiped frame"
    }
    if ((Test-ContentBands -Colors 1 -NonBlackPercent 100.0 -Pin $pin).Pass) {
        throw "self-test: a pin must still reject a solid fill"
    }
    if ($pin.instructions -ne 3734683259) { throw "self-test: the pin lost its instruction count" }
    if ($null -eq $pin.census) { throw "self-test: the pin lost its census" }

    # A nearly-black frame must not produce a band whose floor is below zero.
    $dark = New-BoardPin ([pscustomobject]@{ distinct_colors = 2; non_black_pct = 3.0 }) `
        ([uint64]1) $census
    if ($dark.nonblack_min -lt 0.0) { throw "self-test: a band floor went negative" }
    if ($dark.colors_min -lt 1) { throw "self-test: a colour floor went below one" }
    # A full-coverage frame must not produce a ceiling above 100%.
    $full = New-BoardPin ([pscustomobject]@{ distinct_colors = 200; non_black_pct = 99.6 }) `
        ([uint64]1) $census
    if ($full.nonblack_max -gt 100.0) { throw "self-test: a band ceiling exceeded 100%" }

    # ---- The report, likewise never run until a board runs. ----

    $reportRows = @(
        [pscustomobject]@{ name = "row-pass"; reached = "ModeX"; instructions = [uint64]10
            stats = $measured; verdict = [pscustomobject]@{ Pass = $true; Reasons = @() } }
        [pscustomobject]@{ name = "row-fail"; reached = "-"; instructions = $null; stats = $null
            verdict = [pscustomobject]@{ Pass = $false; Reasons = @("no published frame") } }
    )
    $markdown = Get-BoardMarkdown -Results $reportRows -BoardLabel "self-test"
    foreach ($needle in @("row-pass", "row-fail", "PASS", "no published frame",
            "NO FIGURE IN THIS TABLE IS A PERFORMANCE MEASUREMENT")) {
        if ($markdown -notlike "*$needle*") {
            throw "self-test: the report omits '$needle'"
        }
    }
    # A row with no frame must render its cells as placeholders rather than
    # crashing on a null, which is the shape a StrictMode board dies on.
    if ($markdown -notlike "*| row-fail | - | - | - | - |*") {
        throw "self-test: a frameless row must render placeholder cells"
    }

    Assert-BoardFrameStatsSelfTest
    Assert-BoardEnvironmentSelfTest

    Write-Host "compatibility board self-test passed"
    return
}

# ---------------------------------------------------------------------------
# The run.
# ---------------------------------------------------------------------------
$table = Get-RowTable
$selected = Resolve-RowSelection -Selection $Rows -KnownNames @($table | ForEach-Object { $_.name })
$pinPath = Join-Path $PSScriptRoot "compatibility-board-invariants.json"
$pins = Get-Content -LiteralPath $pinPath -Raw | ConvertFrom-Json -AsHashtable

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$outDir = Join-Path $OutRoot "$Label-$stamp"
$workRoot = Join-Path $outDir "work"
New-Item -ItemType Directory -Path $workRoot -Force | Out-Null

$results = @()
foreach ($name in $selected) {
    $row = $table | Where-Object { $_.name -eq $name }
    Write-Host "running $name"
    $result = Invoke-BoardRow -Row $row -WorkRoot $workRoot -OutDir $outDir `
        -Executable $Executable -FixtureRoot $FixtureRoot
    # What the census actually reached, for the table. Reported whether the row
    # passed or failed, because "it reached Mode13h when ModeX was wanted" is the
    # single most useful line in a failure.
    $reached = if ($null -eq $result.census) { "-" }
    elseif (@($result.census.distira).Count -gt 0) { "Distira" }
    else { (@($result.census.vga) | ForEach-Object { $_.mode } | Sort-Object -Unique) -join "," }
    $result | Add-Member -NotePropertyName reached -NotePropertyValue $reached
    $pin = if ($pins.ContainsKey($name)) { $pins[$name] } else { $null }
    $result | Add-Member -NotePropertyName verdict `
        -NotePropertyValue (Test-BoardRow -Result $result -Row $row -Pin $pin)
    $results += $result
}

if ($RecordInvariants) {
    foreach ($result in $results) {
        if ($null -eq $result.stats) {
            throw "Refusing to record a pin for '$($result.name)': it published no frame."
        }
        $row = $table | Where-Object { $_.name -eq $result.name }
        if (-not (Test-TargetMode -Census $result.census -TargetMode $row.targetMode `
                    -TargetNote $row.targetNote)) {
            # THE ACCEPTANCE RULE, enforced at the one place it could be
            # bypassed. A pin recorded from a run that never reached the mode
            # would make the row permanently green and permanently useless.
            throw ("Refusing to record a pin for '$($result.name)': its census never shows " +
                "$($row.targetMode). Debug the row; do not pin it.")
        }
        $pins[$result.name] = New-BoardPin $result.stats $result.instructions $result.census
    }
    # LF, not CRLF. Set-Content writes CRLF on Windows, but .gitattributes
    # declares `* text=auto eol=lf`, so any checkout sees LF -- and
    # check_file_policy.py hashes the WORKING TREE. A CRLF file here means the
    # manifest carries a sha no CI checkout can reproduce, and the file policy
    # is CI step 1, so it would hide every later gate. Measured: the same pins
    # hashed 2c1a54c0 as CRLF and d93ed2f6 as LF.
    [IO.File]::WriteAllText($pinPath, (($pins | ConvertTo-Json -Depth 8) -replace "`r`n", "`n") + "`n")
    Write-Host "recorded pins for: $($selected -join ', ')"
    Write-Host "NOW MOVE THE SHA in LICENSE_MANIFEST.tsv, in this same commit."
}

$markdown = Get-BoardMarkdown -Results $results -BoardLabel "$Label $stamp"
$markdown | Set-Content -LiteralPath (Join-Path $outDir "board.md")
Write-Host $markdown

if ($results | Where-Object { -not $_.verdict.Pass }) { exit 1 }
