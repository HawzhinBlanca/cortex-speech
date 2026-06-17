# Run real-media tests against a fixture directory supplied by -RealAudioDir
# or CORTEX_REAL_AUDIO_DIR.
param(
    [string]$RealAudioDir = "",
    [string]$UserPodcastFile = "",
    [switch]$Integration,
    [switch]$Mp4Only,
    [switch]$SkipUnitTests,
    [switch]$UserPodcastTest,
    [switch]$AllIgnored,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$SupportedAudioExtensions = @(".wav", ".mp3", ".flac", ".m4a", ".ogg", ".aac", ".opus", ".mp4", ".mov", ".webm", ".wma")
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Tauri = Join-Path $Root "src-tauri"

function Remove-TemporaryFixtureDir {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $resolvedPath = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    $resolvedTemp = (Resolve-Path -LiteralPath $env:TEMP -ErrorAction Stop).Path

    $normalizedPath = $resolvedPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $normalizedTemp = $resolvedTemp.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $tempPrefix = $normalizedTemp + [System.IO.Path]::DirectorySeparatorChar

    if ($normalizedPath.Equals($normalizedTemp, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not $normalizedPath.StartsWith($tempPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        Write-Error "Refusing to remove temporary fixture directory outside TEMP: $resolvedPath"
    }

    Remove-Item -LiteralPath $resolvedPath -Recurse -Force
}

if ([string]::IsNullOrWhiteSpace($RealAudioDir)) {
    $RealAudioDir = $env:CORTEX_REAL_AUDIO_DIR
}

if ([string]::IsNullOrWhiteSpace($UserPodcastFile)) {
    $UserPodcastFile = $env:CORTEX_USER_PODCAST_FILE
}

$runFixtureTests = -not $SkipUnitTests
$runIntegration = [bool]$Integration
$runUserPodcastTest = [bool]$UserPodcastTest -or -not [string]::IsNullOrWhiteSpace($UserPodcastFile)

if (-not $runFixtureTests -and -not $runIntegration -and -not $runUserPodcastTest) {
    Write-Error "No real-data test phase selected. Remove -SkipUnitTests, add -Integration, or pass -UserPodcastFile."
}

if ($runUserPodcastTest) {
    if ([string]::IsNullOrWhiteSpace($UserPodcastFile)) {
        Write-Error "User podcast test requested but no file was configured. Pass -UserPodcastFile <audio-file> or set CORTEX_USER_PODCAST_FILE."
    }
    if (-not (Test-Path -LiteralPath $UserPodcastFile -PathType Leaf)) {
        Write-Error "User podcast file not found: $UserPodcastFile"
    }
    $UserPodcastFile = (Resolve-Path -LiteralPath $UserPodcastFile -ErrorAction Stop).Path
    $userPodcastExtension = [System.IO.Path]::GetExtension($UserPodcastFile).ToLowerInvariant()
    if ($SupportedAudioExtensions -notcontains $userPodcastExtension) {
        Write-Error "User podcast file is not a supported audio file: $UserPodcastFile"
    }
}

if ($runFixtureTests -or $runIntegration) {
    if ([string]::IsNullOrWhiteSpace($RealAudioDir)) {
        Write-Error "Real audio directory not configured. Pass -RealAudioDir <directory> or set CORTEX_REAL_AUDIO_DIR."
    }

    if (-not (Test-Path -LiteralPath $RealAudioDir)) {
        Write-Error "Real audio directory not found: $RealAudioDir"
    }

    $RealAudioDir = (Resolve-Path -LiteralPath $RealAudioDir -ErrorAction Stop).Path
    if (-not (Test-Path -LiteralPath $RealAudioDir -PathType Container)) {
        Write-Error "Real audio path is not a directory: $RealAudioDir"
    }

    $hasSupportedAudio = $false
    foreach ($file in Get-ChildItem -LiteralPath $RealAudioDir -File -Recurse) {
        if ($SupportedAudioExtensions -contains $file.Extension.ToLowerInvariant()) {
            $hasSupportedAudio = $true
            break
        }
    }
    if (-not $hasSupportedAudio) {
        Write-Error "Real audio directory contains no supported audio files: $RealAudioDir"
    }
}

if ($runFixtureTests -or $runIntegration) {
    $env:CORTEX_REAL_AUDIO_DIR = $RealAudioDir
    Write-Host "==> CORTEX_REAL_AUDIO_DIR=$RealAudioDir" -ForegroundColor Cyan
}
if ($runUserPodcastTest) {
    $env:CORTEX_USER_PODCAST_FILE = $UserPodcastFile
    Write-Host "==> CORTEX_USER_PODCAST_FILE=$UserPodcastFile" -ForegroundColor Cyan
}

$mp4FixtureDir = $null
Push-Location $Tauri
try {
    if ($runFixtureTests) {
        if ($AllIgnored) {
            Write-Host "`n==> cargo test -- --ignored (all ignored tests)" -ForegroundColor Cyan
            cargo test -- --ignored --nocapture
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        } else {
            Write-Host "`n==> cargo test --test real_audio -- --ignored --nocapture" -ForegroundColor Cyan
            cargo test --test real_audio -- --ignored --nocapture
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        }
    }

    if ($runUserPodcastTest) {
        Write-Host "`n==> cargo test --test user_data_test -- --nocapture" -ForegroundColor Cyan
        cargo test --test user_data_test -- --nocapture
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }

    if ($runIntegration) {
        $fixtureDir = $RealAudioDir
        if ($Mp4Only) {
            $fixtureDir = Join-Path $env:TEMP "cortex-integration-mp4-$PID"
            $mp4FixtureDir = $fixtureDir
            Remove-TemporaryFixtureDir -Path $fixtureDir
            New-Item -ItemType Directory -Path $fixtureDir | Out-Null
            Get-ChildItem -LiteralPath $RealAudioDir -File -Filter "*.mp4" | ForEach-Object {
                Copy-Item $_.FullName -Destination $fixtureDir
            }
            $count = (Get-ChildItem -LiteralPath $fixtureDir -Filter "*.mp4").Count
            Write-Host "`n==> Mp4Only: copied $count MP4 files from root (no subfolders/FLAC)" -ForegroundColor Yellow
            if ($count -eq 0) { Write-Error "No MP4 files at $RealAudioDir root" }
        }

        Write-Host "`n==> Tauri integration (import -> export -> validate)" -ForegroundColor Cyan
        Write-Host "    Fixture: $fixtureDir" -ForegroundColor DarkGray
        if (-not $Mp4Only) {
            Write-Host "    WARNING: full real-audio integration can be slow for large fixture directories." -ForegroundColor DarkYellow
        }

        if (-not $SkipBuild) {
            Write-Host "`n==> cargo build --release --bin cortex-speech-app" -ForegroundColor Cyan
            cargo build --release --bin cortex-speech-app
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        }

        $env:CORTEX_INTEGRATION_TEST = "1"
        $env:CORTEX_INTEGRATION_FIXTURE = $fixtureDir
        Remove-Item Env:CORTEX_SMOKE_TEST -ErrorAction SilentlyContinue

        cargo run --release --bin cortex-speech-app
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
} finally {
    if ($mp4FixtureDir -and (Test-Path -LiteralPath $mp4FixtureDir)) {
        Remove-TemporaryFixtureDir -Path $mp4FixtureDir
    }
    Pop-Location
}

Write-Host "`nAll requested real-data tests finished." -ForegroundColor Green
