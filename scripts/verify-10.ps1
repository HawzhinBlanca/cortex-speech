#!/usr/bin/env pwsh
# Cortex Speech - Windows entrypoint for the verify-10 aggregator.
# Mirrors `make verify-10` for environments without GNU make; arguments pass through.
#
#   pwsh scripts/verify-10.ps1              # full personal-use aggregator
#   pwsh scripts/verify-10.ps1 --quick      # tiers 0-1 only
#   pwsh scripts/verify-10.ps1 --static     # historical governance gate (CI contract)
#
# Exits non-zero (propagating verify_10.py's status) if any kept gate is red or incomplete.
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

& $py $gate @args
exit $LASTEXITCODE
