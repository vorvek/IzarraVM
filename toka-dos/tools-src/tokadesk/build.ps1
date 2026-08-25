# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
#
# Authoring-only: build TOKADESK.EXE with Open Watcom + NASM.
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
if (-not $env:WATCOM) { $env:WATCOM = 'D:\DevTools\OpenWatcom' }
$env:PATH = "$env:WATCOM\binnt;$env:WATCOM\binw;$env:PATH"
$env:INCLUDE = "$env:WATCOM\h"

$wcc = Join-Path $env:WATCOM 'binnt\wcc386.exe'
$wlink = Join-Path $env:WATCOM 'binnt\wlink.exe'

Push-Location $here
try {
    & nasm -f bin stub.asm -o stub.bin
    if ($LASTEXITCODE) { throw "nasm stub.asm failed" }
    & nasm -f obj crt0.asm -o crt0.obj
    if ($LASTEXITCODE) { throw "nasm crt0.asm failed" }

    $cflags = @('-bt=dos','-s','-oilrt','-zp4','-wx','-we','-zl','-zdp','-3s','-zq')
    foreach ($src in @('margo.c','lotura.c','smoke.c')) {
        & $wcc @cflags $src
        if ($LASTEXITCODE) { throw "wcc386 $src failed" }
    }

    $link = @(
        'format','raw','bin','name','payload.bin',
        'option','offset=0x200000',
        'option','nod',
        'option','map=payload.map',
        'file','crt0.obj',
        'file','margo.obj',
        'file','lotura.obj',
        'file','smoke.obj'
    )
    & $wlink @link
    if ($LASTEXITCODE) { throw "wlink payload.bin failed" }
    if (-not (Test-Path 'payload.bin')) { throw "payload.bin missing" }

    python pack.py stub.bin payload.bin tokadesk.exe
    if ($LASTEXITCODE) { throw "pack.py failed" }
    if (-not (Test-Path 'tokadesk.exe')) { throw "tokadesk.exe missing" }
    Write-Host "TOKADESK.EXE: $((Get-Item tokadesk.exe).Length) bytes"
} finally {
    Pop-Location
}
