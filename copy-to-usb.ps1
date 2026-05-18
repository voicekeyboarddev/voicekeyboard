# Copy project to USB stick, excluding build artifacts and dependencies
# Usage: .\copy-to-usb.ps1

$source = "g:\Ash\VoiceKeyboard\CppGemma\project-source"
$destination = "I:\CppGemma\new"

Write-Host "VoiceKeyboard Project Transfer to USB" -ForegroundColor Cyan
Write-Host "=====================================" -ForegroundColor Cyan
Write-Host "Source: $source"
Write-Host "Destination: $destination"
Write-Host ""

# Create destination directory
New-Item -ItemType Directory -Path $destination -Force | Out-Null

# Function to robocopy files
function Copy-ProjectFiles {
    param(
        [string]$src,
        [string]$dst,
        [string]$include,
        [string]$exclude
    )
    
    $robocopyArgs = @(
        $src,
        $dst,
        $include,
        "/S",           # Copy subdirectories
        "/R:1",         # Retry once
        "/W:1",         # Wait 1 second
        "/NP",          # No progress percentage
        "/XD"           # Exclude directories
    )
    
    if ($exclude) {
        $robocopyArgs += $exclude.Split(" ")
    }
    
    robocopy @robocopyArgs
}

# Exclude patterns for robocopy
$excludeDirs = "node_modules target .git .vscode dist build .next .nuxt .cache"

Write-Host "Copying source code and configuration files..." -ForegroundColor Green

# Copy everything first, then remove excluded directories
robocopy $source $destination /S /R:1 /W:1 /NP `
  /XD node_modules target ".git" ".vscode" dist build ".next" ".nuxt" ".cache" "\.git" `
  | Out-Null

# Remove build artifacts that might have been copied
$dirsToRemove = @("node_modules", "target", ".git", ".vscode", "dist", "build", ".cache")
foreach ($dir in $dirsToRemove) {
    $path = Join-Path $destination $dir
    if (Test-Path $path) {
        Write-Host "Removing $dir..." -ForegroundColor Yellow
        Remove-Item $path -Recurse -Force
    }
}

# Calculate size
$sizeMB = ((Get-ChildItem $destination -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB) -as [int]

Write-Host ""
Write-Host "✓ Copy complete!" -ForegroundColor Green
Write-Host "Location: $destination"
Write-Host "Size: ~$sizeMB MB"
Write-Host ""
Write-Host "Files copied:" -ForegroundColor Cyan
Get-ChildItem $destination -Force | ForEach-Object {
    if ($_.PSIsContainer) {
        Write-Host "  📁 $($_.Name)/"
    } else {
        Write-Host "  📄 $($_.Name)"
    }
}

Write-Host ""
Write-Host "Next steps on the other PC:" -ForegroundColor Cyan
Write-Host "1. npm install"
Write-Host "2. cd src-tauri && cargo build --release"
Write-Host "3. cd .. && npm run tauri build"
