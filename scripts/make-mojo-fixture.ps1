# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
<#
.SYNOPSIS
Build the `.bench/mojo_c` fixture tree from a local copy of the 3dfx Glide 2.43
SDK diagnostics.

.DESCRIPTION
`mojo-586` is the Distira board-identity row. It runs MOJO.EXE, 3dfx's own
DOS diagnostic for the SST-1, and pins the two text reports it redirects to
disk. Like every fixture tree, `.bench/mojo_c` is git-ignored, so this script
is the recipe that reproduces it; the pins in
`scripts/fixture-scoreboard-invariants.json` are meaningless against a tree
built from different binaries, which is why every file copied here is checked
against a recorded sha256 and a mismatch is fatal.

PROVENANCE. MOJO.EXE ships in the 3dfx Glide 2.43 SDK
(`glide_sdk-243.zip`, sha256 30433239BA7DE96DE8DC9AC35A024FF88419830E81C70D3F400004F0BA05EE3A,
historically at 3dfxarchive.com) under `Glide/Diags/Dos/`. The same binary is
redistributed today as `mojo_dos.zip` by bitsundbolts.com; the two copies are
byte-identical, which is the cross-check this script's expected hashes encode.
3dfx released no licence text with the diagnostics and the redistribution
terms are UNCLEAR, so nothing here is committed to git and nothing here ships
with IzarraVM. See `dev_docs/2026-09-01-mojo-fixture.md`.

.EXAMPLE
pwsh scripts/make-mojo-fixture.ps1
Builds `.bench/mojo_c` from the SDK tree already unpacked at
`.bench/glide243-tests`.

.EXAMPLE
pwsh scripts/make-mojo-fixture.ps1 -DiagsDir D:\downloads\mojo_dos -Force
Builds it from an unpacked `mojo_dos.zip` instead, overwriting an existing tree.
#>
[CmdletBinding()]
param(
    # Directory holding the SDK's DOS diagnostics (mojo.exe + DOS4GW.EXE).
    # Defaults to the unpacked SDK under `.bench`.
    [string]$DiagsDir,
    # Where to write the fixture. Defaults to `.bench/mojo_c`.
    [string]$Destination,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
if ([string]::IsNullOrWhiteSpace($DiagsDir)) {
    $DiagsDir = Join-Path $repositoryRoot ".bench\glide243-tests\Glide\Diags\Dos"
}
if ([string]::IsNullOrWhiteSpace($Destination)) {
    $Destination = Join-Path $repositoryRoot ".bench\mojo_c"
}

# The binaries the row depends on, by content. A tree built from some other
# build of MOJO would produce a different report and read as a Distira
# regression, so this is a hard gate rather than a warning.
$expected = [ordered]@{
    "mojo.exe"   = "d1cd2ce36f4eb333d3136ae8a88af027bf29a5475316b48677dc497b8cdeeeb5"
    "DOS4GW.EXE" = "b8265123ac8a189637448618409ef3ecd2e9f3e1a47062c685a02240f688dec1"
}

if (-not (Test-Path -LiteralPath $DiagsDir -PathType Container)) {
    throw ("Glide SDK diagnostics not found at '$DiagsDir'. Unpack glide_sdk-243.zip " +
        "(or bitsundbolts' mojo_dos.zip) and pass -DiagsDir.")
}

if (Test-Path -LiteralPath $Destination) {
    if (-not $Force) {
        throw "$Destination already exists. Pass -Force to rebuild it."
    }
    Remove-Item -LiteralPath $Destination -Recurse -Force
}

$mojoDir = Join-Path $Destination "MOJO"
New-Item -ItemType Directory -Force -Path $mojoDir | Out-Null

foreach ($entry in $expected.GetEnumerator()) {
    $source = Join-Path $DiagsDir $entry.Key
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Missing $($entry.Key) in '$DiagsDir'."
    }
    $actual = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $entry.Value) {
        throw ("$($entry.Key) sha256 is $actual, expected $($entry.Value). The recorded " +
            "MOJO report pins were measured against the expected binary; a different one " +
            "would fail the row for a reason that has nothing to do with Distira.")
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $mojoDir $entry.Key)
}

# EXITVM.COM pokes the Lotura unit-tester exit register, the same way the
# Duke3D row ends itself. MOJO is a report, not a workload: the run should stop
# when the report is written, not when a cycle budget expires.
$exitVm = Join-Path $repositoryRoot ".bench\duke3d_c\EXITVM.COM"
if (-not (Test-Path -LiteralPath $exitVm -PathType Leaf)) {
    throw "EXITVM.COM not found at '$exitVm'; copy it from any fixture that carries one."
}
Copy-Item -LiteralPath $exitVm -Destination (Join-Path $Destination "EXITVM.COM")

# TOKAEMM is loaded for the same reason every other 32-bit row loads it: MOJO is
# a DOS/4GW binary and needs a DPMI/VCPI host.
$configSys = @(
    "FILES=40"
    "LASTDRIVE=D"
    "DEVICE=C:\DOS\TOKAEMM.SYS RAM /T"
    "DOS=HIGH,UMB"
    "SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT"
)

# MOJO is fully non-interactive -- `usage: mojo [-v]` is its entire command
# line -- so this row needs no key injection at all. Bare MOJO prints the board
# report; `-v` prints the SST-1 register dump INSTEAD of it, so both are run and
# both are pinned.
$autoexec = @(
    "@echo off"
    "PATH C:\DOS;C:\MOJO"
    "cd \MOJO"
    "MOJO.EXE > C:\MOJO.TXT"
    "MOJO.EXE -v > C:\MOJOV.TXT"
    "C:\EXITVM.COM"
)

# CRLF and no trailing blank line: DOS text files, written the way the other
# fixture trees carry them.
[IO.File]::WriteAllText((Join-Path $Destination "CONFIG.SYS"),
    (($configSys -join "`r`n") + "`r`n"))
[IO.File]::WriteAllText((Join-Path $Destination "AUTOEXEC.BAT"),
    (($autoexec -join "`r`n") + "`r`n"))

Write-Host "Built $Destination"
Get-ChildItem -Recurse -File $Destination | ForEach-Object {
    Write-Host ("  {0,-24} {1,8}" -f $_.FullName.Substring($Destination.Length + 1), $_.Length)
}
