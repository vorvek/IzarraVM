# Authoring-only: build Toka-DOS (FreeDOS) from vendored source. Not run in CI.
# Builds the kernel (kernel.sys + fat12com.bin) and the FreeCOM shell
# (command.com), in that order.
#
# The shipped artifact is tokados.img (built in a later step), not the files this
# script emits; kernel.sys / fat12com.bin / command.com / *.obj are gitignored.
# Run from anywhere; paths resolve against this script.
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

# --- FreeCOM (English, XMS-swap default) ---
# Unlike the kernel, FreeCOM's own build.bat works on Win64 *directly* -- so we
# drive it rather than replicate its stages. Two reasons it's not the kernel's
# 16-bit-host-tool wall:
#   1. mkfiles\watcom.mak detects Win64 (__NT__ + %ProgramFiles(x86)) and builds
#      the host helper tools (mktools/mkctxt/chunk/mkinfres/ptchsize) NATIVELY via
#      `owcc` from BINNT, while compiling the shell's own .c to 16-bit DOS .obj
#      with `wcc -bt=dos`. So no host tool is ever a DOS exe.
#   2. The shell link step is `wlinker /ma/nologo @command.rsp`; FreeCOM ships a
#      vendored shell\wlinker.bat wrapper (ms2wlink -> wlink) for exactly this.
# The ONE thing build.bat assumes that modern Win64 cmd doesn't give it: the
# current directory on the exe/batch search path. build.bat `call`s config.bat
# and per-stage helper batches (echoto.bat/echolib.bat copied into each subdir,
# plus shell\wlinker.bat) by bare name, which DOS/old-cmd found in the cwd but
# Win64 cmd does not. Prepending '.' to PATH restores that lookup; with it,
# `build.bat wc english` runs clean end-to-end (suppl -> utils -> strings ->
# criter -> lib -> cmd -> shell -> mkinfres + copy/b assembly -> ptchsize +6KB).
# XMS-only swap is build.bat's default (XMS_SWAP=1 -> xmsswap.cln), which is what
# we want. command.com lands at the freecom tree root (~85 KB).
Copy-Item (Join-Path $fcdir 'config.b')   (Join-Path $fcdir 'config.bat') -Force
Copy-Item (Join-Path $fcdir 'config.std') (Join-Path $fcdir 'config.mak') -Force
@"
set COMPILER=WATCOM
set WATCOM=$($env:WATCOM)
set XNASM=nasm
set PATH=%PATH%;$($env:WATCOM)\binnt;$($env:WATCOM)\binw
"@ | Set-Content -Encoding Ascii (Join-Path $fcdir 'config.bat')

$fcPath = $env:PATH
Push-Location $fcdir
try {
    # '.' first so config.bat / echoto.bat / echolib.bat / shell\wlinker.bat
    # resolve from each subdir's cwd as build.bat descends the tree.
    $env:PATH = ".;$fcPath"
    & cmd /c ".\build.bat -r wc english"
    if ($LASTEXITCODE -ne 0) { throw "freecom build.bat failed ($LASTEXITCODE)" }
} finally {
    $env:PATH = $fcPath
    Pop-Location
}
$commandCom = Join-Path $fcdir 'command.com'
if (-not (Test-Path $commandCom)) { throw "command.com not produced at $commandCom" }
Write-Host "command.com: $((Get-Item $commandCom).Length) bytes"
