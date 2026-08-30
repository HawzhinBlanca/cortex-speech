# Measure branch coverage for the robustness loop, and record WHICH COMMIT was measured.
#
# The scoreboard refuses to score a report it cannot tie to a commit, because a stale report is
# worse than no report: it names targets whose tests are already written and lets an iteration
# claim progress it did not make. So measurement and identity are written together, here, or not
# at all.
#
# Runs in the ISOLATED worktree (default cortex-scrub) with the pinned toolchain and the exact
# flags verify_10 uses, so the number this produces is comparable to the CI gate's number.
#
#   powershell -ExecutionPolicy Bypass -File scripts/robustness_measure.ps1 [-Worktree <path>]

param(
    [string]$Worktree = ''
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($Worktree)) {
    $Worktree = Split-Path (Resolve-Path (Join-Path $PSScriptRoot '..')) -Parent
}
$app = Join-Path $Worktree 'cortex-speech-app'
$srcTauri = Join-Path $app 'src-tauri'
$outDir = Join-Path $app 'logs\robustness'
$out = Join-Path $outDir 'coverage-latest.json'
$meta = Join-Path $outDir 'coverage-latest.meta.json'

if (-not (Test-Path $srcTauri)) { throw "measuring worktree not found: $srcTauri" }
New-Item -ItemType Directory -Force $outDir | Out-Null

# The commit is read BEFORE the run and the tree must be clean: measuring a dirty tree produces a
# number that belongs to no commit at all, which is precisely the unattributable claim to avoid.
$dirty = & git -C $Worktree status --porcelain
if ($dirty) { throw "measuring worktree is dirty - commit or stash first, so the number belongs to a commit:`n$dirty" }
$sha = (& git -C $Worktree rev-parse HEAD).Trim()
$branch = (& git -C $Worktree branch --show-current).Trim()

$contract = Get-Content (Join-Path $app 'scripts\rust_coverage_toolchain.json') -Raw | ConvertFrom-Json
$toolchain = $contract.toolchain
Write-Output "measuring $($sha.Substring(0,8)) [$branch] with $toolchain ..."

# CARGO_INCREMENTAL=0, matching the CI job's environment exactly. Two reasons, both learned the
# hard way on 2026-08-29: incremental state ICE'd the pinned nightly on a bin target
# ("internal compiler error[E0618]: expected function" on a plain std::env::var_os call), killing
# two consecutive 35-minute measurements and surviving `llvm-cov clean --workspace`; and a number
# measured under different flags than the gate uses is not comparable to the gate's verdict, which
# is the only thing this measurement exists to predict.
# CARGO_BUILD_JOBS caps LINK PARALLELISM, which is what actually kills this measurement:
#
#   LINK : fatal error LNK1102: out of memory
#
# `--all-targets` links ~20 binaries (bins, benches, integration tests), each pulling the entire
# dependency graph with /DEBUG. Cargo defaults to one job per hardware thread, and this box has 64 --
# so a from-clean run fans out dozens of multi-gigabyte link.exe processes at once and exhausts
# memory no matter how much is installed. Incremental builds hid it by relinking only a few targets;
# the first `llvm-cov clean` is what exposed it. (The earlier "internal compiler error" on a trivial
# std::env::var_os call was the same pressure wearing a different hat -- rustc ICEs under allocation
# failure. CARGO_INCREMENTAL=0 was kept because it matches the CI job's environment, which is what
# makes this number comparable to the gate's, but it was NOT the cause and did not fix it.)
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '6'
Push-Location $srcTauri
try {
    & cargo "+$toolchain" llvm-cov --locked --all-targets --all-features --branch --json --output-path $out
    $code = $LASTEXITCODE
} finally {
    Pop-Location
    Remove-Item Env:\CARGO_INCREMENTAL -ErrorAction SilentlyContinue
    Remove-Item Env:\CARGO_BUILD_JOBS -ErrorAction SilentlyContinue
}
if ($code -ne 0) { throw "cargo llvm-cov failed with exit $code - no measurement written" }
if (-not (Test-Path $out)) { throw 'cargo llvm-cov reported success but wrote no report' }

# Identity is written only AFTER a successful run, so a failed measurement can never leave a
# sidecar blessing a report that does not exist or predates it.
@{
    schema         = 1
    measuredFromSha = $sha
    measuredBranch = $branch
    measuredIn     = $Worktree
    measuredAtUtc  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    toolchain      = $toolchain
} | ConvertTo-Json | Set-Content -Path $meta -Encoding utf8

Write-Output "wrote $out"
Write-Output "wrote $meta ($($sha.Substring(0,8)) [$branch])"
