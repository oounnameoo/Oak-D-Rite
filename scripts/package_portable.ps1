$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$TargetDir = Join-Path $Root "target"
$ReleaseDir = Join-Path $TargetDir "release"
$DistDir = Join-Path $Root "dist"
$BundleName = "oakd-lite-studio-win64-portable"
$BundleDir = Join-Path $DistDir $BundleName
$ZipPath = Join-Path $DistDir "$BundleName.zip"

if (-not (Test-Path (Join-Path $ReleaseDir "oakd-lite-gui.exe"))) {
    Write-Host "Building release binary..."
    Push-Location $Root
    cargo build --release --bin oakd-lite-gui
    Pop-Location
}

if (Test-Path $BundleDir) { Remove-Item -Recurse -Force $BundleDir }
if (Test-Path $ZipPath) { Remove-Item -Force $ZipPath }
New-Item -ItemType Directory -Force -Path $BundleDir | Out-Null

function Find-RuntimeFile([string[]]$CandidateNames) {
    foreach ($name in $CandidateNames) {
        $direct = Join-Path $ReleaseDir $name
        if (Test-Path $direct) { return $direct }
    }
    foreach ($name in $CandidateNames) {
        $found = Get-ChildItem -Path $TargetDir -Filter $name -Recurse -File -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

Copy-Item (Join-Path $ReleaseDir "oakd-lite-gui.exe") -Destination $BundleDir

$RequiredDlls = @(
    @{ Dest = "depthai-core.dll"; Sources = @("depthai-core.dll") },
    @{ Dest = "opencv_world4130.dll"; Sources = @("opencv_world4130.dll", "opencv_world4110.dll") },
    @{ Dest = "dynamic_calibration.dll"; Sources = @("dynamic_calibration.dll") },
    @{ Dest = "libusb-1.0.dll"; Sources = @("libusb-1.0.dll") }
)

foreach ($item in $RequiredDlls) {
    $src = Find-RuntimeFile $item.Sources
    if (-not $src) {
        throw "Missing required runtime file: $($item.Dest) (searched $TargetDir)"
    }
    Copy-Item $src -Destination (Join-Path $BundleDir $item.Dest)
}

# Bundle app-local VC++ 2015-2022 runtime DLLs so no installer is needed on clean Windows PCs
$VcRuntimeDlls = @(
    "msvcp140.dll",
    "msvcp140_1.dll",
    "msvcp140_2.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "concrt140.dll"
)

foreach ($dll in $VcRuntimeDlls) {
    $sysPath = Join-Path "$env:SystemRoot\System32" $dll
    if (Test-Path $sysPath) {
        Copy-Item $sysPath -Destination $BundleDir
    }
}

$ReadmeText = @"
OAK-D Lite Studio (Portable - Windows x86_64)
=============================================

How to use:
1. Extract all files in this folder together (keep the .dll files next to oakd-lite-gui.exe).
2. Plug in your OAK-D Lite via USB 3.0.
3. Double-click oakd-lite-gui.exe — no installation required.

Note on USB Driver (first-time Windows setup only):
If Windows shows "No available devices" or cannot open the USB device, install the
standard WinUSB driver for "Movidius MyriadX" (VID 03E7, PID 2485) using Zadig
(https://zadig.akeo.ie/).
"@
Set-Content -Path (Join-Path $BundleDir "README-PORTABLE.txt") -Value $ReadmeText -Encoding UTF8

$ProgressPreference = "SilentlyContinue"
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $BundleDir,
    $ZipPath,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
)

$ZipSizeMB = [Math]::Round((Get-Item $ZipPath).Length / 1MB, 2)
Write-Host "Created portable folder: $BundleDir"
Write-Host "Created portable zip:    $ZipPath ($ZipSizeMB MB)"
