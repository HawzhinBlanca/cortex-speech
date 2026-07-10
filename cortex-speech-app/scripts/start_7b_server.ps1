#!/usr/bin/env pwsh
# Cortex Speech - one-click start for the OmniASR-7B champion server (WSL).
#
#   powershell -ExecutionPolicy Bypass -File cortex-speech-app\scripts\start_7b_server.ps1
#
# Idempotent: if the server already answers on 127.0.0.1:8799 inside WSL, reports READY and exits.
# Otherwise launches the repo's cortex_7b_server.py inside WSL (nohup, survives this console) and
# waits for the port. Machine-specific locations are env-overridable:
#   CORTEX_7B_MODEL_DIR   WSL path to the champion model dir (LoRA + tokenizer [+ base .pt])
#   CORTEX_7B_PYTHON      WSL path to the python with torch/fairseq2/peft/omnilingual-asr
#   CORTEX_7B_PORT        default 8799
$ErrorActionPreference = "Stop"

$port = if ($env:CORTEX_7B_PORT) { $env:CORTEX_7B_PORT } else { "8799" }
$modelDir = if ($env:CORTEX_7B_MODEL_DIR) { $env:CORTEX_7B_MODEL_DIR } else { "/home/ai/cortex_champion_model" }
$wslPython = if ($env:CORTEX_7B_PYTHON) { $env:CORTEX_7B_PYTHON } else { "/home/ai/.venv-wsl-whisper/bin/python" }

function Test-ServerUp {
    # Probe the loopback port INSIDE WSL's network namespace (not reachable from Windows).
    wsl -- bash -c "timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/$port' 2>/dev/null && echo UP || echo DOWN" 2>$null
}

if ((Test-ServerUp) -match "UP") {
    Write-Host "READY: OmniASR-7B server already answering on 127.0.0.1:$port (inside WSL)." -ForegroundColor Green
    exit 0
}

# Resolve the repo's committed server script and convert to a WSL path.
$serverWin = Join-Path $PSScriptRoot "cortex_7b_server.py"
if (-not (Test-Path $serverWin)) { Write-Error "server script not found: $serverWin"; exit 2 }
$serverWsl = (wsl -- wslpath -a ($serverWin -replace '\\', '/')).Trim()

Write-Host "Starting OmniASR-7B champion server (loads ~30 GB base + Kurdish LoRA, takes 1-5 min)..."
Write-Host "  script:    $serverWsl"
Write-Host "  model dir: $modelDir   python: $wslPython   port: $port"

# nohup + detach so the server outlives this console; log to ~/cortex_7b_server.log in WSL.
$launch = "CORTEX_7B_MODEL_DIR='$modelDir' CORTEX_7B_PORT=$port nohup '$wslPython' '$serverWsl' > ~/cortex_7b_server.log 2>&1 &"
wsl -- bash -lc $launch

$deadline = (Get-Date).AddMinutes(8)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 10
    if ((Test-ServerUp) -match "UP") {
        Write-Host "READY: server answering on 127.0.0.1:$port. The app will now use the champion engine." -ForegroundColor Green
        exit 0
    }
    Write-Host "  ...still loading (log: wsl -- tail -5 ~/cortex_7b_server.log)"
}

Write-Host "FAILED: server did not come up within 8 minutes. Last log lines:" -ForegroundColor Red
wsl -- bash -c "tail -15 ~/cortex_7b_server.log 2>/dev/null"
exit 1
