#Requires -Version 7.4
<#
.SYNOPSIS
Builds the SHORT duke3d benchmark fixture (`.bench/duke3d_short_c`) from the
long one (`.bench/duke3d_c`).

.DESCRIPTION
The duke3d-586 board row costs ~470 s of wall a leg, which makes an A/B/B/A set
over half an hour and a six-leg floor the better part of an evening. The row is
guest-driven -- DUKEMARK plays BENCH2 to its end, prints its report and the
batch pokes EXITVM -- so the cycle budget is a guard and CANNOT be used to
shorten the run without destroying every one of the four DUKEMARK invariants.

What CAN be shortened is the demo itself. A Duke3D .DMO carries its record count
in the first little-endian dword of the header:

    offset 0  dword  reccnt        number of recorded sync records
    offset 4  byte   0x74          BYTEVERSION (116, the Atomic value)
    offset 5  byte   volume_number
    offset 6  byte   level_number
    offset 7  byte   player_skill
    ...
    offset 30 char[] recorder name ("DXZEFF" for the three BENCH demos)

Playback counts records down from `reccnt` and ends the demo when it reaches
zero, so LOWERING that dword ends playback early at a record boundary with the
rest of the file untouched. Nothing is truncated, nothing is re-encoded, and the
records that do play are byte-for-byte the ones the long row plays -- the short
row is a PREFIX of the long row's workload, not a different one.

The fixture this writes differs from `.bench/duke3d_c` in exactly two files:

  * `DUKE3D\BENCH2S.DMO` -- BENCH2.DMO with `reccnt` rewritten.
  * `AUTOEXEC.BAT`       -- `DUKEMARK.EXE /bqBENCH2S` instead of `/bqBENCH2`.

Everything else, DUKE3D.CFG included, is copied byte-exact, so the Info String
invariant (`2,320,200,2,0,1,1,1`) is unchanged and sound and music stay on.

.PARAMETER Records
Records to leave in the short demo. Default 1560, calibrated 2026-08-16 at main
`89dc3a69` to land the whole guest run at about 60 guest seconds at the 586
persona. BENCH2 has 3909.

Two measured points fix the line (both at 586, arm on, one-lookup 1/1):
200 records ran 14.707 guest / 36.473 wall seconds, 3909 ran 138.19 / 341.851.
That is 0.033293 guest and 0.082334 wall seconds per record on top of a FIXED
8.05 guest / 20.01 wall second load phase, so the wall does not scale with the
guest budget: 60 guest seconds costs 148 wall seconds, a 2.30x cut rather than
the 3x a proportional reading would predict. 3.00x needs 1141 records and
46 guest seconds. 1560 was chosen because a longer prefix keeps the load phase
down to 13% of the row's guest time (against 17% at 1141 and 6% on the long
row), and representativeness is what the row is for.
#>
[CmdletBinding()]
param(
    [string]$Repository = (Split-Path -Parent $PSScriptRoot),
    [int]$Records = 1560,
    [switch]$Force
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$source = Join-Path $Repository ".bench\duke3d_c"
$target = Join-Path $Repository ".bench\duke3d_short_c"
if (-not (Test-Path -LiteralPath $source)) { throw "Missing source fixture: $source" }
if ((Test-Path -LiteralPath $target) -and -not $Force) {
    throw "$target already exists; pass -Force to rebuild it"
}
if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }

$robo = Start-Process -FilePath robocopy.exe -ArgumentList @(
    $source, $target, "/E", "/COPY:DAT", "/DCOPY:DAT", "/R:1", "/W:1",
    "/NFL", "/NDL", "/NJH", "/NJS", "/NP"
) -NoNewWindow -Wait -PassThru
if ($robo.ExitCode -ge 8) { throw "robocopy failed with exit code $($robo.ExitCode)" }

# A stale DUKEMARK.TXT in the fixture would be graded as a run's own result.
$stale = Join-Path $target "DUKEMARK.TXT"
if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force }

$longDemo = Join-Path $target "DUKE3D\BENCH2.DMO"
$shortDemo = Join-Path $target "DUKE3D\BENCH2S.DMO"
$bytes = [IO.File]::ReadAllBytes($longDemo)
if ($bytes.Length -lt 64) { throw "BENCH2.DMO is too small to be a demo" }
if ($bytes[4] -ne 0x74) {
    throw ("BENCH2.DMO byte 4 is 0x{0:X2}, expected 0x74 (Atomic BYTEVERSION); " +
        "the header layout this script rewrites is not the one on disk" -f $bytes[4])
}
$original = [BitConverter]::ToUInt32($bytes, 0)
if ($original -ne 3909) {
    throw "BENCH2.DMO reccnt is $original, expected 3909; the fixture changed under this script"
}
if ($Records -lt 1 -or $Records -ge $original) {
    throw "Records must be between 1 and $($original - 1); got $Records"
}
[Array]::Copy([BitConverter]::GetBytes([uint32]$Records), 0, $bytes, 0, 4)
[IO.File]::WriteAllBytes($shortDemo, $bytes)

# CRLF, ASCII, no BOM -- byte-for-byte the long fixture's AUTOEXEC with BENCH2
# changed to BENCH2S. Written through WriteAllBytes rather than Set-Content
# because Set-Content -Encoding utf8 emits a BOM the guest shell would execute.
$autoexec = "@echo off`r`ncd \DUKE3D`r`nDUKEMARK.EXE /bqBENCH2S > C:\DUKEMARK.TXT`r`nC:\EXITVM.COM`r`n"
[IO.File]::WriteAllBytes((Join-Path $target "AUTOEXEC.BAT"),
    [Text.Encoding]::ASCII.GetBytes($autoexec))

$sha = (Get-FileHash -LiteralPath $shortDemo -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "duke3d_short_c built at $target"
Write-Host ("  BENCH2S.DMO  reccnt {0} of {1}, sha256 {2}" -f $Records, $original, $sha)
