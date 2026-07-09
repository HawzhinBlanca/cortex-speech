#!/usr/bin/env pwsh
# Cortex Speech - Windows entrypoint for the governance verification gate.
# Mirrors `make governance-proof` for environments without GNU make.
#
#   pwsh scripts/verify-10.ps1      # or:  powershell -File scripts/verify-10.ps1
#
# Exits non-zero (propagating verify_10.py's status) if any governance gate is red.
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$gate = Join-Path $repoRoot "scripts/verify_10.py"

$py = $null
foreach ($candidate in @("python", "python3", "py")) {
    if (Get-Command $candidate -ErrorAction SilentlyContinue) { $py = $candidate; break }
}
if (-not $py) {
    Write-Error "Python not found on PATH; install Python 3.x to run the verification gate."
    exit 2
}

& $py $gate
exit $LASTEXITCODE
