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
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
            # Three mode X geometries in 40 guest seconds, one of them 256 pixels
            # wide, which no other fixture anywhere exercises. `machine=vgaonly`,
            # and the closest sibling to Psycho Pinball.
            targetMode = "ModeX"; targetNote = "hdisp_end 256"
        }
        [pscustomobject]@{
            name = "keen4-486"; folder = "ckeen4_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
            # EGA smooth scroll with a split-screen status panel. The corpus
            # records it as svga_s3, so no census could have found it.
            targetMode = "Planar"; targetNote = "line_compare_active"
        }
        [pscustomobject]@{
            name = "jazz-486"; folder = "jazzjack_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
            # 199 SOURCE rows, not 199 raster lines. vdisp_end counts raster
            # lines: standard mode 13h reads 400 there with double_scan set, so a
            # rule reading vdisp_end could never match. Measured on Psycho
            # Pinball 2026-08-29.
            targetMode = "Mode13h"; targetNote = "source_lines 199"
        }
        [pscustomobject]@{
            name = "koreatetris-486"; folder = "koreatet_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
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
            cycles = $null; injectKeys = $null; injectMouse = $null
            targetMode = "ModeX"; targetNote = ""
        }
        [pscustomobject]@{
            name = "cabal-486"; folder = "cabal_c"
            arguments = @("--cpu", "486", "--memory-mib", "64", "--video", "vega")
            cycles = $null; injectKeys = $null; injectMouse = $null
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

    Assert-BoardFrameStatsSelfTest

    Write-Host "compatibility board self-test passed"
    return
}

throw "The run path is not implemented yet."
