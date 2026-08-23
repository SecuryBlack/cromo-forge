# CromoForge — Windows Install Script
# Usage: irm https://install.cromoforge.dev | iex -Token <TOKEN>
[CmdletBinding()]
param(
    [string]$Token  = "",
    [string]$Source = "local"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$SbAgentLabel = "cromoforge"
$libUrl = "https://raw.githubusercontent.com/securyblack/sb-agent-core/master/scripts/install-lib.ps1"
$libTmp = Join-Path ([System.IO.Path]::GetTempPath()) "sb-agent-core-install-lib.ps1"
Invoke-WebRequest -Uri $libUrl -OutFile $libTmp -UseBasicParsing
. $libTmp

# ─── Constants ────────────────────────────────────────────────────────────────
$GithubRepo  = "securyblack/cromo-forge"
$BinaryName  = "cromoforge.exe"
$InstallDir  = "$env:ProgramFiles\CromoForge"
$ConfigDir   = "$env:ProgramData\cromoforge"
$ConfigFile  = "$ConfigDir\config.toml"
$ServiceName = "CromoForge"

Write-Host ""
Write-Host "  CromoForge — Deploy Agent" -ForegroundColor Cyan -NoNewline
Write-Host " (Windows Installer)" -ForegroundColor Gray
Write-Host ""

Assert-SbAdmin
$target = Get-SbArchTarget
$version = Get-SbLatestVersion -GithubRepo $GithubRepo

$tmpDir = [System.IO.Path]::GetTempPath() + [System.IO.Path]::GetRandomFileName()
New-Item -ItemType Directory -Path $tmpDir | Out-Null

try {
    $assetName = "cromoforge-$target.zip"
    $zipPath = Get-SbReleaseAsset -GithubRepo $GithubRepo -Version $version -AssetName $assetName -TmpDir $tmpDir
    Install-SbBinaryFromZip -ZipPath $zipPath -BinaryName $BinaryName -InstallDir $InstallDir -ServiceName $ServiceName

    New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null

    if (-not $Token) {
        $secToken = Read-Host "  Auth token" -AsSecureString
        $Token = [Runtime.InteropServices.Marshal]::PtrToStringAuto(
                    [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secToken))
    }
    if (-not $Token) { Invoke-SbFail "Token cannot be empty" }

    Write-SbInfo "Writing config to $ConfigFile..."
    @"
# CromoForge configuration
# Do not share this file — it contains your auth token.
version = "$version"
token = "$Token"
source = "$Source"
poll_interval_secs = 30
"@ | Set-Content -Path $ConfigFile -Encoding UTF8

    Protect-SbConfigFile -Path $ConfigFile
    Write-SbSuccess "Config written"

    Register-SbWindowsService -ServiceName $ServiceName -DisplayName "CromoForge Deploy Agent" `
        -BinaryPath "$InstallDir\$BinaryName" `
        -Description "SecuryBlack CromoForge deploy agent. See https://github.com/$GithubRepo"

} finally {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "  CromoForge $version installed successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "  Status:  " -NoNewline; Write-Host "Get-Service CromoForge" -ForegroundColor White
Write-Host "  Config:  " -NoNewline; Write-Host $ConfigFile -ForegroundColor White
Write-Host ""
