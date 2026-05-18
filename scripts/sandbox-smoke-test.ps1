$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Drop = Join-Path $env:USERPROFILE "Desktop\vm-test-drop"
$LogPath = Join-Path $Drop "sandbox-smoke-result.txt"

function Write-Log([string]$Message) {
    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $Message
    Add-Content -LiteralPath $LogPath -Value $line
    Write-Host $line
}

function Find-InstalledApp {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "Programs\Voice Keyboard\Voice Keyboard.exe"),
        (Join-Path $env:ProgramFiles "Voice Keyboard\Voice Keyboard.exe"),
        (Join-Path ${env:ProgramFiles(x86)} "Voice Keyboard\Voice Keyboard.exe")
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return $candidate
        }
    }

    $roots = @($env:LOCALAPPDATA, $env:ProgramFiles, ${env:ProgramFiles(x86)}) |
        Where-Object { $_ -and (Test-Path -LiteralPath $_) }
    foreach ($root in $roots) {
        $found = Get-ChildItem -LiteralPath $root -Recurse -File -Filter "Voice Keyboard.exe" -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($found) {
            return $found.FullName
        }
    }
    return $null
}

function Wait-HttpOk([string]$Url, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $response = Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 2
            if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
                return $true
            }
        } catch {
            Start-Sleep -Seconds 1
        }
    }
    return $false
}

Set-Content -LiteralPath $LogPath -Value "Voice Keyboard Windows Sandbox smoke test"
Write-Log "Drop folder: $Drop"

if (-not (Test-Path -LiteralPath $Drop)) {
    throw "Mapped drop folder not found: $Drop"
}

$installer = Get-ChildItem -LiteralPath $Drop -File -Filter "*setup.exe" | Select-Object -First 1
if (-not $installer) {
    throw "Installer not found in $Drop"
}
Write-Log "Installer: $($installer.FullName)"

$model = Get-ChildItem -LiteralPath $Drop -File -Filter "*.gguf" |
    Where-Object { $_.Name -notmatch "mmproj" } |
    Sort-Object Length -Descending |
    Select-Object -First 1
$mmproj = Get-ChildItem -LiteralPath $Drop -File -Filter "*.gguf" |
    Where-Object { $_.Name -match "mmproj" } |
    Sort-Object Length -Descending |
    Select-Object -First 1

if (-not $model) {
    throw "Main GGUF model not found in $Drop"
}
Write-Log "Main model: $($model.Name) ($([math]::Round($model.Length / 1GB, 2)) GB)"
if ($mmproj) {
    Write-Log "MM projector: $($mmproj.Name) ($([math]::Round($mmproj.Length / 1GB, 2)) GB)"
} else {
    Write-Log "MM projector: not supplied"
}

Write-Log "Running silent installer..."
$install = Start-Process -FilePath $installer.FullName -ArgumentList "/S" -Wait -PassThru
Write-Log "Installer exit code: $($install.ExitCode)"
if ($install.ExitCode -ne 0) {
    throw "Installer failed with exit code $($install.ExitCode)"
}

Start-Sleep -Seconds 5
$appExe = Find-InstalledApp
if (-not $appExe) {
    throw "Installed Voice Keyboard.exe not found"
}
Write-Log "Installed app: $appExe"

$installDir = Split-Path $appExe -Parent
$runtimeCandidates = @(
    (Join-Path $installDir "resources\runtime\llama-server.exe"),
    (Join-Path (Split-Path $installDir -Parent) "resources\runtime\llama-server.exe")
)
$llamaServer = $runtimeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $llamaServer) {
    throw "Bundled llama-server.exe not found near install dir"
}
Write-Log "Bundled backend: $llamaServer"

$modelsDir = Join-Path $env:APPDATA "CppGemma\VoiceKeyboard\config\models"
New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
Write-Log "Copying GGUF files into app models folder: $modelsDir"
Copy-Item -LiteralPath $model.FullName -Destination (Join-Path $modelsDir $model.Name) -Force
if ($mmproj) {
    Copy-Item -LiteralPath $mmproj.FullName -Destination (Join-Path $modelsDir $mmproj.Name) -Force
}

$modelPath = Join-Path $modelsDir $model.Name
$mmprojPath = if ($mmproj) { Join-Path $modelsDir $mmproj.Name } else { "" }
$deviceOutput = & $llamaServer --list-devices 2>&1
$device = $deviceOutput |
    ForEach-Object { if ($_ -match '^\s*([^:\s]+):\s+') { $Matches[1] } } |
    Select-Object -First 1
$deviceArgs = if ($device) { @("--device", $device) } else { @() }
if ($device) {
    Write-Log "Using llama.cpp device: $device"
} else {
    Write-Log "No llama.cpp GPU device detected; smoke test will run CPU-only and may be slow."
}

$args = @(
    "-m", $modelPath,
    "--host", "127.0.0.1",
    "--port", "8099",
    $deviceArgs,
    "-ngl", "all",
    "--fit", "on",
    "--fit-target", "384",
    "-c", "2048",
    "--parallel", "1",
    "--no-cache-idle-slots",
    "--jinja",
    "--flash-attn", "on",
    "--temp", "0",
    "--reasoning", "off",
    "--image-min-tokens", "70",
    "--image-max-tokens", "70",
    "--metrics",
    "--no-webui"
)
if ($mmprojPath) {
    $args = @("-m", $modelPath, "--mmproj", $mmprojPath) + $args[2..($args.Count - 1)]
}

$serverOut = Join-Path $Drop "sandbox-llama-server.out.log"
$serverErr = Join-Path $Drop "sandbox-llama-server.err.log"
Write-Log "Starting bundled llama-server for health check..."
$server = Start-Process -FilePath $llamaServer -ArgumentList $args -RedirectStandardOutput $serverOut -RedirectStandardError $serverErr -PassThru -WindowStyle Hidden
Write-Log "llama-server pid: $($server.Id)"

try {
    if (Wait-HttpOk "http://127.0.0.1:8099/health" 180) {
        Write-Log "PASS: llama-server /health returned OK"
    } else {
        Write-Log "FAIL: llama-server /health did not become ready"
        if (Test-Path -LiteralPath $serverErr) {
            Write-Log "Last stderr lines:"
            Get-Content -LiteralPath $serverErr -Tail 40 | ForEach-Object { Write-Log "stderr: $_" }
        }
        throw "llama-server health check failed"
    }
} finally {
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        Write-Log "Stopped llama-server"
    }
}

Write-Log "Starting installed app briefly..."
$app = Start-Process -FilePath $appExe -PassThru
Start-Sleep -Seconds 8
if ($app.HasExited) {
    throw "Installed app exited early with code $($app.ExitCode)"
}
Write-Log "PASS: installed app process started"
Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue

Write-Log "SMOKE TEST PASS"
