#!/usr/bin/env pwsh
# Cortex Speech - desktop launcher: full app initialization from one double-click.
#
# 1. If the OmniASR-7B champion server is not answering inside WSL, kick start_7b_server.ps1
#    in a hidden background process (the app's ask-dialog covers the loading window; by the
#    time an import runs, the engine is warm).
# 2. Launch the release exe (single-instance: a second click just surfaces the running app).
#
# Tracked script - no machine-specific paths; everything derives from this file's location.
$ErrorActionPreference = "SilentlyContinue"
$app = Split-Path -Parent $PSScriptRoot                     # scripts/ -> cortex-speech-app/
$exe = Join-Path $app "src-tauri\target\release\cortex-speech-app.exe"
$startServer = Join-Path $PSScriptRoot "start_7b_server.ps1"

if (-not (Test-Path $exe)) {
    # Loud failure beats a silent no-op on double-click.
    Add-Type -AssemblyName PresentationFramework
    [System.Windows.MessageBox]::Show(
        "Cortex exe not found:`n$exe`n`nBuild it with: make build-app", "Cortex Speech", "OK", "Error") | Out-Null
    exit 1
}

# Ensure the champion engine (idempotent; hidden; don't block the app launch).
$port = if ($env:CORTEX_7B_PORT) { $env:CORTEX_7B_PORT } else { "8799" }
$up = wsl -- bash -c "timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/$port' 2>/dev/null && echo UP || echo DOWN" 2>$null
if ($up -notmatch "UP") {
    Start-Process powershell -WindowStyle Hidden -ArgumentList @(
        "-ExecutionPolicy", "Bypass", "-File", "`"$startServer`""
    ) | Out-Null
}

Start-Process -FilePath $exe -WorkingDirectory (Split-Path -Parent $exe)
exit 0
