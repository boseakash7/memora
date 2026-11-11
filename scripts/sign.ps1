# Signs one file with Authenticode. `cargo tauri build` calls it for memora.exe, the
# installer and the uninstaller when scripts\build-installer.ps1 finds a certificate set up.
#
#   powershell -ExecutionPolicy Bypass -File scripts\sign.ps1 <file>
#
# Set up ONE of these (as environment variables) before building:
#   MEMORA_SIGN_THUMBPRINT           SHA-1 thumbprint of a code-signing certificate in the Windows
#                                    certificate store (also hardware tokens and cloud keys that
#                                    appear there, e.g. Certum SimplyShare, SSL.com eSigner).
#   MEMORA_SIGN_PFX                  Path to a .pfx file; its password in MEMORA_SIGN_PFX_PASSWORD.
#   MEMORA_TRUSTED_SIGNING_METADATA  Azure Trusted Signing: path to metadata.json, plus
#   MEMORA_TRUSTED_SIGNING_DLIB      the path to Azure.CodeSigning.Dlib.dll.
# Optional:
#   MEMORA_SIGN_TIMESTAMP_URL        RFC 3161 timestamp server (default: DigiCert's).
#   MEMORA_SIGNTOOL                  Path to signtool.exe (default: newest Windows SDK one).

param([Parameter(Mandatory = $true)][string]$File)
$ErrorActionPreference = 'Stop'

function Find-SignTool {
    if ($env:MEMORA_SIGNTOOL) { return $env:MEMORA_SIGNTOOL }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $found = Get-ChildItem $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' } |
        Sort-Object { [version]($_.Directory.Parent.Name -replace '[^0-9.]', '') } -Descending |
        Select-Object -First 1
    if (-not $found) { throw 'signtool.exe not found: install the Windows SDK or set MEMORA_SIGNTOOL' }
    return $found.FullName
}

$timestamp = if ($env:MEMORA_SIGN_TIMESTAMP_URL) { $env:MEMORA_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
# /d is the name Windows shows in the "Do you want to allow this app..." prompt.
$signArgs = @('sign', '/fd', 'SHA256', '/tr', $timestamp, '/td', 'SHA256', '/d', 'Memora')

if ($env:MEMORA_SIGN_THUMBPRINT) {
    $signArgs += @('/sha1', $env:MEMORA_SIGN_THUMBPRINT)
} elseif ($env:MEMORA_SIGN_PFX) {
    $signArgs += @('/f', $env:MEMORA_SIGN_PFX)
    if ($env:MEMORA_SIGN_PFX_PASSWORD) { $signArgs += @('/p', $env:MEMORA_SIGN_PFX_PASSWORD) }
} elseif ($env:MEMORA_TRUSTED_SIGNING_METADATA) {
    $signArgs += @('/dlib', $env:MEMORA_TRUSTED_SIGNING_DLIB, '/dmdf', $env:MEMORA_TRUSTED_SIGNING_METADATA)
} else {
    throw 'No signing certificate configured (see the top of scripts\sign.ps1)'
}

$signtool = Find-SignTool
& $signtool @signArgs $File
if ($LASTEXITCODE -ne 0) { throw "signtool failed for $File" }
