# Fetches a known-good prebuilt bootable FreeDOS 1.44MB floppy image for the SP-1
# boot spike. The image is a dev-local GPL FreeDOS artifact, never committed.
# Override the source if the FreeDOS download layout changes:
#   $env:FREEDOS_SPIKE_URL  -> a .zip of floppy images to download (default below)
#   $env:FREEDOS_SPIKE_SRC  -> a local .zip or .img to use instead of downloading
$ErrorActionPreference = 'Stop'

$Url  = if ($env:FREEDOS_SPIKE_URL) { $env:FREEDOS_SPIKE_URL }
        else { 'https://www.ibiblio.org/pub/micro/pc-stuff/freedos/files/distributions/1.3/FD13-FloppyEdition.zip' }
$Dest = Join-Path $PSScriptRoot '..\.local\freedos'
$Img  = Join-Path $Dest 'freedos-spike.img'
$Size = 1474560   # 1.44MB floppy: 80 cyl x 2 heads x 18 sec x 512 b

New-Item -ItemType Directory -Force -Path $Dest | Out-Null

if ((Test-Path $Img -PathType Leaf) -and ((Get-Item $Img).Length -eq $Size)) {
    Write-Host "Already present: $Img"
    Write-Host "Set IZARRAVM_FREEDOS_SPIKE_IMG=$Img"
    exit 0
}

$work = Join-Path $Dest '_work'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

$src = $env:FREEDOS_SPIKE_SRC
if (-not $src) {
    $src = Join-Path $work 'src.zip'
    Write-Host "Downloading $Url"
    Invoke-WebRequest -Uri $Url -OutFile $src
}

if ($src -like '*.img' -or $src -like '*.ima') {
    Copy-Item $src $Img -Force
} else {
    Expand-Archive -Path $src -DestinationPath $work -Force
    $imgs = Get-ChildItem -Path $work -Recurse -File |
        Where-Object { $_.Length -eq $Size -and $_.Extension -match '\.(img|ima|144|flp)$' }
    # Prefer a boot disk (name contains "boot"); otherwise take any 1.44MB image.
    $cand = ($imgs | Where-Object { $_.Name -match '(?i)boot' } | Select-Object -First 1)
    if (-not $cand) { $cand = $imgs | Select-Object -First 1 }
    if (-not $cand) {
        throw "No 1,474,560-byte floppy image found in the source. Set `$env:FREEDOS_SPIKE_SRC to a local boot floppy .img."
    }
    Copy-Item $cand.FullName $Img -Force
}

Remove-Item -Recurse -Force $work
$len = (Get-Item $Img).Length
if ($len -ne $Size) { throw "Image is $len bytes, expected $Size." }
Write-Host "FreeDOS spike image ready: $Img ($len bytes)"
Write-Host "Set IZARRAVM_FREEDOS_SPIKE_IMG=$Img for the smoke test."
