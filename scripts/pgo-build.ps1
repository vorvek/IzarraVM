#!/usr/bin/env pwsh
# Profile-guided-optimization (PGO) release build for izarravm.
#
# Two-stage build: instrument -> run a representative workload to gather a
# profile -> rebuild with the profile. This is a RELEASE recipe, not the default
# `cargo build` (which is untouched). See dev_docs/2026-06-28-perf-pgo-design.md.
#
# Why --config instead of $env:RUSTFLAGS: the project pins
# `target.x86_64-pc-windows-msvc.rustflags = ["-C","target-cpu=x86-64-v3"]` in
# .cargo/config.toml. Setting RUSTFLAGS would REPLACE (not merge) that, silently
# dropping target-cpu=x86-64-v3 from both PGO stages -- shipping a wrong binary
# and invalidating the A/B. Overriding the same target key via --config keeps the
# flag explicit and merges nothing implicitly.
#
# Usage:  pwsh scripts/pgo-build.ps1
# Leaves the optimized binary in target/release/izarravm.exe and prints a
# baseline-vs-PGO --headless-bench comparison.

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$triple = "x86_64-pc-windows-msvc"
$targetCpu = "x86-64-v3"  # must match .cargo/config.toml
$profRaw = Join-Path $env:TEMP "izarravm-pgo\raw"
$profData = Join-Path $env:TEMP "izarravm-pgo\merged.profdata"
$bin = "target/release/izarravm.exe"

function Rustflags([string[]]$flags) {
    # Build a --config value overriding the target rustflags array.
    $items = ($flags | ForEach-Object { "'$_'" }) -join ","
    return "target.$triple.rustflags=[$items]"
}

# --- locate a version-matched llvm-profdata --------------------------------
$rustcLlvm = (rustc -vV | Select-String "LLVM version:\s*(\d+)").Matches.Groups[1].Value
$sysroot = (rustc --print sysroot).Trim()
$profdataExe = Join-Path $sysroot "lib/rustlib/$triple/bin/llvm-profdata.exe"
if (-not (Test-Path $profdataExe)) {
    # Fall back to a standalone LLVM install.
    $profdataExe = "C:\Program Files\LLVM\bin\llvm-profdata.exe"
    if (-not (Test-Path $profdataExe)) {
        throw "llvm-profdata not found. Run: rustup component add llvm-tools"
    }
}
$pdLlvm = (& $profdataExe --version | Select-String "LLVM version (\d+)").Matches.Groups[1].Value
if ($rustcLlvm -ne $pdLlvm) {
    throw "LLVM major mismatch: rustc=$rustcLlvm, llvm-profdata=$pdLlvm. Profile data is keyed to the LLVM major; aborting before the instrumented build. Run: rustup component add llvm-tools"
}
Write-Host "llvm-profdata: $profdataExe (LLVM $pdLlvm, matches rustc)" -ForegroundColor Green

# --- baseline (plain release, target-cpu=v3 from config) -------------------
Write-Host "`n[1/5] baseline release build..." -ForegroundColor Cyan
cargo build --release -p izarravm
Write-Host "[1/5] baseline --headless-bench..." -ForegroundColor Cyan
$baseline = & $bin --headless-bench 2>$null

# --- stage 1: instrumented build -------------------------------------------
Write-Host "`n[2/5] instrumented build (profile-generate)..." -ForegroundColor Cyan
if (Test-Path $profRaw) { Remove-Item -Recurse -Force $profRaw }
New-Item -ItemType Directory -Force -Path $profRaw | Out-Null
cargo build --release -p izarravm --config (Rustflags @("-C", "target-cpu=$targetCpu", "-C", "profile-generate=$profRaw"))

# --- gather a representative profile ---------------------------------------
Write-Host "[3/5] gathering profile (bench + boot suite)..." -ForegroundColor Cyan
& $bin --headless-bench 2>$null | Out-Null
& $bin --headless-boot-suite 2>$null | Out-Null

# --- merge ------------------------------------------------------------------
Write-Host "[4/5] merging profraw -> profdata..." -ForegroundColor Cyan
& $profdataExe merge -o $profData (Get-ChildItem -Path $profRaw -Filter *.profraw | ForEach-Object { $_.FullName })

# --- stage 2: optimized build ----------------------------------------------
Write-Host "`n[5/5] optimized build (profile-use)..." -ForegroundColor Cyan
cargo build --release -p izarravm --config (Rustflags @("-C", "target-cpu=$targetCpu", "-C", "profile-use=$profData"))
$pgo = & $bin --headless-bench 2>$null

# --- report -----------------------------------------------------------------
function Bench586($lines) {
    $lines | Where-Object { $_ -match "^\w+\s+586\s" } | ForEach-Object {
        $f = $_ -split "\s+"; "{0,-10} {1,8}" -f $f[0], $f[8]
    }
}
Write-Host "`n=== baseline (release, v3) 586 rt_factor ===" -ForegroundColor Yellow
Bench586 $baseline
Write-Host "=== PGO (release, v3 + profile-use) 586 rt_factor ===" -ForegroundColor Yellow
Bench586 $pgo
Write-Host "`nOptimized binary: $bin" -ForegroundColor Green
