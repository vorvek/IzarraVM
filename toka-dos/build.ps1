# build.ps1 - build Toka-DOS: compile the C tools with Open Watcom, assemble the
# boot record with NASM, and pack everything into tokados.rom.
#
# Authoring only. CI never runs this; the built binaries and tokados.rom are
# checked in and embedded by izarravm-firmware (the same pattern as the BIOS
# .bin). Run it from anywhere; paths are resolved against this script.
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

$env:WATCOM  = 'D:\DevTools\OpenWatcom'
$env:PATH    = "$env:WATCOM\binnt;$env:PATH"
$env:INCLUDE = "$env:WATCOM\h"

$build = Join-Path $root 'build'
# Wipe the build dir first so a renamed or removed tool cannot linger in it and
# get packed into the ROM (a stale build/emm386.com would otherwise ship next to
# the renamed iemm.com, breaking reproducibility from a clean checkout).
if (Test-Path $build) { Remove-Item -Recurse -Force $build }
New-Item -ItemType Directory -Force $build | Out-Null

function Compile($src, $objName) {
    # Emit the .obj into the build dir so the link step uses bare, space-free
    # names (wlink's `file a,b` directive does not tolerate quoted paths well).
    # -s drops stack-overflow checks: the .COM model does not set up the
    # stack-limit symbol __STK relies on, so the check fires falsely on larger
    # stack frames.
    & wcc -ms -0 -s -q -I"$root\runtime" -fo="$build\$objName" $src
    if ($LASTEXITCODE -ne 0) { throw "wcc failed on $src" }
}

function LinkCom($name, $objNames) {
    Push-Location $build
    try {
        & wlink system com name "$name.com" file ($objNames -join ',')
        if ($LASTEXITCODE -ne 0) { throw "wlink failed on $name" }
    } finally {
        Pop-Location
    }
}

# Shared runtime, compiled once and linked into every tool.
Compile "$root\runtime\toka.c" 'toka.obj'

# The shell.
Compile "$root\icommand\icommand.c" 'icommand.obj'
LinkCom 'icommand' @('icommand.obj', 'toka.obj')

# External tools: one .c per tool under tools/, each linked with the runtime.
Get-ChildItem "$root\tools\*.c" -ErrorAction SilentlyContinue | ForEach-Object {
    $name = $_.BaseName
    Compile $_.FullName "$name.obj"
    LinkCom $name @("$name.obj", 'toka.obj')
}

# Dev tools: built with the same recipe but emitted to the C: drive fixture dir,
# not packed into tokados.rom. TESTS.COM is a tracked debug tool, not a system
# file. Compile and link inside build/ (where wlink writes <name>.com), then move
# the result to c_drive/ so the pack step does not include it.
$cdrive = Join-Path $root '..\c_drive'
New-Item -ItemType Directory -Force $cdrive | Out-Null
Get-ChildItem "$root\devtools\*.c" -ErrorAction SilentlyContinue | ForEach-Object {
    $name = $_.BaseName
    Compile $_.FullName "$name.obj"
    LinkCom $name @("$name.obj", 'toka.obj')
    $built = Join-Path $build "$name.com"
    Move-Item -Force $built (Join-Path $cdrive "$($name.ToUpper()).COM")
}

# The boot record.
& nasm -f bin "$root\boot\tokaboot.asm" -o "$build\tokaboot.bin"
if ($LASTEXITCODE -ne 0) { throw "nasm failed on tokaboot" }

# Pack the build directory straight into the ROM blob the firmware embeds. That
# committed blob is the single source of truth; build/ is intermediate only.
$rom = Join-Path $root '..\crates\izarravm-firmware\roms\tokados.rom'
& cargo run -q --manifest-path "$root\pack\Cargo.toml" -- "$build" "$rom"
if ($LASTEXITCODE -ne 0) { throw "pack failed" }

# wcc leaves <name>.err warning logs in the working directory (its CWD, not the
# build dir); drop them so an authoring run leaves the tree clean.
Get-ChildItem "$root\..\*.err" -ErrorAction SilentlyContinue | Remove-Item -Force

Write-Host "Toka-DOS build complete: $rom"
