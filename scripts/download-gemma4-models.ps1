param(
    [string]$Destination = "",
    [string]$ModelRepo = "mradermacher/gemma-4-E4B-it-GGUF",
    [string]$ModelFile = "gemma-4-E4B-it.Q4_K_M.gguf",
    [string]$MmprojRepo = "ggml-org/gemma-4-E4B-it-GGUF",
    [string]$MmprojFile = "mmproj-gemma-4-E4B-it-Q8_0.gguf",
    [string]$Revision = "main",
    [string]$HfToken = $env:HF_TOKEN
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $Destination.Trim()) {
    $Destination = Join-Path $env:APPDATA "VoiceKeyboard\VoiceKeyboard\config\models"
}

function Download-HuggingFaceFile(
    [string]$Repo,
    [string]$File,
    [string]$Revision,
    [string]$Token,
    [string]$DestinationDir
) {
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if (-not $curl) {
        throw "curl.exe is required. It is included with recent Windows 10/11 installs."
    }

    New-Item -ItemType Directory -Force -Path $DestinationDir | Out-Null
    $out = Join-Path $DestinationDir (Split-Path $File -Leaf)
    $url = "https://huggingface.co/$Repo/resolve/$Revision/$File"

    $args = @("-L", "--fail", "--retry", "5", "--retry-delay", "2", "-C", "-", "-o", $out)
    if ($Token.Trim()) {
        $args += @("-H", "Authorization: Bearer $Token")
    }
    $args += $url

    Write-Host "Downloading $Repo/$File"
    & $curl.Source @args
    if ($LASTEXITCODE -ne 0) {
        throw "Download failed for $Repo/$File"
    }
}

Download-HuggingFaceFile $ModelRepo $ModelFile $Revision $HfToken $Destination
Download-HuggingFaceFile $MmprojRepo $MmprojFile $Revision $HfToken $Destination

Write-Host ""
Write-Host "Models downloaded to: $Destination"
Write-Host "Restart Voice Keyboard so it can auto-detect the GGUF files."
