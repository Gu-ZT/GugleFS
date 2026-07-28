param(
    [switch]$Install
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$msiPath = Join-Path $repositoryRoot 'src-tauri\resources\winfsp-2.1.25156.msi'
$expectedHash = '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A'

if (-not (Test-Path -LiteralPath $msiPath -PathType Leaf)) {
    throw "WinFsp MSI not found: $msiPath"
}

$actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $msiPath).Hash
if ($actualHash -ne $expectedHash) {
    throw "WinFsp MSI hash mismatch. Expected $expectedHash, got $actualHash"
}

if ($Install) {
    Write-Host "Installing WinFsp SDK from $msiPath"
    $arguments = "/i `"$msiPath`" /qn /norestart INSTALLLEVEL=1000"
    $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -notin @(0, 1638, 3010)) {
        throw "WinFsp installation failed with exit code $($process.ExitCode)"
    }
}

Write-Host "WinFsp MSI verified: $actualHash"
