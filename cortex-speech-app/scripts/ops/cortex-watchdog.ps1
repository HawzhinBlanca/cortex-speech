# Cortex watchdog — phase 3 of docs/REMOTE_PUBLIC_LINKS_PLAN.md.
#
# One mechanism is both the autostart and the crash/wedge healer: a scheduled task runs this at
# logon and every 5 minutes. It probes the review server and relaunches the app only when the
# server is genuinely unreachable — so a healthy app is never touched, and a wedged one (process
# alive, port dead) is killed and restarted rather than trusted.
#
# Registration (one-time, as the logged-on user — see docs/REMOTE_PUBLIC_LINKS_PLAN.md):
#   powershell -ExecutionPolicy Bypass -File cortex-watchdog.ps1 -Register
# To stop the app ON PURPOSE without it resurrecting within 5 minutes:
#   schtasks /change /tn CortexWatchdog /disable   (re-enable with /enable)
#
# Design notes, argued in the plan:
#   * ANY HTTP status counts as alive — 401 is the server's normal answer to an unauthenticated
#     probe, and auth working IS the server working. Only refused/timeout means dead.
#   * "Run only when user is logged on": a WebView2 GUI cannot render in Session 0, which is why
#     service wrappers (NSSM/WinSW) are ruled out entirely.
#   * Paths are derived from this script's own location, never hardcoded — the repo's hygiene gate
#     forbids private absolute paths, and the watchdog must survive the repo being moved.
#   * The optional dead-man ping reads %APPDATA%\cortex-speech\healthcheck.url (untracked, owner
#     creates it). The GET carries liveness and source IP only — no data, per the privacy stance.

# -DryRun decides and REPORTS without killing or launching anything, so every branch can be drilled.
#
# This script had no test of any kind, and its most dangerous branch is the one that force-kills. The
# branch that must LEAVE A HEALTHY APP ALONE (the owner pressed Stop) was reviewed but never verified,
# and it could not be: proving it for real means pressing Stop, which deletes the session file and
# revokes the owner's live link. Ports and paths are overridable for the same reason — a drill must
# never touch the real profile. Production behaviour is unchanged when neither is set.
param(
    [switch]$Register,
    [switch]$DryRun,
    [ValidateSet('CortexWatchdog', 'CortexPrivateProductionWatchdog')]
    [string]$TaskName = 'CortexWatchdog'
)

$ErrorActionPreference = 'Stop'
$repoApp = Resolve-Path (Join-Path $PSScriptRoot '..\..')   # cortex-speech-app/
$dataDir = if ($env:CORTEX_WATCHDOG_DATA_DIR) { $env:CORTEX_WATCHDOG_DATA_DIR } else { Join-Path $env:APPDATA 'cortex-speech' }
$port = if ($env:CORTEX_WATCHDOG_PORT) { $env:CORTEX_WATCHDOG_PORT } else { '8737' }
$logDir = Join-Path $dataDir 'logs'
$log = Join-Path $logDir 'watchdog.log'
# CORTEX_WATCHDOG_EXE exists for the same reason the DATA_DIR/PORT overrides do: the three branches
# that depend on a LIVE process could only ever be drilled when the real app happened to be running,
# so the coverage of the force-kill decision — the most dangerous line in the availability path — was
# a coin toss on machine state. With the exe path overridable, the drill points this at a harmless
# decoy process it starts itself and reaches all three deterministically. Production is unaffected
# when unset. Private production adds one stronger source: an atomically published, hash-bound active
# release manifest in the data profile. A mutable cargo target is only a compatibility fallback before
# the first managed release. Once the pointer exists, a malformed/tampered pointer is a hard stop; it
# must never silently fall back to whichever binary a later build happened to overwrite.
# TLS FIRST, then plain HTTP. Couch Review serves TLS on every interface with a self-signed
# certificate it generates locally, so an http:// probe against it does not fail cleanly — it comes
# back as "corrupt message of type InvalidContentType", which this script read as a DEAD PORT and
# answered by force-killing a perfectly healthy app, every five minutes, forever.
#
# MEASURED 2026-08-16: exactly that. Three probes 5s apart, then a kill, then a relaunch, in a loop —
# the watchdog became the outage it exists to prevent. Both schemes are tried because the same script
# has to supervise an older HTTP build and the current TLS one.
#
# Certificate validation is disabled for THIS probe only: the certificate is self-signed by design
# and this is a localhost liveness check, not an authentication decision. It reads no response body.
$probeUrls = @("https://127.0.0.1:$port/", "http://127.0.0.1:$port/")
try {
    Add-Type -TypeDefinition @'
using System.Net;
using System.Security.Cryptography.X509Certificates;
public static class CortexProbeCerts {
    public static void TrustAll() {
        ServicePointManager.ServerCertificateValidationCallback = delegate { return true; };
        ServicePointManager.SecurityProtocol = SecurityProtocolType.Tls12;
    }
}
'@ -ErrorAction Stop
    [CortexProbeCerts]::TrustAll()
} catch {
    # Already loaded in this session, or the type exists — either way the callback is set.
}

# The release pointer binds the complete operations tree, not only this watchdog file. Keep the
# implementation inside the supported .NET runtime so a recovery tick does not depend on Python,
# Git, module autoload, or shell command parsing. File streams deny concurrent writes while each
# size/hash pair is measured; reparse points fail closed instead of making authority leave the
# immutable release directory.
$helperTypeLoadError = $null
try {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;

public static class CortexOperationsDigest {
    private static readonly Regex BuildMarker = new Regex(
        "CORTEX_BUILD_SHA:([0-9a-f]{40}|unknown)(?![0-9a-f])",
        RegexOptions.CultureInvariant);
    private sealed class Entry {
        public string Relative;
        public string Full;
        public Entry(string relative, string full) { Relative = relative; Full = full; }
    }

    private static string Hex(byte[] bytes) {
        StringBuilder value = new StringBuilder(bytes.Length * 2);
        foreach (byte item in bytes) value.Append(item.ToString("x2"));
        return value.ToString();
    }

    private static byte[] BigEndian(UInt64 value, int width) {
        byte[] bytes = BitConverter.GetBytes(value);
        if (BitConverter.IsLittleEndian) Array.Reverse(bytes);
        if (bytes.Length == width) return bytes;
        byte[] result = new byte[width];
        Buffer.BlockCopy(bytes, bytes.Length - width, result, 0, width);
        return result;
    }

    private static void Transform(SHA256 digest, byte[] bytes) {
        digest.TransformBlock(bytes, 0, bytes.Length, bytes, 0);
    }

    private static void AddTree(string root, DirectoryInfo directory, List<Entry> entries) {
        if ((directory.Attributes & FileAttributes.ReparsePoint) != 0)
            throw new IOException("operations bundle contains a directory reparse point: " + directory.FullName);
        foreach (FileInfo file in directory.GetFiles()) {
            if ((file.Attributes & FileAttributes.ReparsePoint) != 0)
                throw new IOException("operations bundle contains a file reparse point: " + file.FullName);
            if (String.Equals(file.Extension, ".pyc", StringComparison.OrdinalIgnoreCase)) continue;
            string relative = file.FullName.Substring(root.Length).TrimStart('\\', '/').Replace('\\', '/');
            entries.Add(new Entry(relative, file.FullName));
        }
        foreach (DirectoryInfo child in directory.GetDirectories()) {
            if (String.Equals(child.Name, "__pycache__", StringComparison.Ordinal)) continue;
            AddTree(root, child, entries);
        }
    }

    private static void RequireContainedDirectoryChain(string root, FileInfo file) {
        DirectoryInfo directory = file.Directory;
        while (directory != null) {
            if ((directory.Attributes & FileAttributes.ReparsePoint) != 0)
                throw new IOException("operations authority crosses a directory reparse point: " + directory.FullName);
            if (String.Equals(directory.FullName.TrimEnd('\\', '/'), root, StringComparison.OrdinalIgnoreCase)) return;
            directory = directory.Parent;
        }
        throw new IOException("operations authority escapes the immutable release root: " + file.FullName);
    }

    public static string BakedGitSha(string path) {
        string actual = null;
        byte[] carry = new byte[0];
        byte[] chunk = new byte[1024 * 1024];
        using (FileStream stream = new FileStream(
            path, FileMode.Open, FileAccess.Read, FileShare.Read,
            chunk.Length, FileOptions.SequentialScan)) {
            int read;
            while ((read = stream.Read(chunk, 0, chunk.Length)) > 0) {
                byte[] window = new byte[carry.Length + read];
                Buffer.BlockCopy(carry, 0, window, 0, carry.Length);
                Buffer.BlockCopy(chunk, 0, window, carry.Length, read);
                string text = Encoding.ASCII.GetString(window);
                int safeStartLimit = Math.Max(0, window.Length - 80);
                foreach (Match match in BuildMarker.Matches(text)) {
                    if (match.Index >= safeStartLimit) break;
                    if (actual != null) throw new IOException("executable contains multiple build SHA markers");
                    actual = match.Groups[1].Value;
                }
                int keep = Math.Min(80, window.Length);
                carry = new byte[keep];
                Buffer.BlockCopy(window, window.Length - keep, carry, 0, keep);
            }
        }
        string tail = Encoding.ASCII.GetString(carry);
        foreach (Match match in BuildMarker.Matches(tail)) {
            if (actual != null) throw new IOException("executable contains multiple build SHA markers");
            actual = match.Groups[1].Value;
        }
        if (actual == null) throw new IOException("executable has no exact build SHA marker");
        return actual;
    }

    public static string Compute(string rootPath) {
        string root = Path.GetFullPath(rootPath).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        DirectoryInfo rootInfo = new DirectoryInfo(root);
        if (!rootInfo.Exists || (rootInfo.Attributes & FileAttributes.ReparsePoint) != 0)
            throw new IOException("immutable operations root is missing or is a reparse point");
        DirectoryInfo scripts = new DirectoryInfo(Path.Combine(root, "scripts"));
        if (!scripts.Exists) throw new DirectoryNotFoundException("operations scripts directory is missing");
        List<Entry> entries = new List<Entry>();
        AddTree(root, scripts, entries);
        string migration = Path.Combine(root, "src-tauri", "src", "migrations", "mod.rs");
        string dialect = Path.Combine(root, "src-tauri", "src", "dialect.rs");
        FileInfo migrationInfo = new FileInfo(migration);
        FileInfo dialectInfo = new FileInfo(dialect);
        if (!migrationInfo.Exists || !dialectInfo.Exists || entries.Count == 0)
            throw new IOException("operations bundle is missing scripts, the canonical migration ledger, or dialect authority");
        if ((migrationInfo.Attributes & FileAttributes.ReparsePoint) != 0)
            throw new IOException("canonical migration ledger is a reparse point");
        if ((dialectInfo.Attributes & FileAttributes.ReparsePoint) != 0)
            throw new IOException("dialect authority is a reparse point");
        RequireContainedDirectoryChain(root, migrationInfo);
        RequireContainedDirectoryChain(root, dialectInfo);
        entries.Add(new Entry("src-tauri/src/migrations/mod.rs", migrationInfo.FullName));
        entries.Add(new Entry("src-tauri/src/dialect.rs", dialectInfo.FullName));
        entries.Sort(delegate(Entry left, Entry right) {
            return StringComparer.Ordinal.Compare(left.Relative, right.Relative);
        });

        using (SHA256 aggregate = SHA256.Create()) {
            foreach (Entry entry in entries) {
                byte[] relative = Encoding.UTF8.GetBytes(entry.Relative);
                byte[] contentHash;
                long length;
                using (FileStream stream = new FileStream(
                    entry.Full, FileMode.Open, FileAccess.Read, FileShare.Read,
                    1024 * 1024, FileOptions.SequentialScan)) {
                    length = stream.Length;
                    using (SHA256 content = SHA256.Create()) contentHash = content.ComputeHash(stream);
                }
                Transform(aggregate, BigEndian((UInt64)relative.Length, 4));
                Transform(aggregate, relative);
                Transform(aggregate, BigEndian((UInt64)length, 8));
                Transform(aggregate, Encoding.ASCII.GetBytes(Hex(contentHash)));
            }
            aggregate.TransformFinalBlock(new byte[0], 0, 0);
            return Hex(aggregate.Hash);
        }
    }
}

public static class CortexTcpOwnership {
    private const int AF_INET = 2;
    private const int TCP_TABLE_OWNER_PID_LISTENER = 3;
    private const uint ERROR_INSUFFICIENT_BUFFER = 122;

    [StructLayout(LayoutKind.Sequential)]
    private struct MibTcpRowOwnerPid {
        public uint State;
        public uint LocalAddress;
        public uint LocalPort;
        public uint RemoteAddress;
        public uint RemotePort;
        public uint OwningPid;
    }

    [DllImport("iphlpapi.dll", SetLastError = true)]
    private static extern uint GetExtendedTcpTable(
        IntPtr table, ref int size, bool order, int family, int tableClass, uint reserved);

    public static int[] ListenerPids(int port) {
        int size = 0;
        uint result = GetExtendedTcpTable(
            IntPtr.Zero, ref size, true, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0);
        if (result != ERROR_INSUFFICIENT_BUFFER)
            throw new InvalidOperationException("TCP listener table size query failed: " + result);
        IntPtr buffer = Marshal.AllocHGlobal(size);
        try {
            result = GetExtendedTcpTable(
                buffer, ref size, true, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0);
            if (result != 0) throw new InvalidOperationException("TCP listener table query failed: " + result);
            int count = Marshal.ReadInt32(buffer);
            int rowSize = Marshal.SizeOf(typeof(MibTcpRowOwnerPid));
            long cursor = buffer.ToInt64() + sizeof(uint);
            HashSet<int> pids = new HashSet<int>();
            for (int index = 0; index < count; index++) {
                MibTcpRowOwnerPid row = (MibTcpRowOwnerPid)Marshal.PtrToStructure(
                    new IntPtr(cursor + ((long)index * rowSize)), typeof(MibTcpRowOwnerPid));
                int rowPort = (int)(((row.LocalPort & 0xffU) << 8) | ((row.LocalPort & 0xff00U) >> 8));
                IPAddress address = new IPAddress((long)row.LocalAddress);
                if (rowPort == port && (row.LocalAddress == 0 || address.Equals(IPAddress.Loopback)))
                    pids.Add((int)row.OwningPid);
            }
            int[] answer = new int[pids.Count];
            pids.CopyTo(answer);
            return answer;
        } finally {
            Marshal.FreeHGlobal(buffer);
        }
    }
}
'@ -ErrorAction Stop
} catch {
    $helperTypeLoadError = $_.Exception.Message
}
# The one line a drill reads. Printed for every decision so a test asserts the CHOICE, not a side effect.
function Report([string]$action) { Write-Output "WATCHDOG-ACTION: $action" }

function Get-Sha256Hex([string]$path) {
    # Do not depend on PowerShell module autoload during crash recovery. `Get-FileHash` is supplied
    # by Microsoft.PowerShell.Utility and was unavailable in a real handover subprocess even though
    # it existed interactively. The framework primitive is always present in the supported runtime.
    $stream = [System.IO.File]::OpenRead($path)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return -join @($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

function Get-Sha256Utf8Lf([string]$path) {
    $text = [System.IO.File]::ReadAllText($path).Replace("`r`n", "`n")
    if ($text.Contains("`r")) { throw "unsupported bare carriage return in $path" }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return -join @($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $sha.Dispose()
    }
}

# Logging must never be able to stop the watchdog. With $ErrorActionPreference = 'Stop' a failed
# Add-Content (full disk, the log opened by an editor, a locked profile) threw and aborted the run
# BEFORE the kill/relaunch below — so the one condition most likely to take the app down was also the
# condition that disabled its recovery. Recovering matters more than recording why.
function Write-Log([string]$msg) {
    try {
        if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Force $logDir | Out-Null }
        Add-Content -Path $log -Value ("{0}  {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg)
    } catch { }
}

function Get-VerifiedActiveRelease {
    $pointerPath = Join-Path $dataDir 'active-private-production-release.json'
    if (-not (Test-Path -LiteralPath $pointerPath)) { return $null }
    try {
        $value = Get-Content -LiteralPath $pointerPath -Raw | ConvertFrom-Json
        $legacyExpected = @(
            'schema', 'releaseId', 'expectedDatabaseSchema', 'appGitSha', 'createdAtUtc',
            'directory', 'appExe', 'poolAdminExe', 'appSha256', 'poolAdminSha256',
            'watchdogScript', 'watchdogSha256', 'operationsSha256',
            'dedupManifest', 'dedupManifestSha256'
        )
        $currentExpected = @($legacyExpected) + @(
            'schemaContract', 'schemaContractId', 'schemaContractSha256'
        )
        if ($value.schema -isnot [int]) { throw 'release pointer schema is not an integer' }
        if ($value.schema -eq 2) {
            $expected = $currentExpected
            if ($value.expectedDatabaseSchema -isnot [int] -or $value.expectedDatabaseSchema -ne 69) {
                throw 'release pointer does not require private-production database schema 69'
            }
        } elseif ($value.schema -eq 1) {
            # The only legacy boundary still safe for the 65->69 handover is the exact managed v65
            # pointer. Schema 63/64 releases are not migration authorities for this controller.
            $expected = $legacyExpected
            if ($value.expectedDatabaseSchema -isnot [int] -or $value.expectedDatabaseSchema -ne 65) {
                throw 'legacy release pointer is not the exact schema-65 handover boundary'
            }
        } else {
            throw 'release pointer manifest schema is unsupported'
        }
        $actual = @($value.PSObject.Properties.Name)
        $missing = @($expected | Where-Object { $_ -notin $actual })
        $extra = @($actual | Where-Object { $_ -notin $expected })
        if ($missing.Count -or $extra.Count) { throw "release pointer fields do not match manifest schema $($value.schema)" }
        if ([string]$value.appGitSha -notmatch '^[0-9a-f]{40}$') { throw 'release pointer git SHA is invalid' }
        $hashFields = @('appSha256', 'poolAdminSha256', 'watchdogSha256', 'operationsSha256', 'dedupManifestSha256')
        if ($value.schema -eq 2) { $hashFields += 'schemaContractSha256' }
        foreach ($field in $hashFields) {
            if ([string]$value.$field -notmatch '^[0-9a-f]{64}$') { throw "release pointer $field is invalid" }
        }
        if ($null -ne $helperTypeLoadError) {
            throw "release verification helpers could not load: $helperTypeLoadError"
        }
        $directory = (Resolve-Path -LiteralPath ([string]$value.directory) -ErrorAction Stop).Path.TrimEnd('\')
        $pathFields = @('appExe', 'poolAdminExe', 'watchdogScript', 'dedupManifest')
        if ($value.schema -eq 2) { $pathFields += 'schemaContract' }
        foreach ($field in $pathFields) {
            $resolved = (Resolve-Path -LiteralPath ([string]$value.$field) -ErrorAction Stop).Path
            if (-not $resolved.StartsWith($directory + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "release pointer $field escapes its immutable release directory"
            }
            $value.$field = $resolved
        }
        $dedup = Get-Content -LiteralPath ([string]$value.dedupManifest) -Raw | ConvertFrom-Json
        if ($dedup.manifestSchema -ne 1 -or [string]$dedup.manifestSha256 -ne [string]$value.dedupManifestSha256) {
            throw 'release dedup manifest identity does not match the pointer'
        }
        if ($dedup.summary.unconfirmedRiskGroups -ne 0) { throw 'release dedup manifest has unresolved risk' }
        if ($value.schema -eq 2) {
            $expectedContract = (Resolve-Path -LiteralPath (Join-Path $directory 'scripts\private_production_schema_contract.v1.json') -ErrorAction Stop).Path
            if ([string]$value.schemaContract -cne $expectedContract) {
                throw 'release schema contract is not at its canonical immutable path'
            }
            if ([string]$value.schemaContractId -cne 'cortex-private-production-schema-65-to-69-v1') {
                throw 'release schema contract identity is invalid'
            }
            if ((Get-Sha256Hex ([string]$value.schemaContract)) -cne [string]$value.schemaContractSha256) {
                throw 'release schema contract hash does not match the pointer'
            }
            $contract = Get-Content -LiteralPath ([string]$value.schemaContract) -Raw | ConvertFrom-Json
            $contractExpected = @(
                'schema', 'contractId', 'targetSchema', 'supportedMigrationSources',
                'sameSchemaRecovery', 'normalization', 'algorithm', 'migrationSource',
                'migrationSourceSha256', 'historicalPrefixThroughSchema',
                'historicalPrefixSha256', 'appendOnlyContract', 'appendOnlyContractSha256'
            )
            $contractActual = @($contract.PSObject.Properties.Name)
            $contractMissing = @($contractExpected | Where-Object { $_ -notin $contractActual })
            $contractExtra = @($contractActual | Where-Object { $_ -notin $contractExpected })
            if ($contractMissing.Count -or $contractExtra.Count) { throw 'release schema contract fields are invalid' }
            $sources = @($contract.supportedMigrationSources)
            if ($contract.schema -isnot [int] -or $contract.schema -ne 1 `
                -or [string]$contract.contractId -cne 'cortex-private-production-schema-65-to-69-v1' `
                -or $contract.targetSchema -isnot [int] -or $contract.targetSchema -ne 69 `
                -or $sources.Count -ne 1 -or $sources[0] -isnot [int] -or $sources[0] -ne 65 `
                -or $contract.sameSchemaRecovery -isnot [bool] -or $contract.sameSchemaRecovery -ne $true `
                -or [string]$contract.normalization -cne 'utf8-lf' `
                -or [string]$contract.algorithm -cne 'sha256' `
                -or $contract.historicalPrefixThroughSchema -isnot [int] `
                -or $contract.historicalPrefixThroughSchema -ne 65) {
                throw 'release schema contract semantics are invalid'
            }
            if ([string]$contract.migrationSource -cne 'src-tauri/src/migrations/mod.rs' `
                -or [string]$contract.appendOnlyContract -cne 'scripts/append_only_migration_contract.v1.json') {
                throw 'release schema contract source paths are invalid'
            }
            foreach ($field in @('migrationSourceSha256', 'historicalPrefixSha256', 'appendOnlyContractSha256')) {
                if ([string]$contract.$field -notmatch '^[0-9a-f]{64}$') { throw "release schema contract $field is invalid" }
            }
            $migrationSource = (Resolve-Path -LiteralPath (Join-Path $directory 'src-tauri\src\migrations\mod.rs') -ErrorAction Stop).Path
            $appendOnlyContract = (Resolve-Path -LiteralPath (Join-Path $directory 'scripts\append_only_migration_contract.v1.json') -ErrorAction Stop).Path
            if ((Get-Sha256Utf8Lf $migrationSource) -cne [string]$contract.migrationSourceSha256) {
                throw 'release migration source does not match the schema contract'
            }
            if ((Get-Sha256Hex $appendOnlyContract) -cne [string]$contract.appendOnlyContractSha256) {
                throw 'release append-only authority does not match the schema contract'
            }
        }
        $checks = @(
            @([string]$value.appExe, [string]$value.appSha256),
            @([string]$value.poolAdminExe, [string]$value.poolAdminSha256),
            @([string]$value.watchdogScript, [string]$value.watchdogSha256)
        )
        foreach ($check in $checks) {
            $actualSha = Get-Sha256Hex $check[0]
            if ($actualSha -ne $check[1]) { throw "release artifact hash mismatch: $($check[0])" }
        }
        $operationsActual = [CortexOperationsDigest]::Compute($directory)
        if ($operationsActual -cne [string]$value.operationsSha256) {
            throw 'release operations bundle does not match the pointer'
        }
        foreach ($binary in @(
            @([string]$value.appExe, 'application'),
            @([string]$value.poolAdminExe, 'pool administrator')
        )) {
            $bakedSha = [CortexOperationsDigest]::BakedGitSha($binary[0])
            if ($bakedSha -cne [string]$value.appGitSha) {
                throw "release $($binary[1]) build SHA does not match the pointer"
            }
        }
        return $value
    } catch {
        Report 'blocked (active release pointer invalid)'
        Write-Log "active release pointer refused: $($_.Exception.Message)"
        exit 1
    }
}

$activeRelease = if ($env:CORTEX_WATCHDOG_EXE) { $null } else { Get-VerifiedActiveRelease }
$packagedExe = Join-Path $repoApp 'cortex-speech-app.exe'
$legacyExe = Join-Path $repoApp 'src-tauri\target\release\cortex-speech-app.exe'
$packagedPoolAdmin = Join-Path $repoApp 'pool_admin.exe'
$legacyPoolAdmin = Join-Path $repoApp 'src-tauri\target\release\pool_admin.exe'
$exe = if ($env:CORTEX_WATCHDOG_EXE) { $env:CORTEX_WATCHDOG_EXE } elseif ($null -ne $activeRelease) {
    [string]$activeRelease.appExe
} elseif (Test-Path -LiteralPath $packagedExe) { $packagedExe } else { $legacyExe }
$poolAdmin = if ($env:CORTEX_WATCHDOG_POOL_ADMIN) { $env:CORTEX_WATCHDOG_POOL_ADMIN } elseif ($null -ne $activeRelease) {
    [string]$activeRelease.poolAdminExe
} elseif (Test-Path -LiteralPath $packagedPoolAdmin) { $packagedPoolAdmin } else { $legacyPoolAdmin }
$exeFull = try { (Resolve-Path -LiteralPath $exe -ErrorAction Stop).Path } catch { $exe }
$session = Join-Path $dataDir 'couch_session.json'

# A hash-valid pointer is still unsafe if its executable and live database disagree about schema.
# `status` uses pool_admin's source-enforced read-only opener, which validates the complete applied
# migration history against the exact binary without migrating or taking a reviewer lease. Do this
# before any probe, kill or launch decision so a v69 binary can never be restart-looped against a v65
# or future database (and the narrowly-supported legacy v65 pointer proves the inverse boundary too).
function Test-ActiveReleaseDatabaseSchema([string]$adminPath, [string]$databasePath) {
    $process = $null
    try {
        $start = New-Object System.Diagnostics.ProcessStartInfo
        $start.FileName = $adminPath
        $start.Arguments = 'status --db "' + $databasePath + '"'
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $process = New-Object System.Diagnostics.Process
        $process.StartInfo = $start
        if (-not $process.Start()) { return @{ Healthy = $false; Reason = 'pool_admin did not start' } }
        if (-not $process.WaitForExit(60000)) {
            try { $process.Kill() } catch { }
            try { [void]$process.WaitForExit(5000) } catch { }
            return @{ Healthy = $false; Reason = 'pool_admin schema probe exceeded 60 seconds' }
        }
        if ($process.ExitCode -ne 0) {
            return @{ Healthy = $false; Reason = "pool_admin status exited $($process.ExitCode)" }
        }
        return @{ Healthy = $true; Reason = 'exact binary accepted complete migration history' }
    } catch {
        return @{ Healthy = $false; Reason = "pool_admin schema probe failed: $($_.Exception.Message)" }
    } finally {
        if ($null -ne $process) { $process.Dispose() }
    }
}

if ($null -ne $activeRelease) {
    $dbPath = Join-Path $dataDir 'cortex-speech.db'
    if (-not (Test-Path -LiteralPath $dbPath)) {
        Report 'blocked (active release database missing)'
        Write-Log 'active release database is missing - refusing process control'
        exit 1
    }
    $schemaProbe = Test-ActiveReleaseDatabaseSchema $poolAdmin $dbPath
    if ($schemaProbe.Healthy -ne $true) {
        Report 'blocked (active release database schema mismatch)'
        Write-Log "active release database schema refused by exact pool_admin: $($schemaProbe.Reason)"
        exit 1
    }
}

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    # Two triggers: at logon (the autostart), and a repeating clock (the healer). Task Scheduler
    # caps a repetition trigger's duration; (New-TimeSpan -Days 3650) is rejected on Win11, so the
    # logon trigger carries an indefinite repetition instead.
    # An unscoped AtLogOn trigger means "any user" and requires administrator rights. Production
    # review runs only in this interactive user's WebView2 session, so bind the trigger to the exact
    # logged-on principal. This is both least privilege and independently registerable during a
    # recovery-safe handover.
    $currentPrincipal = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $logon = New-ScheduledTaskTrigger -AtLogOn -User $currentPrincipal
    # A repeating AtLogOn trigger registered after the user already logged on has no NextRunTime
    # until the next sign-in. That left a newly deployed reviewer line unmonitored for the rest of
    # the current session. Keep the logon trigger for future interactive sessions and add a separate
    # clock trigger that starts within one minute of registration and repeats indefinitely.
    $clock = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
        -RepetitionInterval (New-TimeSpan -Minutes 5)
    # Battery flags are ON by default and would silently disable the whole watchdog the moment Windows
    # believes it is on battery — which includes a desktop behind a UPS, exactly the machine most
    # likely to be running this. An always-on server must not stop healing itself during a power event.
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger @($logon, $clock) `
        -Settings $settings -Force | Out-Null
    Write-Log "registered (exe: $exe)"
    Write-Output "$TaskName registered: at-logon + every 5 minutes, run-only-when-logged-on."
    exit 0
}

# ── the probe ──────────────────────────────────────────────────────────────────
# THREE attempts, and a timeout longer than anything the server may legitimately be inside.
#
# The old single 5s probe force-killed healthy apps. couch.rs spawns ONE accept thread per reviewer,
# and while that thread is inside handle_request it is not accepting — so the probe's connection sits
# in the listen backlog unanswered. Two ordinary things hold it past 5s: any DB call contending with
# the desktop app (busy_timeout is 10s — twice the old probe), and materialising a clip's WAV. A
# transport timeout leaves $_.Exception.Response empty, which read as "dead", so the busier the
# reviewer the likelier the watchdog was to kill the app mid-review and destroy their in-flight work.
#
# 20s clears busy_timeout with margin, and one answer out of three attempts is enough to prove life.
$alive = $false
foreach ($attempt in 1..3) {
    foreach ($probeUrl in $probeUrls) {
        try {
            Invoke-WebRequest -Uri $probeUrl -UseBasicParsing -TimeoutSec 20 | Out-Null
            $alive = $true   # 2xx/3xx
        } catch {
            # A status-carrying refusal (401 et al.) is the server ANSWERING — alive. Only a transport
            # failure (refused, timeout, reset) leaves .Response empty and means dead.
            if ($null -ne $_.Exception.Response) { $alive = $true }
            # An SSL/content-type mismatch is ALSO the server answering — it spoke, we mis-heard it.
            # Treating a protocol mismatch as death is what turned this watchdog into a kill loop.
            elseif ($_.Exception.Message -match 'SSL|TLS|secure channel|corrupt message|InvalidContentType') { $alive = $true }
        }
        if ($alive) { break }
    }
    if ($alive) { break }
    if ($attempt -lt 3) { Start-Sleep -Seconds 5 }
}

# Consecutive forced kills that did NOT restore service. Killing is only ever justified as a way to
# fix a wedge; when it demonstrably is not fixing one, continuing is just an app that dies every five
# minutes forever. Reset the moment the server answers.
$killCountFile = Join-Path $logDir 'watchdog-kills.txt'
$maxConsecutiveKills = 3

# STARTUP GRACE. A freshly launched app that has not opened 8737 YET is indistinguishable from a
# wedged one by port alone — and the difference is the whole decision. Startup work scales with the
# library: measured 2026-08-14 it took 8m16s to reach couch, and on 2026-08-15, after the library
# tripled to 14,828 clips, it exceeded the 5-minute check interval entirely. The watchdog then killed
# the app at 5 minutes, three times in a row, and the app never once got far enough to serve. A
# watchdog that shortens startup until startup can never finish is a denial of service on its own app.
#
# 10 minutes. THE THING THIS WAS SIZED FOR NO LONGER EXISTS (2026-08-17). The grace was 45 minutes
# because startup to couch measured 8m16s at ~500 clips and 18m11s at 14,828, and "it scales with the
# library" was taken as a fact of life. It was one line: Database::backup paced the SQLite online
# backup at 5 pages per 250 ms — 80 KB/s — and take_snapshot runs SYNCHRONOUSLY on the startup path,
# so the whole delay was the app copying its own database at dial-up speed before opening the port.
# Fixed there; MEASURED startup to couch afterwards: 6.4 SECONDS at 14,828 clips.
#
# So the generosity now costs what it was buying. At 45 minutes a genuinely wedged app sits unnoticed
# for three quarters of an hour — and the old comment's claim that being slow "costs ONE extra check
# cycle" was never true. 10 minutes is ~90x the measured startup, which is ample headroom for a much
# larger library or a busy disk, while cutting the worst case for a wedged app by more than four
# times. The kill path still works for anything older than this.
# Overridable so the decision tests can exercise BOTH sides: 0 makes every process instantly
# "old enough" to reach the kill path, which is the only way to test the kill path without waiting
# 20 real minutes.
$startupGraceMinutes = if ($env:CORTEX_WATCHDOG_STARTUP_GRACE_MIN) { [double]$env:CORTEX_WATCHDOG_STARTUP_GRACE_MIN } else { 10 }

if ($alive) {
    # A response proves only that *something* owns the port. Before declaring the supervised app
    # healthy, bind the IPv4 listener PID back to the exact immutable executable path. Otherwise a
    # stray dev server, stale helper, or hostile local responder can mask a crashed production app
    # forever. Re-query each PID after the probe so a process observed before the 60-second probe
    # window cannot be confused with a later PID reuse.
    try {
        if ($null -ne $helperTypeLoadError) { throw "listener ownership helper could not load: $helperTypeLoadError" }
        $portNumber = 0
        if (-not [int]::TryParse([string]$port, [ref]$portNumber) -or $portNumber -lt 1 -or $portNumber -gt 65535) {
            throw "watchdog port is invalid: $port"
        }
        $listenerPids = @([CortexTcpOwnership]::ListenerPids($portNumber))
        $matchingOwners = @($listenerPids | ForEach-Object {
            $owner = Get-Process -Id $_ -ErrorAction SilentlyContinue
            if ($null -ne $owner -and $owner.Path -and $owner.Path -eq $exeFull) { $owner }
        })
        if (-not $matchingOwners.Count) {
            throw "responding port $port is not owned by exact app path $exeFull"
        }
    } catch {
        Report 'blocked (responding port is not owned by active release)'
        Write-Log "liveness responder refused: $($_.Exception.Message)"
        exit 1
    }
    if (Test-Path $killCountFile) { Remove-Item $killCountFile -Force -ErrorAction SilentlyContinue }
    Report 'alive'
    if ($DryRun) { exit 0 }
    # v69 private-production certification. This is a read-only SQLite/filesystem report: it does not
    # fetch a queue, take a lease, mark a clip seen, or touch reviewer history. Run it on the same
    # five-minute clock as liveness while a reviewer session is expected. A failed certification does
    # NOT trigger the destructive restart path (restarting cannot repair missing audio/rights/backups),
    # but it suppresses the dead-man success ping and leaves an actionable report in the data profile.
    $certHealthy = $true
    $dbPath = Join-Path $dataDir 'cortex-speech.db'
    if ((Test-Path -LiteralPath $session) -and (Test-Path -LiteralPath $poolAdmin) -and (Test-Path -LiteralPath $dbPath)) {
        $certError = Join-Path $logDir 'pool-certification.stderr.log'
        $certOutput = @(& $poolAdmin certify --db $dbPath --require-review-ready 2> $certError)
        $certExit = $LASTEXITCODE
        if ($certOutput.Count -gt 0) {
            try {
                if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Force $logDir | Out-Null }
                $certPath = Join-Path $logDir 'pool-certification.json'
                $certTemp = Join-Path $logDir ('.pool-certification-' + [guid]::NewGuid().ToString('N') + '.tmp')
                [System.IO.File]::WriteAllText($certTemp, (($certOutput -join [Environment]::NewLine) + [Environment]::NewLine))
                Move-Item -LiteralPath $certTemp -Destination $certPath -Force
            } catch {
                $certHealthy = $false
                Write-Log "pool certification output could not be published: $($_.Exception.Message)"
            }
        }
        if ($certExit -ne 0) {
            $certHealthy = $false
            $reason = try { (Get-Content -LiteralPath $certError -Raw -ErrorAction Stop).Trim() } catch { 'no stderr detail' }
            Write-Log "pool certification FAILED (exit $certExit): $reason"
        } else {
            Write-Log 'pool certification OK (review-ready)'
        }
    }
    # Optional dead-man ping: silence at healthchecks.io alerts the owner's phone.
    $hcFile = Join-Path $dataDir 'healthcheck.url'
    if ($certHealthy -and (Test-Path $hcFile)) {
        $hc = (Get-Content $hcFile -TotalCount 1).Trim()
        if ($hc -match '^https://') {
            try { Invoke-WebRequest -Uri $hc -UseBasicParsing -TimeoutSec 10 | Out-Null } catch {}
        }
    }
    exit 0
}

# ── port dead: decide WHY before touching anything ────────────────────────────
# A closed port has two honest meanings, and only the session file tells them apart:
#   * couch_session.json EXISTS  -> the server is SUPPOSED to be serving (resume would bring it up),
#     so a running process with a dead port is wedged: kill and relaunch.
#   * no session file            -> the owner pressed Stop. A running app with couch off is a
#     HEALTHY state — killing it here would resurrect-loop the app every 5 minutes and make Stop
#     feel haunted. Leave a running app alone; only launch if the process itself is gone
#     (the autostart half of this task).
# Matched by PATH, not by name. `Get-Process -Name cortex-speech-app` force-kills ANY process with
# that name — a second checkout, a debug build, an installed copy under Program Files — while the
# relaunch below only ever starts THIS one. The watchdog would happily kill a build it is not
# responsible for and then report success. (A process whose Path cannot be read is not ours to kill.)
$proc = @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path -eq $exeFull })

# A live batch importer deliberately owns the SAME exclusive `cortex.lock` as the GUI. Launching the
# GUI while that lock is held cannot succeed, and repeating the attempt every five minutes turns an
# expected long import into noisy failed starts while reviewers still see a dead link. Check the OS
# handle, not mere file existence: a crashed process may leave a stale lockfile, but that file opens
# successfully and the app's normal stale-lock recovery will remove it. Only a Windows sharing/lock
# violation proves a live holder and defers; every other probe fault is operationally red. Once the
# holder exits, the next watchdog tick resumes the ordinary launch path automatically.
function Get-DatabaseLockState {
    $lockPath = Join-Path $dataDir 'cortex.lock'
    if (-not (Test-Path -LiteralPath $lockPath)) { return 'absent' }
    $stream = $null
    try {
        $stream = [System.IO.File]::Open(
            $lockPath,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
        return 'free'
    } catch [System.IO.IOException] {
        # Only Windows sharing/lock violations prove that another live process owns the file. Treating
        # disk faults, malformed paths or ACL damage as an ordinary importer made the watchdog report
        # success while deferring forever. Preserve the actual fault for an actionable blocked state.
        $win32 = $_.Exception.HResult -band 0xFFFF
        if ($win32 -eq 32 -or $win32 -eq 33) { return 'held' }
        throw
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
    }
}

$lockState = try { Get-DatabaseLockState } catch {
    Report 'blocked (database lock probe failed)'
    Write-Log "database lock probe failed - refusing to launch blindly: $($_.Exception.Message)"
    exit 1
}
if (-not $proc.Count -and $lockState -eq 'held') {
    Report 'defer (live database lock held by importer/maintenance)'
    Write-Log 'database lock is held by another process - deferring app launch without disturbing it'
    exit 0
}

if (-not (Test-Path $session)) {
    if ($proc.Count) { Report 'leave-alone (deliberate Stop)'; exit 0 }   # the app is fine
    Report 'launch (no session, not running)'
    Write-Log "app not running (no session) - launching for availability"
} elseif ($proc.Count) {
    # THE KILL LOOP. A present session file does NOT mean couch can come up. resume() swallows every
    # start failure (something else holding 8737), and load_session refuses a session whose db_path
    # moved — both leave the file on disk with the port dead and the app running, forever. Killing
    # then never helps and the app dies every five minutes, losing in-flight work each time.
    # Youngest instance decides: if ANY matching process is still inside its startup grace, the port
    # being closed is expected, not evidence of a wedge.
    $youngestMin = ($proc | ForEach-Object { ((Get-Date) - $_.StartTime).TotalMinutes } | Measure-Object -Minimum).Minimum
    if ($youngestMin -lt $startupGraceMinutes) {
        Report ("starting-up ({0:N1} min old, grace {1} min)" -f $youngestMin, $startupGraceMinutes)
        Write-Log ("port not open yet but the app is only {0:N1} min old (grace {1} min) - leaving it alone to finish starting" -f $youngestMin, $startupGraceMinutes)
        exit 0
    }
    $kills = 0
    if (Test-Path $killCountFile) { $kills = [int]((Get-Content $killCountFile -TotalCount 1).Trim()) }
    if ($kills -ge $maxConsecutiveKills) {
        Report 'give-up (kill cap reached)'
        Write-Log "port still dead after $kills forced restarts - NOT killing again; couch cannot start (port taken, or the library moved). Owner action needed."
        exit 1
    }
    Report "kill-and-relaunch (attempt $($kills + 1)/$maxConsecutiveKills)"
    if ($DryRun) { exit 0 }
    Write-Log "session expected but port dead - killing wedged pid(s): $($proc.Id -join ', ') (attempt $($kills + 1)/$maxConsecutiveKills)"
    $proc | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2   # flock.rs clears the stale lock on the next start
    try { Set-Content -Path $killCountFile -Value ([string]($kills + 1)) -Encoding utf8 } catch { }
} else {
    Report 'relaunch (session expected, not running)'
    # WHY the app was gone is the whole question, and until the exit marker existed this line could not
    # answer it: "session expected but app not running" reads identically whether the owner closed the
    # window or the process died. The app clears logs\last-exit.txt at every start and writes it only
    # from RunEvent::Exit, so present = it got there, absent = it did not. Five relaunches in the week
    # of 2026-07-27 are all recorded without this distinction and stay unattributable.
    $exitMarker = Join-Path $logDir 'last-exit.txt'
    $how = if (Test-Path $exitMarker) {
        "clean exit ($((Get-Content $exitMarker -TotalCount 1 -EA SilentlyContinue)))"
    } else {
        'NO exit marker - died without reaching shutdown'
    }
    Write-Log "session expected but app not running - relaunching [$how]"
}
if ($DryRun) { exit 0 }
if (-not (Test-Path $exe)) {
    Write-Log "exe missing at $exe - nothing to launch (mid-rebuild?)"
    exit 1
}
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
Write-Log "launched $exe"
