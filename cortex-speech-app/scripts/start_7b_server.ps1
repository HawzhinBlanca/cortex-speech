#!/usr/bin/env pwsh
# Cortex Speech - one-click start for the OmniASR-7B champion server (WSL).
#
#   powershell -ExecutionPolicy Bypass -File cortex-speech-app\scripts\start_7b_server.ps1
#
# Idempotent: if the server already answers on 127.0.0.1:8799 inside WSL, reports READY and exits.
# Otherwise launches the repo's cortex_7b_server.py inside WSL (nohup, survives this console) and
# waits for the port. Machine-specific locations are env-overridable:
#   CORTEX_7B_CHAMPION_POINTER  WSL path to the registry champion pointer (default: the app's
#                         %APPDATA%\cortex-speech\champion.json via wslpath — the SAME pointer the
#                         app regenerates on every start, so helper and app can never disagree)
#   CORTEX_7B_PYTHON      WSL path to the python with torch/fairseq2/peft/omnilingual-asr
#   CORTEX_7B_PORT        default 8799
#   CORTEX_7B_DEVICES     e.g. "0,1" — one model replica per GPU (default: every visible GPU).
#                         On this rig (2x 3090 Ti) the default loads both cards for ~2x throughput.
# NOTE: run from an INTERACTIVE console. The nohup-detach dies with a non-interactive runner's
# session (WSL kills the session's children when the launching wsl.exe exits) — a headless harness
# must instead hold `wsl -- bash -lc "... exec python cortex_7b_server.py"` alive itself.
$ErrorActionPreference = "Stop"

$port = if ($env:CORTEX_7B_PORT) { $env:CORTEX_7B_PORT } else { "8799" }
# 2026-08-20 external review: this helper used to pass an OBSOLETE CORTEX_7B_MODEL_DIR the server
# ignores entirely — the server loads ONLY the deployment named by CORTEX_7B_CHAMPION_POINTER and
# refuses to start without one, so the helper "worked" only on machines where the pointer happened
# to be exported by hand. Default to the app's own regenerated pointer.
$pointer = if ($env:CORTEX_7B_CHAMPION_POINTER) { $env:CORTEX_7B_CHAMPION_POINTER } else {
    $winPointer = Join-Path $env:APPDATA "cortex-speech\champion.json"
    if (-not (Test-Path $winPointer)) { Write-Error "no champion pointer at $winPointer (start the app once, or set CORTEX_7B_CHAMPION_POINTER)"; exit 2 }
    (wsl -- wslpath -a ($winPointer -replace '\', '/')).Trim()
}
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
Write-Host "  pointer:   $pointer   python: $wslPython   port: $port"

# nohup + detach so the server outlives this console; log to ~/cortex_7b_server.log in WSL.
$devEnv = if ($env:CORTEX_7B_DEVICES) { "CORTEX_7B_DEVICES='$($env:CORTEX_7B_DEVICES)' " } else { "" }
$launch = "${devEnv}CORTEX_7B_CHAMPION_POINTER='$pointer' CORTEX_7B_PORT=$port nohup '$wslPython' '$serverWsl' > ~/cortex_7b_server.log 2>&1 &"
wsl -- bash -lc $launch

# Two replicas stream the ~30 GB checkpoint twice, so allow for the multi-GPU default.
$deadline = (Get-Date).AddMinutes(15)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 10
    if ((Test-ServerUp) -match "UP") {
        Write-Host "READY: server answering on 127.0.0.1:$port. The app will now use the champion engine." -ForegroundColor Green
        exit 0
    }
    Write-Host "  ...still loading (log: wsl -- tail -5 ~/cortex_7b_server.log)"
}

Write-Host "FAILED: server did not come up within 15 minutes. Last log lines:" -ForegroundColor Red
wsl -- bash -c "tail -15 ~/cortex_7b_server.log 2>/dev/null"
exit 1
