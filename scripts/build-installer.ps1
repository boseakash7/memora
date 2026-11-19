# Builds the Windows installer with everything Memora needs inside it: FFmpeg, the face and
# content recognition models and DirectML.dll, so nothing else has to be installed.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 [-Models <folder>] [-NoModels]
#
# Output: <cargo target>\release\bundle\nsis\Memora_<version>_x64-setup.exe, the full offline
# installer. With -NoModels: Memora_<version>_x64-online-setup.exe, about 600 MB smaller; the
# app then offers to download the AI features the user picks on first start (and in Settings).
#
# Signing: when a certificate is set up (see scripts\sign.ps1), memora.exe, the installer and
# the uninstaller are signed. Set MEMORA_REQUIRE_SIGNING=1 for release builds, so a missing
# certificate stops the build instead of producing an unsigned installer.
#
# Downloads (FFmpeg, and the models if -Models doesn't have them) are kept in the cargo target
# folder, so later builds work offline. Plain `cargo tauri build` also works, but its installer
# lacks these files (the app then uses FFmpeg from PATH and models from %LOCALAPPDATA%).

param([string]$Models = (Join-Path $env:LOCALAPPDATA 'Memora\models'), [switch]$NoModels)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# FFmpeg 7.1.1 "essentials" build by Gyan Doshi (GPLv3; source: see THIRD-PARTY-NOTICES.md),
# pinned by hash. Memora is tested against FFmpeg 7.1: HEIC tile layout is read from the file
# to match FFmpeg 7.1's stream numbering (crates/core/src/heif.rs), so update with care and
# run the `heif_layout_matches_ffmpeg` test against the new version.
$ffmpegVersion = '7.1.1'
$ffmpegUrl = "https://github.com/GyanD/codexffmpeg/releases/download/$ffmpegVersion/ffmpeg-$ffmpegVersion-essentials_build.zip"
$ffmpegSha256 = '04861d3339c5ebe38b56c19a15cf2c0cc97f5de4fa8910e4d47e5e6404e4a2d4'

$modelFiles = @(
    'face_detection_yunet_2023mar.onnx',
    'face_recognition_sface_2021dec.onnx',
    'clip-vit-base-patch32\vision_model_quantized.onnx',
    'clip-vit-base-patch32\text_model_quantized.onnx',
    'clip-vit-base-patch32\tokenizer.json',
    'birefnet-lite\model.onnx',
    'birefnet-lite-512\model.onnx'
)
if ($NoModels) { $modelFiles = @() }

function Get-SigningConfigured {
    return [bool]($env:MEMORA_SIGN_THUMBPRINT -or $env:MEMORA_SIGN_PFX -or $env:MEMORA_TRUSTED_SIGNING_METADATA)
}
if ($env:MEMORA_REQUIRE_SIGNING -eq '1' -and -not (Get-SigningConfigured)) {
    throw 'MEMORA_REQUIRE_SIGNING is set but no certificate is configured (see scripts\sign.ps1)'
}

$metadata = cargo metadata --format-version 1 --no-deps --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json
$target = $metadata.target_directory
$stage = Join-Path $target 'installer-files'
$downloads = Join-Path $target 'downloads'
New-Item -ItemType Directory -Force $stage, $downloads | Out-Null

# ---------- FFmpeg ----------
$zip = Join-Path $downloads "ffmpeg-$ffmpegVersion-essentials_build.zip"
if (-not (Test-Path $zip) -or (Get-FileHash $zip -Algorithm SHA256).Hash -ne $ffmpegSha256) {
    Write-Host "Downloading FFmpeg $ffmpegVersion"
    Invoke-WebRequest -Uri $ffmpegUrl -OutFile $zip -UseBasicParsing
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($hash -ne $ffmpegSha256) {
        Remove-Item $zip
        throw "The FFmpeg download has an unexpected SHA-256 ($hash); not using it"
    }
}
$ffmpegDir = Join-Path $stage "ffmpeg-$ffmpegVersion"
# Extracted fresh every time: a signed build signs these copies in place, and a later build
# must not mistake that (perhaps test) signature for its own.
& {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    New-Item -ItemType Directory -Force $ffmpegDir | Out-Null
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try {
        foreach ($entry in $archive.Entries) {
            $name = switch ($entry.FullName) {
                { $_ -like '*/bin/ffmpeg.exe' } { 'ffmpeg.exe' }
                { $_ -like '*/bin/ffprobe.exe' } { 'ffprobe.exe' }
                { $_ -like '*/LICENSE' } { 'FFmpeg-LICENSE.txt' }
                { $_ -like '*/README.txt' } { 'FFmpeg-README.txt' }
                default { $null }
            }
            if ($name) {
                [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $ffmpegDir $name), $true)
            }
        }
    } finally {
        $archive.Dispose()
    }
}

# ---------- recognition models ----------
$missing = $modelFiles | Where-Object { -not (Test-Path (Join-Path $Models $_)) }
if ($missing) {
    $Models = Join-Path $downloads 'models'
    Write-Host "Models not found locally; fetching them into $Models"
    & (Join-Path $PSScriptRoot 'fetch-models.ps1') $Models
}

# ---------- DirectML.dll (copied next to the build output by the ort crate) ----------
Push-Location $root
try {
    cargo build --release -p photos-ml
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
} finally {
    Pop-Location
}
$directml = Join-Path $target 'release\DirectML.dll'
if (-not (Test-Path $directml)) { throw "DirectML.dll not found at $directml" }
# ort puts a link to its download cache there. The Tauri build copies resources into that
# folder too, which would write through the link into the cache (and fail while a dev build
# of Memora has the DLL open), so make it a plain copy; ort leaves plain files alone.
$item = Get-Item $directml
if ($item.LinkType) {
    $real = @($item.Target)[0]
    Remove-Item $directml
    Copy-Item $real $directml
}
Copy-Item $directml (Join-Path $stage 'DirectML.dll') -Force

# ---------- everything in one folder ----------
$modelStage = Join-Path $stage 'models'
foreach ($file in $modelFiles) {
    $dest = Join-Path $modelStage $file
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Copy-Item (Join-Path $Models $file) $dest -Force
}
$licenseStage = Join-Path $stage 'licenses'
New-Item -ItemType Directory -Force $licenseStage | Out-Null
Copy-Item (Join-Path $ffmpegDir 'FFmpeg-LICENSE.txt'), (Join-Path $ffmpegDir 'FFmpeg-README.txt') $licenseStage -Force
Copy-Item (Join-Path $root 'THIRD-PARTY-NOTICES.md') $licenseStage -Force
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $licenseStage 'Memora-LICENSE.txt') -Force

# The Tauri CLI loses the drive letter of absolute resource paths on another drive than the
# project, so the staging folder (in the cargo target folder) is reached through a junction
# inside src-tauri and given as relative paths.
$link = Join-Path $root 'src-tauri\installer-files'
$existing = Get-Item $link -ErrorAction SilentlyContinue
if ($existing -and ($existing.LinkType -ne 'Junction' -or $existing.Target -notcontains $stage)) {
    if ($existing.LinkType -ne 'Junction') { throw "$link exists and is not a junction; move it away" }
    cmd /c rmdir "$link" | Out-Null  # removes only the junction, never its target's files
    $existing = $null
}
if (-not $existing) { New-Item -ItemType Junction -Path $link -Target $stage | Out-Null }

# ---------- what goes next to memora.exe ----------
$ffmpegRel = "installer-files/ffmpeg-$ffmpegVersion"
$resources = [ordered]@{
    "$ffmpegRel/ffmpeg.exe"   = 'ffmpeg.exe'
    "$ffmpegRel/ffprobe.exe"  = 'ffprobe.exe'
    'installer-files/DirectML.dll' = 'DirectML.dll'
}
foreach ($file in $modelFiles) {
    $rel = 'models/' + ($file -replace '\\', '/')
    $resources["installer-files/$rel"] = $rel
}
foreach ($file in Get-ChildItem $licenseStage -File) {
    $resources["installer-files/licenses/$($file.Name)"] = "licenses/$($file.Name)"
}

$windows = [ordered]@{}
if (Get-SigningConfigured) {
    $windows['signCommand'] = [ordered]@{
        cmd  = 'powershell'
        args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $PSScriptRoot 'sign.ps1'), '%1')
    }
    # The Tauri CLI also runs signtool itself, to see which bundled .exe and .dll files are
    # already signed (unsigned ones, such as ffmpeg.exe, get signed too).
    $signtool = if ($env:MEMORA_SIGNTOOL) { Get-Item $env:MEMORA_SIGNTOOL } else {
        Get-ChildItem (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin') -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.Directory.Name -eq 'x64' } |
            Sort-Object { [version]($_.Directory.Parent.Name -replace '[^0-9.]', '') } -Descending |
            Select-Object -First 1
    }
    if (-not $signtool) { throw 'signtool.exe not found: install the Windows SDK or set MEMORA_SIGNTOOL' }
    $env:TAURI_WINDOWS_SIGNTOOL_PATH = $signtool.FullName
    Write-Host 'Signing is set up: memora.exe and the installer will be signed'
} else {
    Write-Host 'No signing certificate set up: the installer will be unsigned (see scripts\sign.ps1)'
}
$config = [ordered]@{ bundle = [ordered]@{ resources = $resources; windows = $windows } }
$configFile = Join-Path $stage 'tauri.installer.json'
# UTF-8 without a byte-order mark, which the Tauri CLI's JSON reader wants.
[System.IO.File]::WriteAllText($configFile, ($config | ConvertTo-Json -Depth 6))

# ---------- build ----------
# Tauri names both installers <name>_<version>_x64-setup.exe, so an online build would
# overwrite a full installer of the same version: keep it aside while bundling.
$nsis = Join-Path $target 'release\bundle\nsis'
$kept = $null
if ($NoModels) {
    $full = Join-Path $nsis ("Memora_{0}_x64-setup.exe" -f $metadata.packages[0].version)
    if (Test-Path $full) {
        $kept = "$full.full"
        Move-Item $full $kept -Force
    }
}
Push-Location $root
try {
    cargo tauri build --config $configFile
    if ($LASTEXITCODE -ne 0) {
        if ($kept) { Move-Item $kept ($kept -replace '\.full$', '') -Force }
        throw 'cargo tauri build failed'
    }
} finally {
    Pop-Location
}

$installer = Get-ChildItem (Join-Path $target 'release\bundle\nsis') -Filter '*-setup.exe' |
    Where-Object { $_.Name -notlike '*-online-setup.exe' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($NoModels) {
    # Kept beside the full installer under its own name.
    $online = Join-Path $installer.DirectoryName ($installer.Name -replace '-setup\.exe$', '-online-setup.exe')
    Move-Item $installer.FullName $online -Force
    $installer = Get-Item $online
}
if ($kept) { Move-Item $kept ($kept -replace '\.full$', '') -Force }
$signature = Get-AuthenticodeSignature $installer.FullName
Write-Host ''
Write-Host ("Installer: {0} ({1:N0} MB)" -f $installer.FullName, ($installer.Length / 1MB))
Write-Host ("Signature: {0}" -f $signature.Status)
