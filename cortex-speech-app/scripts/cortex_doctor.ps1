#!/usr/bin/env pwsh
# Cortex Speech - preflight doctor: is the app truly ready for real use RIGHT NOW on this machine?
#
#   powershell -ExecutionPolicy Bypass -File cortex-speech-app\scripts\cortex_doctor.ps1
#
# Read-only. Checks every prerequisite of the daily-use path and prints one PASS/FAIL line each,
# then a single verdict. Exit 0 = ready, 1 = not ready.
$ErrorActionPreference = "SilentlyContinue"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)   # scripts/ -> app/ -> repo root
$app = Join-Path $repo "cortex-speech-app"
$fails = @()

function Check($name, $ok, $detail) {
    if ($ok) { Write-Host ("  PASS  {0,-28} {1}" -f $name, $detail) -ForegroundColor Green }
    else { Write-Host ("  FAIL  {0,-28} {1}" -f $name, $detail) -ForegroundColor Red; $script:fails += $name }
}

Write-Host "CORTEX DOCTOR - real-use preflight ($(Get-Date -Format 'yyyy-MM-dd HH:mm'))"
Write-Host ("-" * 72)

# 1. Release exe present
$exe = Join-Path $app "src-tauri\target\release\cortex-speech-app.exe"
Check "release-exe" (Test-Path $exe) $exe

# 2. Exe compiled from current HEAD (freshness guard)
$py = (Get-Command python).Source
$fresh = & $py (Join-Path $app "scripts\check_exe_freshness.py") 2>&1
Check "exe-freshness" ($LASTEXITCODE -eq 0) (($fresh | Select-Object -Last 1))

# 3. Required local models
Check "model-silero-vad" (Test-Path (Join-Path $app "src-tauri\models\silero_vad_v4.onnx")) "VAD (segmentation)"
Check "model-omniasr-300m" (Test-Path (Join-Path $app "src-tauri\models\omniasr-ctc-300m\model.int8.onnx")) "offline fallback ASR"

# 4. 7B champion server answering inside WSL
$port = if ($env:CORTEX_7B_PORT) { $env:CORTEX_7B_PORT } else { "8799" }
$up = wsl -- bash -c "timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/$port' 2>/dev/null && echo UP || echo DOWN" 2>$null
Check "champion-7b-server" ($up -match "UP") "127.0.0.1:$port in WSL (if FAIL: run scripts\start_7b_server.ps1)"

# 5. Settings point at the champion
$settings = Join-Path $env:APPDATA "cortex-speech\settings.json"
$modelSize = ""
if (Test-Path $settings) {
    try { $modelSize = (Get-Content $settings -Raw | ConvertFrom-Json).asr_model_size } catch {}
}
Check "settings-champion" ($modelSize -eq "WSL7B") "asr_model_size=$modelSize ($settings)"

# 6. No stale single-instance lock or orphan app processes
$orphans = @(Get-Process cortex-speech-app -ErrorAction SilentlyContinue)
Check "no-orphan-app" ($orphans.Count -eq 0) "$($orphans.Count) cortex-speech-app.exe running (close before fresh launch)"

# 7. Disk space on the data drive (>= 5 GiB free)
$dataDrive = (Get-PSDrive -Name ($env:APPDATA)[0]).Free
Check "disk-space" ($dataDrive -gt 5GB) ("{0:N1} GiB free on {1}:" -f ($dataDrive / 1GB), ($env:APPDATA)[0])

# 8. Database present and non-trivial (first run: absent is fine)
$db = Join-Path $env:APPDATA "cortex-speech\cortex-speech.db"
if (Test-Path $db) {
    Check "database" $true ("{0:N1} MB at {1}" -f ((Get-Item $db).Length / 1MB), $db)
} else {
    Write-Host ("  INFO  {0,-28} {1}" -f "database", "no library yet (fresh profile) - the app will create it") -ForegroundColor Yellow
}

Write-Host ("-" * 72)
if ($fails.Count -eq 0) {
    Write-Host "VERDICT: READY for real use - every preflight check passed." -ForegroundColor Green
    exit 0
}
Write-Host ("VERDICT: NOT READY - {0} check(s) failed: {1}" -f $fails.Count, ($fails -join ", ")) -ForegroundColor Red
exit 1
