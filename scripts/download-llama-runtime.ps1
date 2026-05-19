param(
    [string]$Version = "b9222",
    [string]$Backend = "vulkan",
    [string]$Destination = "src-tauri\resources\runtime",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$DestinationPath = if ([System.IO.Path]::IsPathRooted($Destination)) {
    $Destination
} else {
    Join-Path $RepoRoot $Destination
}

$assets = @{
    vulkan = @{
        Name = "llama-b9222-bin-win-vulkan-x64.zip"
        Url = "https://github.com/ggml-org/llama.cpp/releases/download/b9222/llama-b9222-bin-win-vulkan-x64.zip"
        Sha256 = "71d896955c0ae3576f6fd147f4ca197aa6a839ed7693651c14fe74014cef954e"
    }
}

if ($Version -ne "b9222") {
    throw "Unsupported llama.cpp version '$Version'. This script is pinned to b9222."
}
if (-not $assets.ContainsKey($Backend)) {
    throw "Unsupported llama.cpp backend '$Backend'. Supported backends: $($assets.Keys -join ', ')"
}

$asset = $assets[$Backend]
$serverPath = Join-Path $DestinationPath "llama-server.exe"
if ((Test-Path -LiteralPath $serverPath) -and -not $Force) {
    Write-Host "llama.cpp runtime already staged: $DestinationPath"
    exit 0
}

$tempRoot = Join-Path $RepoRoot ".runtime-download"
$zipPath = Join-Path $tempRoot $asset.Name
$extractPath = Join-Path $tempRoot "extract"

if (Test-Path -LiteralPath $tempRoot) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
New-Item -ItemType Directory -Force -Path $extractPath | Out-Null

Write-Host "Downloading llama.cpp $Version Windows x64 $Backend runtime..."
Invoke-WebRequest -Uri $asset.Url -OutFile $zipPath -UseBasicParsing

$actualSha = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualSha -ne $asset.Sha256) {
    throw "Checksum mismatch for $($asset.Name). Expected $($asset.Sha256), got $actualSha"
}

Expand-Archive -LiteralPath $zipPath -DestinationPath $extractPath -Force

$server = Get-ChildItem -LiteralPath $extractPath -Recurse -File -Filter "llama-server.exe" |
    Select-Object -First 1
if (-not $server) {
    throw "Downloaded llama.cpp archive did not contain llama-server.exe"
}

if (Test-Path -LiteralPath $DestinationPath) {
    Remove-Item -LiteralPath $DestinationPath -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $DestinationPath | Out-Null

$runtimeFiles = Get-ChildItem -LiteralPath $server.DirectoryName -File |
    Where-Object { $_.Extension -in @(".exe", ".dll", ".json", ".txt") }
foreach ($file in $runtimeFiles) {
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $DestinationPath $file.Name) -Force
}

$manifest = [ordered]@{
    source = "ggml-org/llama.cpp"
    version = $Version
    backend = $Backend
    asset = $asset.Name
    url = $asset.Url
    sha256 = $asset.Sha256
    staged_at_utc = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
}
$manifest | ConvertTo-Json -Depth 4 |
    Set-Content -LiteralPath (Join-Path $DestinationPath "runtime-manifest.json") -Encoding UTF8

if (-not (Test-Path -LiteralPath $serverPath)) {
    throw "llama-server.exe was not staged into $DestinationPath"
}

Write-Host "Staged llama.cpp runtime in: $DestinationPath"
