# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

<#
.SYNOPSIS
Boot the DOS extenders on the compatibility ladder and check each one reached
its own evidence of success.

.DESCRIPTION
The games are not in the repository, so this is a hand-run acceptance gate
rather than a CI test. Point it at a C: drive holding GAMES\DOOM, GAMES\QUAKE
and GAMES\TSUMERA, plus DOS32A.EXE at the root. It copies that tree to a
scratch directory and never writes to the drive it is given.

DOS/4GW is the case this exists for. It failed with
"DOS/16M error: [23] no memory for VCPI page table" while EMS was a static
partition, because DOS/16M probes for pool sharing by taking every free XMS
kilobyte and re-reading the other interfaces. A count that cannot move reads as
"disjoint pools", so it kept the XMS block and left VCPI empty.

The other three passed throughout and are controls. If a change to the arena
breaks one of them, that is the signal. MEM is checked too, since the report
shape is part of the same change.

.PARAMETER CDrive
The C: drive to copy from. Defaults to the per-user drive the GUI uses.

.PARAMETER Cpu
GSW mode to pin. The default CMOS is 386 at 22 MHz, which silently halves
measurements, so this is always passed explicitly.
#>
[CmdletBinding()]
param(
    [string]$CDrive = (Join-Path $HOME ".izarravm\c_drive"),
    [string]$Exe = "target\release\izarravm.exe",
    [string]$Cpu = "gsw586",
    [string]$Scratch = (Join-Path ([System.IO.Path]::GetTempPath()) "izarravm-extender-gate")
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    throw "no emulator at $Exe. Build it first: cargo build --release -j8 -p izarravm"
}
if (-not (Test-Path $CDrive)) { throw "no C: drive at $CDrive" }

# A cargo test run does not rebuild the binary, so a stale exe here would test
# the wrong driver. Warn if it predates the disk image it will boot.
$img = "crates\izarravm-firmware\roms\tokados-hdd.img"
if ((Test-Path $img) -and ((Get-Item $img).LastWriteTime -gt (Get-Item $Exe).LastWriteTime)) {
    Write-Warning "$Exe is older than $img. Rebuild before trusting this run."
}

# QUAKE\cd is the disc rip, hundreds of megabytes this gate never reads.
if (Test-Path $Scratch) { Remove-Item -Recurse -Force $Scratch }
New-Item -ItemType Directory -Force $Scratch | Out-Null
robocopy $CDrive $Scratch /E /XD (Join-Path $CDrive "GAMES\QUAKE\cd") /NFL /NDL /NJH /NJS /R:1 /W:1 | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy failed with exit code $LASTEXITCODE" }

$configSys = @(
    "FILES=40",
    "LASTDRIVE=D",
    "DEVICE=C:\DOS\TOKAEMM.SYS RAM",
    "DOS=HIGH,UMB",
    "SHELL=C:\DOS\COMMAND.COM C:\DOS /E:2048 /P=C:\AUTOEXEC.BAT"
)

# Expect is a regex matched against the guest's text page. Colours is used
# instead for cases that end in a graphics mode, where --dump-result prints no
# text page at all: the framebuffer histogram is the only evidence available.
$cases = @(
    @{ Name = "DOOM under DOS/4GW";  Cycles = 20000000000
       Auto = @("CD GAMES\DOOM", "DOOM -timedemo demo3")
       Expect = "timed \d+ gametics in \d+ realtics" }
    @{ Name = "DOOM under DOS/32A";  Cycles = 20000000000
       Auto = @("CD GAMES\DOOM", "C:\DOS32A.EXE DOOM.EXE -timedemo demo3")
       Expect = "timed \d+ gametics in \d+ realtics" }
    @{ Name = "Quake under CWSDPMI"; Cycles = 60000000000
       Auto = @("CD GAMES\QUAKE", "QUAKE -winmem 16")
       MinColours = 16 }
    @{ Name = "TSUMERA under 32RTM"; Cycles = 15000000000
       Auto = @("CD GAMES\TSUMERA", "TSUMERA")
       MinColours = 16 }
    @{ Name = "MEM shared-pool report"; Cycles = 6000000000
       Auto = @("MEM")
       Expect = "Extended \(XMS\)\*" }
    @{ Name = "MEM omits the EMS row"; Cycles = 6000000000
       Auto = @("MEM")
       Reject = "Expanded \(EMS\)" }
)

$failures = @()
foreach ($case in $cases) {
    $prelude = @("@ECHO OFF", "PROMPT `$P`$G", "PATH C:\DOS",
                 "SET BLASTER=A220 I7 D1 H5 P300 T6")
    $lines = $prelude + $case.Auto
    # CRLF, and a terminating newline: DOS drops a final line without one.
    [IO.File]::WriteAllText((Join-Path $Scratch "CONFIG.SYS"),
                            ($configSys -join "`r`n") + "`r`n", [Text.Encoding]::ASCII)
    [IO.File]::WriteAllText((Join-Path $Scratch "AUTOEXEC.BAT"),
                            ($lines -join "`r`n") + "`r`n", [Text.Encoding]::ASCII)

    $ppm = Join-Path $Scratch "result.ppm"
    $out = & $Exe --hdd-folder $Scratch --cpu $Cpu --cycles $case.Cycles `
                  --dump-result --result-ppm $ppm 2>&1 | Out-String

    $ok = $true
    $detail = ""

    if ($out -match 'CpuError\("([^"]+)"\)') {
        $ok = $false
        $detail = "stopped on CpuError: $($Matches[1])"
    }
    elseif ($null -ne $case.Expect) {
        if ($out -match $case.Expect) {
            if ($out -match "timed (\d+) gametics in (\d+) realtics") {
                $detail = "$($Matches[1]) gametics in $($Matches[2]) realtics"
            }
        } else {
            $ok = $false
            $detail = "no match for /$($case.Expect)/"
        }
    }
    elseif ($null -ne $case.Reject) {
        if ($out -match $case.Reject) {
            $ok = $false
            $detail = "found /$($case.Reject)/, which should be gone"
        }
    }
    elseif ($null -ne $case.MinColours) {
        if ($out -match "distinct colors: (\d+)") {
            $n = [int]$Matches[1]
            $detail = "$n distinct colours, frame at $ppm"
            if ($n -lt $case.MinColours) {
                $ok = $false
                $detail = "only $n distinct colours, wanted at least $($case.MinColours). Frame at $ppm"
            }
        } else {
            $ok = $false
            $detail = "no framebuffer histogram in the output"
        }
    }

    # DOS/16M reports its own failures on the text page rather than to the
    # host, so a run can look clean and still have died inside the extender.
    if ($ok -and ($out -match "DOS/16M error: \[(\d+)\]\s*(.*)")) {
        $ok = $false
        $detail = "DOS/16M error [$($Matches[1])] $($Matches[2].Trim())"
    }

    if ($ok) {
        Write-Host ("PASS  {0}" -f $case.Name) -ForegroundColor Green
        if ($detail) { Write-Host "      $detail" }
    } else {
        Write-Host ("FAIL  {0}" -f $case.Name) -ForegroundColor Red
        Write-Host "      $detail"
        $failures += $case.Name
        $out -split "`n" | Select-Object -Last 25 | ForEach-Object { Write-Host "      $_" }
    }
}

if ($failures.Count -gt 0) {
    throw ("extender gate failed: {0}" -f ($failures -join ", "))
}
Write-Host "extender gate: all cases passed" -ForegroundColor Green
