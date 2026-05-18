param(
    [string]$RuntimeDir = "portable-export\VoiceKeyboard-portable-20260507\runtime",
    [string[]]$ModelFiles = @(),
    [string[]]$ModelDirs = @(),
    [string]$HfRepo = "",
    [string[]]$HfFiles = @(),
    [string]$HfRevision = "main",
    [string]$HfToken = $env:HF_TOKEN,
    [string]$ModelSubdir = "",
    [switch]$NoModels,
    [switch]$CleanResources,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-RepoPath([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return $Path
    }
    return Join-Path $RepoRoot $Path
}

function Remove-DirectorySafely([string]$Path) {
    $resolved = [System.IO.Path]::GetFullPath($Path)
    $resourceRoot = [System.IO.Path]::GetFullPath($ResourcesDir)
    if (-not $resolved.StartsWith($resourceRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove path outside resources: $resolved"
    }
    if (Test-Path -LiteralPath $resolved) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}

function Copy-BackendRuntime([string]$Source, [string]$Destination) {
    if (-not (Test-Path -LiteralPath $Source)) {
        throw "RuntimeDir does not exist: $Source"
    }
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    $patterns = @("*.exe", "*.dll", "*.json", "*.txt")
    foreach ($pattern in $patterns) {
        Get-ChildItem -LiteralPath $Source -File -Filter $pattern |
            Where-Object { $_.Name -notmatch "\.(log|out|err)$" } |
            ForEach-Object {
                Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $Destination $_.Name) -Force
            }
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Destination "llama-server.exe"))) {
        throw "llama-server.exe was not found in staged runtime: $Destination"
    }
}

function Copy-ModelFile([string]$SourceFile, [string]$DestinationRoot) {
    if (-not (Test-Path -LiteralPath $SourceFile)) {
        throw "Model file does not exist: $SourceFile"
    }
    $parentName = Split-Path (Split-Path $SourceFile -Parent) -Leaf
    $destinationDir = Join-Path $DestinationRoot $parentName
    New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null
    Copy-Item -LiteralPath $SourceFile -Destination (Join-Path $destinationDir (Split-Path $SourceFile -Leaf)) -Force
}

function Copy-ModelDirectory([string]$SourceDir, [string]$DestinationRoot) {
    if (-not (Test-Path -LiteralPath $SourceDir)) {
        throw "Model directory does not exist: $SourceDir"
    }
    $destinationDir = Join-Path $DestinationRoot (Split-Path $SourceDir -Leaf)
    if (Test-Path -LiteralPath $destinationDir) {
        Remove-DirectorySafely $destinationDir
    }
    New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null
    Get-ChildItem -LiteralPath $SourceDir -File -Recurse |
        Where-Object { $_.Name -notmatch "\.(curl\.log|lock|part|tmp)$" -and $_.FullName -notmatch "\\\.cache\\" } |
        ForEach-Object {
            $relative = [System.IO.Path]::GetRelativePath($SourceDir, $_.FullName)
            $target = Join-Path $destinationDir $relative
            New-Item -ItemType Directory -Force -Path (Split-Path $target -Parent) | Out-Null
            Copy-Item -LiteralPath $_.FullName -Destination $target -Force
        }
}

function Download-HuggingFaceFile([string]$Repo, [string]$Revision, [string]$File, [string]$Token, [string]$DestinationRoot) {
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if (-not $curl) {
        throw "curl.exe is required for Hugging Face downloads"
    }

    $subdir = if ($ModelSubdir.Trim()) {
        $ModelSubdir.Trim()
    } else {
        $Repo.Replace("/", "__")
    }
    $destinationDir = Join-Path $DestinationRoot $subdir
    New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null

    $fileName = Split-Path $File -Leaf
    $destination = Join-Path $destinationDir $fileName
    $url = "https://huggingface.co/$Repo/resolve/$Revision/$File"

    $args = @("-L", "--fail", "--retry", "5", "--retry-delay", "2", "-o", $destination)
    if ($Token.Trim()) {
        $args += @("-H", "Authorization: Bearer $Token")
    }
    $args += $url

    Write-Host "Downloading $File from $Repo..."
    & $curl.Source @args
    if ($LASTEXITCODE -ne 0) {
        throw "Hugging Face download failed for $File"
    }
}

function Get-RelativeResourcePath([string]$FullPath) {
    $relative = [System.IO.Path]::GetRelativePath($ResourcesDir, $FullPath)
    return $relative.Replace("/", "\")
}

function Write-BundledSettings([string]$RuntimeOut, [string]$ModelsOut) {
    $model = Get-ChildItem -LiteralPath $ModelsOut -File -Recurse -Filter "*.gguf" -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notmatch "mmproj" } |
        Sort-Object Length -Descending |
        Select-Object -First 1
    $mmproj = Get-ChildItem -LiteralPath $ModelsOut -File -Recurse -Filter "*.gguf" -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match "mmproj" } |
        Sort-Object Length -Descending |
        Select-Object -First 1

    $settings = [ordered]@{
        audio_chunk_ms = 500
        rolling_history_seconds = 30
        pre_roll_ms = 2000
        trigger_hold_ms = 450
        right_click_trigger_enabled = $false
        movement_tolerance_px = 12.0
        vad_rms_threshold = 0.008
        vad_calibrated = $false
        calibration_prompt_enabled = $true
        shortcuts_enabled = $true
        context_enabled = $true
        dry_run = $false
        confirm_large_text_chars = 800
        confirm_close_shortcuts = $true
        kill_switch_enabled = $true
        injection_delay_ms = 20
        managed_server = $true
        server_url = "http://127.0.0.1:8099"
        llama_server_path = "runtime\llama-server.exe"
        llama_device = "Vulkan0"
        model_path = if ($model) { Get-RelativeResourcePath $model.FullName } else { "models\model.gguf" }
        mmproj_path = if ($mmproj) { Get-RelativeResourcePath $mmproj.FullName } else { "" }
        context_length_tokens = 4096
        log_retention_bytes = 5242880
        common_terms = ""
        spoken_languages = "English"
        recent_context_enabled = $true
        recent_context_max_requests = 5
        recent_context_window_seconds = 60
        recent_context_max_items = 5
        thinking_handoff_enabled = $true
        thinking_handoff_min_chars = 250
        thinking_handoff_reasoning_budget = 64
        thinking_handoff_context_items = 3
    }

    $configDir = Join-Path $ResourcesDir "portable-config"
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    $settingsJson = $settings | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText(
        (Join-Path $configDir "settings.json"),
        $settingsJson,
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Update-TauriResources() {
    $configPath = Join-Path $RepoRoot "src-tauri\tauri.conf.json"
    $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    if (-not $config.bundle) {
        $config | Add-Member -MemberType NoteProperty -Name bundle -Value ([pscustomobject]@{}) -Force
    }
    $config.bundle | Add-Member -MemberType NoteProperty -Name resources -Value @("resources/**/*") -Force
    $configJson = $config | ConvertTo-Json -Depth 20
    [System.IO.File]::WriteAllText($configPath, $configJson, [System.Text.UTF8Encoding]::new($false))
}

$RepoRoot = Split-Path -Parent $PSScriptRoot
$ResourcesDir = Join-Path $RepoRoot "src-tauri\resources"
$RuntimeOut = Join-Path $ResourcesDir "runtime"
$ModelsOut = Join-Path $ResourcesDir "models"
$RuntimeSource = Resolve-RepoPath $RuntimeDir

New-Item -ItemType Directory -Force -Path $ResourcesDir | Out-Null
if ($CleanResources) {
    Remove-DirectorySafely $RuntimeOut
    Remove-DirectorySafely $ModelsOut
    Remove-DirectorySafely (Join-Path $ResourcesDir "portable-config")
}

Copy-BackendRuntime $RuntimeSource $RuntimeOut

if (-not $NoModels) {
    New-Item -ItemType Directory -Force -Path $ModelsOut | Out-Null
    foreach ($dir in $ModelDirs) {
        Copy-ModelDirectory (Resolve-RepoPath $dir) $ModelsOut
    }
    foreach ($file in $ModelFiles) {
        Copy-ModelFile (Resolve-RepoPath $file) $ModelsOut
    }
    if ($HfRepo.Trim() -and $HfFiles.Count -gt 0) {
        foreach ($file in $HfFiles) {
            Download-HuggingFaceFile $HfRepo $HfRevision $file $HfToken $ModelsOut
        }
    }
}

Write-BundledSettings $RuntimeOut $ModelsOut
Update-TauriResources

Write-Host ""
Write-Host "Staged llama.cpp runtime in: $RuntimeOut"
Write-Host "Staged models in:          $ModelsOut"
Write-Host "Updated Tauri resources:   src-tauri\tauri.conf.json"

if (-not $NoBuild) {
    Push-Location $RepoRoot
    try {
        npm run tauri:build
    } finally {
        Pop-Location
    }
}
