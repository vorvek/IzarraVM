# Authoring-only: build Toka-DOS (FreeDOS) from vendored source. Not run in CI.
#
# The shipped artifact is tokados.img (built in a later step), not the files this
# script emits; kernel.sys / fat12com.bin / *.obj are gitignored. Run from
# anywhere; paths resolve against this script.
#
# Why not freedos\kernel\build.bat? That batch uses the COMPILER=WATCOM profile,
# which builds the *host* build tools (patchobj, exeflat) with `wcl -bt=DOS`,
# i.e. as 16-bit DOS executables. Those cannot run on 64-bit Windows ("not
# compatible with the version of Windows you are running"), so the build dies in
# the LIB stage. The kernel's own cross-compile recipe (top-level makefile,
# `COMPILER=owwin`, see kernel/ci_build.sh) instead builds the host tools as
# native Win32 via owcc, and links the kernel with wlink's native `F { ... }`
# syntax. That top-level makefile is GNU-make syntax (meant for mingw32-make,
# which we don't have), so we replicate its `all` target here with wmake per
# subdir, mirroring kernel/makefile's own dispatch.
$ErrorActionPreference = 'Stop'
$root  = $PSScriptRoot
$fd    = Join-Path $root 'freedos'
$kdir  = Join-Path $fd 'kernel'
$fcdir = Join-Path $fd 'freecom'

# Open Watcom env (mirror toka-dos/build.ps1).
$env:WATCOM  = 'D:\DevTools\OpenWatcom'
$env:PATH    = "$env:WATCOM\binnt;$env:WATCOM\binw;$env:PATH"
$env:INCLUDE = "$env:WATCOM\h"
$env:EDPATH  = "$env:WATCOM\eddat"

# --- Kernel ---
# Target: 8086, FAT32 (-> kernel reports DOS 7.10), NASM, no UPX. These flow to
# the subdir makefiles via wmake macros; owwin.mak appends to XLINK so it must
# arrive via the environment (a wmake *command-line* macro would freeze it bare).
$env:XLINK = 'wlink'
$mk = @(
    'wmake','-ms','-h',
    'COMPILER=owwin','XCPU=86','XFAT=32','XNASM=nasm','XUPX='
)

function Make-Stage($subdir, $target) {
    Push-Location (Join-Path $kdir $subdir)
    try {
        & $mk[0] $mk[1..($mk.Length-1)] $target
        if ($LASTEXITCODE -ne 0) { throw "wmake failed in $subdir/$target ($LASTEXITCODE)" }
    } finally { Pop-Location }
}

# Mirror the top-level makefile `all` target order. (sys/setver/share/country are
# not needed for kernel.sys, so they are skipped.)
Make-Stage 'utils'   'production'                # patchobj (native), exeflat.exe
$libm = Join-Path $kdir 'lib\libm.lib'           # `all` only touches libm.lib here
if (-not (Test-Path $libm)) { & "$env:WATCOM\binnt\wtouch.exe" $libm }
Make-Stage 'drivers' 'production'                # device.lib
Make-Stage 'kernel'  'production'                # kernel.exe -> exeflat -> kernel.sys

$kernelSys = Join-Path $kdir 'bin\kernel.sys'
if (-not (Test-Path $kernelSys)) { throw "kernel.sys not produced at $kernelSys" }
Write-Host "kernel.sys: $((Get-Item $kernelSys).Length) bytes"

# --- FAT12 boot sector ---
$bootBin = Join-Path $kdir 'boot\fat12com.bin'
& nasm -i "$kdir\hdr\" -dISFAT12 "$kdir\boot\boot.asm" -o $bootBin
if ($LASTEXITCODE -ne 0) { throw "boot sector nasm failed" }
if ((Get-Item $bootBin).Length -ne 512) { throw "boot sector is not 512 bytes" }
Write-Host "fat12com.bin: 512 bytes"
