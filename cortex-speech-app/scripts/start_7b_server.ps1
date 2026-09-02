#!/usr/bin/env pwsh
# Cortex Speech - one-click start for the OmniASR-7B champion server (WSL).
#
#   powershell -ExecutionPolicy Bypass -File cortex-speech-app\scripts\start_7b_server.ps1
#
# Idempotent: if the exact pointer-selected Cortex champion answers an identity-bound health request, reports READY
# and exits. Otherwise launches the repo's cortex_7b_server.py behind a heartbeat guard in a hidden,
# independent wsl.exe process and waits for pointer-bound readiness. Machine-specific locations are
# env-overridable:
#   CORTEX_7B_CHAMPION_POINTER  WSL path to the registry champion pointer (default: the app's
#                         %APPDATA%\cortex-speech\champion.json via wslpath — the SAME pointer the
#                         app regenerates on every start, so helper and app can never disagree)
#   CORTEX_7B_PYTHON      WSL path to the python with torch/fairseq2/peft/omnilingual-asr
#   CORTEX_7B_PORT        default 8799
#   CORTEX_7B_DEVICES     e.g. "0,1" — one model replica per GPU (default: every visible GPU).
#                         On this rig (2x 3090 Ti) the default loads both cards for ~2x throughput.
# The dedicated wsl.exe remains alive for the lifetime of the server. Before READY, the in-WSL guard
# also requires launcher heartbeats, because Windows PowerShell 5.1 can bypass finally on Ctrl+C.
$ErrorActionPreference = "Stop"

$rawPort = if ($env:CORTEX_7B_PORT) { $env:CORTEX_7B_PORT } else { "8799" }
$portNumber = 0
if (-not [int]::TryParse($rawPort, [ref]$portNumber) -or $portNumber -lt 1 -or $portNumber -gt 65535) {
    [Console]::Error.WriteLine("CORTEX_7B_PORT must be an integer from 1 through 65535")
    exit 2
}
$port = $portNumber.ToString([Globalization.CultureInfo]::InvariantCulture)
$wslPython = if ($env:CORTEX_7B_PYTHON) { $env:CORTEX_7B_PYTHON } else { "/home/ai/.venv-wsl-whisper/bin/python" }

function ConvertTo-BashLiteral([string]$Value) {
    # A single-quoted Bash literal escapes an embedded apostrophe as: '"'"'
    return "'" + $Value.Replace("'", "'`"'`"'") + "'"
}

function Invoke-WslBashProgram([string]$Program, [int]$TimeoutSeconds) {
    # Native invocations can otherwise wait forever when WSL or a probe wedges. Keep the exact
    # Process object, bound its lifetime and output, and never search for unrelated WSL processes.
    if ($TimeoutSeconds -lt 1 -or $TimeoutSeconds -gt 300) {
        throw "WSL operation timeout must be between 1 and 300 seconds"
    }

    $encodedProgram = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Program))
    $bootstrap = "printf %s $encodedProgram | /usr/bin/base64 -d | /bin/bash --noprofile --norc"
    $bootstrapArgument = '"' + $bootstrap + '"'
    $stdoutPath = [IO.Path]::GetTempFileName()
    $stderrPath = [IO.Path]::GetTempFileName()
    $process = $null
    try {
        $process = Start-Process -FilePath "wsl.exe" -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -ArgumentList @(
                "--", "/usr/bin/env", "-u", "BASH_ENV", "-u", "ENV",
                "/bin/bash", "--noprofile", "--norc", "-c", $bootstrapArgument
            )
        # Windows PowerShell 5.1 resolves Process handles lazily. Pin it before a fast child exits;
        # otherwise HasExited can be true while ExitCode is silently $null.
        $null = $process.Handle
        if (-not $process.WaitForExit([int]($TimeoutSeconds * 1000))) {
            Stop-Process -InputObject $process -Force -ErrorAction SilentlyContinue
            if (-not $process.WaitForExit(5000)) {
                throw "WSL operation exceeded $TimeoutSeconds seconds and did not confirm termination"
            }
            throw "WSL operation exceeded $TimeoutSeconds seconds"
        }
        # The parameterless wait drains asynchronous file redirection before the files are read.
        $process.WaitForExit()
        $process.Refresh()
        $exitCode = $process.ExitCode
        $maximumOutputBytes = 2 * 1024 * 1024
        foreach ($outputPath in @($stdoutPath, $stderrPath)) {
            if ((Get-Item -LiteralPath $outputPath).Length -gt $maximumOutputBytes) {
                throw "WSL operation output exceeded $maximumOutputBytes bytes"
            }
        }
        return [PSCustomObject]@{
            ExitCode = $exitCode
            Stdout = [IO.File]::ReadAllText($stdoutPath)
            Stderr = [IO.File]::ReadAllText($stderrPath)
        }
    }
    finally {
        if ($null -ne $process) {
            $process.Dispose()
        }
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }
}

function ConvertTo-WslPath([string]$WindowsPath) {
    $normalizedPath = $WindowsPath -replace '\\', '/'
    $result = Invoke-WslBashProgram -Program "exec /usr/bin/wslpath -a $(ConvertTo-BashLiteral $normalizedPath)" -TimeoutSeconds 15
    $converted = $result.Stdout.Trim()
    if ($result.ExitCode -ne 0 -or -not $converted) {
        throw "could not convert a required Windows path for WSL"
    }
    return $converted
}

# 2026-08-20 external review: this helper used to pass an OBSOLETE CORTEX_7B_MODEL_DIR the server
# ignores entirely — the server loads ONLY the deployment named by CORTEX_7B_CHAMPION_POINTER and
# refuses to start without one, so the helper "worked" only on machines where the pointer happened
# to be exported by hand. Default to the app's own regenerated pointer.
$pointer = if ($env:CORTEX_7B_CHAMPION_POINTER) { $env:CORTEX_7B_CHAMPION_POINTER } else {
    $winPointer = Join-Path $env:APPDATA "cortex-speech\champion.json"
    if (-not (Test-Path $winPointer)) { [Console]::Error.WriteLine("no champion pointer in the app data directory (start the app once, or set CORTEX_7B_CHAMPION_POINTER)"); exit 2 }
    ConvertTo-WslPath $winPointer
}

# Resolve the repo's committed server/client/launch-guard scripts and convert them to WSL paths.
$serverWin = Join-Path $PSScriptRoot "cortex_7b_server.py"
$clientWin = Join-Path $PSScriptRoot "cortex_7b_client.py"
$guardWin = Join-Path $PSScriptRoot "cortex_7b_launch_guard.py"
if (-not (Test-Path $serverWin)) { [Console]::Error.WriteLine("the repository champion server script is missing"); exit 2 }
if (-not (Test-Path $clientWin)) { [Console]::Error.WriteLine("the repository champion client script is missing"); exit 2 }
if (-not (Test-Path $guardWin)) { [Console]::Error.WriteLine("the repository champion launch guard is missing"); exit 2 }
$serverWsl = ConvertTo-WslPath $serverWin
$clientWsl = ConvertTo-WslPath $clientWin
$guardWsl = ConvertTo-WslPath $guardWin

function Test-ServerReady {
    # An open port is not authority: another process could own it. The committed client validates
    # protocol, family, language, deployment/component digests, provenance and worker identity.
    try {
        $healthEnvironment = @(
            ConvertTo-BashLiteral "CORTEX_7B_HOST=127.0.0.1"
            ConvertTo-BashLiteral "CORTEX_7B_PORT=$port"
            ConvertTo-BashLiteral "CORTEX_7B_HEALTH_TIMEOUT_SECONDS=5"
        )
        $healthProgram = "exec /usr/bin/env $($healthEnvironment -join ' ') $(ConvertTo-BashLiteral $wslPython) $(ConvertTo-BashLiteral $clientWsl) --health --expected-pointer $(ConvertTo-BashLiteral $pointer)"
        $result = Invoke-WslBashProgram -Program $healthProgram -TimeoutSeconds 15
        return ($result.ExitCode -eq 0 -and ($result.Stdout -match "(?m)^__HEALTH__="))
    }
    catch {
        return $false
    }
}

if (Test-ServerReady) {
    Write-Host "READY: exact OmniASR-7B champion already healthy on 127.0.0.1:$port." -ForegroundColor Green
    exit 0
}

$launchMutex = New-Object System.Threading.Mutex($false, "Local\CortexSpeechChampionLaunch-$port")
$ownsLaunchMutex = $false
try {
    $followTimer = [Diagnostics.Stopwatch]::StartNew()
    $waitingAnnounced = $false
    while (-not $ownsLaunchMutex) {
        try {
            $ownsLaunchMutex = $launchMutex.WaitOne(0)
        }
        catch [System.Threading.AbandonedMutexException] {
            # The kernel grants this thread ownership when the previous launcher died.
            $ownsLaunchMutex = $true
        }
        if ($ownsLaunchMutex) { break }
        if (-not $waitingAnnounced) {
            Write-Host "Another champion launch is already in progress; waiting for its exact pointer-bound service..."
            $waitingAnnounced = $true
        }
        Start-Sleep -Seconds 5
        if (Test-ServerReady) {
            Write-Host "READY: concurrent launch produced the exact champion on 127.0.0.1:$port." -ForegroundColor Green
            exit 0
        }
        if ($followTimer.Elapsed -ge [TimeSpan]::FromMinutes(15)) {
            Write-Host "FAILED: concurrent champion launch did not become ready within 15 minutes." -ForegroundColor Red
            exit 1
        }
    }

    # Close the check-then-start race: the previous mutex owner may have completed immediately
    # before releasing ownership.
    if (Test-ServerReady) {
        Write-Host "READY: exact OmniASR-7B champion became healthy before a new launch was needed." -ForegroundColor Green
        exit 0
    }

    Write-Host "Starting OmniASR-7B champion server (loads ~30 GB base + Kurdish LoRA, takes 1-5 min)..."
    Write-Host "  source:    repository server + current app champion pointer   port: $port"

    # Encode the Bash program before crossing PowerShell -> CreateProcess -> wsl.exe -> Bash. This keeps
    # spaces and apostrophes in machine-specific paths data, never shell syntax. The new wsl.exe owns the
    # guard as its foreground process; the guard owns the exact server process group.
    $launchToken = [Guid]::NewGuid().ToString("N")
    $environment = @(
        ConvertTo-BashLiteral "PYTHONUNBUFFERED=1"
        ConvertTo-BashLiteral "CORTEX_7B_CHAMPION_POINTER=$pointer"
        ConvertTo-BashLiteral "CORTEX_7B_PORT=$port"
    )
    if ($env:CORTEX_7B_DEVICES) {
        $environment += ConvertTo-BashLiteral "CORTEX_7B_DEVICES=$($env:CORTEX_7B_DEVICES)"
    }
    $launch = "exec /usr/bin/env $($environment -join ' ') $(ConvertTo-BashLiteral $wslPython) $(ConvertTo-BashLiteral $guardWsl) supervise --token $launchToken --heartbeat-timeout 45 -- $(ConvertTo-BashLiteral $wslPython) $(ConvertTo-BashLiteral $serverWsl)"
    $encodedLaunch = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($launch))
    $bootstrap = "printf %s $encodedLaunch | /usr/bin/base64 -d | /bin/bash --noprofile --norc"
    $bootstrapArgument = '"' + $bootstrap + '"'

    function Show-LaunchLog {
        try {
            $tailProgram = "exec $(ConvertTo-BashLiteral $wslPython) $(ConvertTo-BashLiteral $guardWsl) tail --token $(ConvertTo-BashLiteral $launchToken) --lines 15"
            $result = Invoke-WslBashProgram -Program $tailProgram -TimeoutSeconds 10
            if ($result.Stdout) {
                Write-Host ($result.Stdout.TrimEnd())
            }
            if ($result.ExitCode -ne 0) {
                Write-Warning "the launch guard could not return its bounded log tail"
            }
        }
        catch {
            Write-Warning "the launch guard log tail was unavailable: $($_.Exception.Message)"
        }
    }

    function Set-LaunchState([ValidateSet("heartbeat", "ready", "stop")][string]$State) {
        $signalProgram = "exec $(ConvertTo-BashLiteral $wslPython) $(ConvertTo-BashLiteral $guardWsl) signal --token $(ConvertTo-BashLiteral $launchToken) --state $(ConvertTo-BashLiteral $State)"
        $result = Invoke-WslBashProgram -Program $signalProgram -TimeoutSeconds 10
        if ($result.ExitCode -ne 0) {
            throw "champion launch guard rejected the $State signal"
        }
    }

    $serverProcess = $null
    $retainServer = $false
    try {
        $serverProcess = Start-Process -FilePath "wsl.exe" -WindowStyle Hidden -PassThru -ArgumentList @(
            "--", "/usr/bin/env", "-u", "BASH_ENV", "-u", "ENV",
            "/bin/bash", "--noprofile", "--norc", "-c", $bootstrapArgument
        )
        $null = $serverProcess.Handle
        Write-Host "  attempt:   $launchToken   log: ~/.cache/cortex-speech/champion-launch/$launchToken.log"

        # Two replicas stream the ~30 GB checkpoint twice, so allow for the multi-GPU default.
        $startupTimer = [Diagnostics.Stopwatch]::StartNew()
        while ($startupTimer.Elapsed -lt [TimeSpan]::FromMinutes(15)) {
            if ($serverProcess.HasExited) {
                Write-Host "FAILED: champion process exited with code $($serverProcess.ExitCode). Last log lines:" -ForegroundColor Red
                Show-LaunchLog
                exit 1
            }
            Set-LaunchState "heartbeat"
            Start-Sleep -Seconds 10
            if (Test-ServerReady) {
                # Only the pointer-bound health result transfers ownership beyond this launcher. If the
                # guard is still alive, make that transfer durable before allowing the PowerShell process
                # to disappear. An exact service that came up independently needs no transfer from us.
                if (-not $serverProcess.HasExited) {
                    Set-LaunchState "ready"
                    $retainServer = $true
                }
                Write-Host "READY: exact champion healthy on 127.0.0.1:$port. The app will now use it." -ForegroundColor Green
                exit 0
            }
            Write-Host "  ...still loading (attempt $launchToken)"
        }

        Write-Host "FAILED: server did not come up within 15 minutes; stopping this launch attempt. Last log lines:" -ForegroundColor Red
        Show-LaunchLog
        exit 1
    }
    finally {
        if ($null -ne $serverProcess -and -not $retainServer -and -not $serverProcess.HasExited) {
            # Stop only the exact Process object returned by Start-Process. Do not search by PID/name and
            # risk killing an unrelated WSL session. The in-WSL guard owns the exact Linux process group.
            # Its pre-READY heartbeat also covers Ctrl+C paths on PowerShell 5.1 that bypass this finally.
            try {
                Set-LaunchState "stop"
            }
            catch {
                Write-Warning "the launch guard did not acknowledge cleanup; waiting before forced WSL cleanup"
            }
            if (-not $serverProcess.WaitForExit(20000)) {
                Stop-Process -InputObject $serverProcess -Force -ErrorAction SilentlyContinue
                if (-not $serverProcess.WaitForExit(5000)) {
                    Write-Warning "the launcher's exact wsl.exe did not confirm exit after forced cleanup"
                }
            }
        }
    }
}
finally {
    if ($ownsLaunchMutex) {
        $launchMutex.ReleaseMutex()
    }
    $launchMutex.Dispose()
}
