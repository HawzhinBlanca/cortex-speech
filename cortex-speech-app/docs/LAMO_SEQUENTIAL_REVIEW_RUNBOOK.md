# Lamo sequential review runbook

**Current canon (2026-08-23):** one exact 6,922-clip Lamo focus. Rubar completes the first pass;
only then does Alle receive a blind, independent second pass. Payment is outside this release path.
The only transcription model accepted for this campaign is the proven OmniASR-7B champion identity.

This document is an operating contract, not a statement that the live service is healthy. Every
session and every phase change must be proven again against the exact live files.

## Non-negotiable stop rules

- Never let Rubar and Alle work the same campaign phase concurrently.
- Never activate Alle until the atomic transition proves all 6,922 effective Rubar phone decisions.
- Never edit campaign JSON, SQLite campaign tables, decisions, or adjudications by hand.
- Never use a generic/legacy exporter for this campaign. It remains blocked even after completion.
- Never add rights or consent metadata by assumption. Missing rights correctly blocks TTS export.
- Never start or change ASR/GPU processes as part of review administration.
- Stop the review service before snapshots, release migration, phase changes, restore, or export.

## Fixed local paths

Run PowerShell from the repository root:

```powershell
$Repo = (Get-Location).Path
$Data = Join-Path $env:APPDATA 'cortex-speech'
$Db = Join-Path $Data 'cortex-speech.db'
$Focus = Join-Path $Data 'voice_focus.json'
$Admin = Join-Path $Repo 'src-tauri\target\release\campaign_admin.exe'
```

Do not continue if any resolved path is different from the intended production profile.

## Build and inspect without changing production

```powershell
cargo build --release --manifest-path "$Repo\src-tauri\Cargo.toml" --bin campaign_admin
& $Admin certify --db $Db --focus $Focus
python "$Repo\scripts\check_database_integrity.py" --db $Db
python "$Repo\scripts\check_review_serving_provenance.py" $Db
```

`certify` reads a detached in-memory snapshot: it cannot initialize, migrate, or modify production.
During first pass it binds the exact focus and reports completed and pending counts. The atomic
activation command repeats that proof under a write lock and is the only phase-change authority.

## Rubar first pass → Alle second pass

1. Stop Cortex and its watchdog; confirm no reviewer session or database writer is active.
2. Create a verified recovery snapshot and run its disposable restore drill:

```powershell
python "$Repo\scripts\create_recovery_snapshot.py" --data-dir $Data --label pre_lamo_second_pass
python "$Repo\scripts\restore_drill.py" '<the exact snapshot directory printed above>'
```

3. Start the tested Cortex release once, with reviewers still paused, so the normal app startup
   applies schema 61. Stop Cortex again.
4. Read the exact current maximum review event without modifying the database:

```powershell
$MaxEvent = python -c "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(c.execute('select coalesce(max(id),0) from review_events').fetchone()[0])" $Db
```

5. Atomically prove Rubar completion, freeze the focus in SQLite, and activate Alle:

```powershell
& $Admin activate-second-pass --db $Db --focus $Focus --expected-max-review-event-id $MaxEvent
& $Admin certify --db $Db
```

Success must report `second_pass_active`, `authorizedReviewer: Alle`, `registeredFocus.segmentCount: 6922`,
`independentPending: 6922`, zero adjudications, zero conflicts, clean SQLite checks, and zero
foreign-key violations. Any mismatch is a hard stop; there is no force option.

6. Start the tested Cortex release, run the live supervision/link/queue probes, then release only
   Alle's already-issued link. Rubar remains paused.

## Alle completion → adjudication

With Cortex stopped:

```powershell
& $Admin certify --db $Db
& $Admin adjudicate --db $Db
& $Admin certify --db $Db
```

Exact independent agreements seal automatically. Disagreements remain in `adjudication_active` and
require a separate explicit manual-adjudication JSON file; the system never guesses. A campaign is
complete only when all 6,922 clips have immutable adjudications and `conflictsRemaining` is zero.

## Purpose-bound exports

Only after `certify` reports `completed: true`:

```powershell
& $Admin export --db $Db --output 'D:\CortexDatasets\Lamo-v1' --voice Lamo
```

The command publishes separate ASR and TTS artifacts atomically. Rejected clips appear only in the
exclusion manifest. ASR audio is mono 16 kHz PCM16; TTS keeps byte-exact mono 24 kHz PCM16 masters.
Missing or revoked license, consent, source, training permission, or voice-synthesis permission blocks
the export rather than manufacturing approval.

## What old documents must not override

Historical pilot, additive 8,274-focus, multi-reviewer, payment-panel, 300M/1B fallback, and generic
export instructions are not authority for this campaign. The database policy, exact focus digest,
schema-61 immutable evidence, this runbook, and successful current-release proof commands are the
active contract.
