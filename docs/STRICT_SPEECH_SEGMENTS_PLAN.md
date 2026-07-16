# STRICT conversion of `speech_segments` — plan, blocker, and correct recipe

**Status: BLOCKED on migration-framework FK handling. Owner-gated / deferred — do NOT ship as a
routine migration.** This is the last open Week-2 (storage-durability) item. The v38 pilot
(`decision_verdicts` → STRICT) established the recreate pattern; this note records why the same
pattern is **data-destroying** for `speech_segments` and what the correct conversion requires.

## Why STRICT here is low-marginal-value + high-risk

Every write to `speech_segments` already goes through a typed Rust boundary: the `SpeechSegment`
struct, `rusqlite` param binding, and `validate_segment` (db.rs). So affinity-mangled writes are
already implausible on the app path; STRICT would be defense-in-depth at the DB boundary, not a fix
for an observed bug. Meanwhile the conversion is the **highest-risk migration in the app** (see
below) and runs **unattended on the owner's next app launch** against the real production DB, which
cannot be exercised from this loop. That risk/value balance is why this is surfaced for a
supervised pass, not auto-shipped.

## The blocker (PROVEN by a regression test)

`speech_segments` is an **FK parent of seven child tables**: five with `ON DELETE CASCADE` —
`segment_hypotheses` (v4), `agent_examples` (v11), `decision_log` (v28), `decision_verdicts` (v29),
`loop0_shadow_log` (v30) — and two with `ON DELETE SET NULL` — `correction_memory.source_segment`
(v20) and `corrections.segment_id` (v21). The CASCADE children are the ones wiped by the trap
below; the SET NULL children would instead have their `segment_id` silently nulled.

SQLite cannot `ALTER … SET STRICT`; the only path is the recreate: create a STRICT twin → copy →
`DROP` the old table → `RENAME` the twin. But with `foreign_keys=ON` (the app default, set in
`Database::open`, db.rs:246), **`DROP TABLE speech_segments` performs an implicit `DELETE` of every
row, which fires `ON DELETE CASCADE` and wipes every child table.** `apply_migration` runs `up_sql`
inside `conn.unchecked_transaction()`, and **`PRAGMA foreign_keys=OFF` is a no-op inside a
transaction** — so a normal v39 migration literally cannot turn the cascade off.

This is proven, not asserted, by
`db::tests::dropping_speech_segments_cascade_deletes_children_so_strict_recreate_needs_fk_off`:
it inserts a segment + a real `decision_verdicts` child, runs the naive `DROP TABLE speech_segments`
inside a transaction exactly as `apply_migration` would, and asserts the child rows are gone. If a
future change ever makes the naive recreate "look safe", that test fails loudly.

`PRAGMA defer_foreign_keys=ON` does **not** rescue this: deferral changes *when constraint
violations are checked*, not whether `ON DELETE CASCADE` *actions* fire. The cascade still runs.

## The correct recipe (SQLite's 12-step recreate)

The conversion must run with `foreign_keys` OFF, and that PRAGMA only takes effect **outside** a
transaction. So this migration cannot use the standard transaction-wrapped `apply_migration` path;
the framework needs a one-off "FK-off" migration mode:

1. `PRAGMA foreign_keys=OFF;`   *(autocommit — NOT inside a txn)*
2. `BEGIN;`
3. `CREATE TABLE speech_segments_strict ( …all 34 columns… ) STRICT;`
4. `INSERT INTO speech_segments_strict (rowid, <34 cols>) SELECT rowid, <34 cols> FROM speech_segments;`
   — **preserve `rowid`** (the `segments_fts` external-content table uses `content_rowid=rowid`).
5. `DROP TABLE speech_segments;`  *(FK off ⇒ no cascade; also drops ALL 10 indexes + 3 triggers)*
6. `ALTER TABLE speech_segments_strict RENAME TO speech_segments;`
7. Recreate **all 10 indexes** the DROP removed (a fully-migrated table has ten, not three — miss any
   and that column's hot path becomes a full table scan). Copy the exact `CREATE INDEX` statements
   from their source migrations:
   - base (db.rs / 001_initial.sql): `idx_segments_verified(verified)`,
     `idx_segments_speaker(speaker_id)`, `idx_segments_created(created_at)`
   - v11: `idx_segments_verdict(verdict)`, `idx_segments_escalated(escalated)`
   - v13: `idx_segments_audio_path(audio_path)`  *(used by the media-security / relink path)*
   - v19: `idx_segments_verified_created(verified, created_at)`  *(the composite that makes the main
     segment-list query 100k-segment-instant — its own migration comment flags it as load-bearing)*
   - v26: `idx_segments_human_decision(human_decision)`
   - v36: `idx_segments_cloud_call(cloud_call)`, `idx_segments_confidence_source(confidence_source)`
8. Recreate the three FTS triggers `segments_ai / segments_ad / segments_au` (dropped with the old
   table) — copy them verbatim from `Database::initialize` (db.rs), the AUTHORITATIVE definitions.
9. `INSERT INTO segments_fts(segments_fts) VALUES('rebuild');`  *(resync the external-content index)*
10. `COMMIT;`
11. `PRAGMA foreign_key_check;`  *(must return zero rows)*
12. `PRAGMA foreign_keys=ON;`

Atomicity note: steps 2–10 are one transaction, so a mid-migration failure rolls back cleanly; the
only non-transactional state is the `foreign_keys` pragma, which step 12 restores (and which
`Database::open` re-asserts on every boot anyway).

## The 34 live columns (all already valid STRICT types — no remapping)

Derived from `initialize()`'s base table + every `ALTER TABLE speech_segments ADD COLUMN` migration.
**Correction to prior ledger notes:** there are **no BOOLEAN-declared columns**. `verified`,
`escalated`, `is_gold`, `cloud_call` are all declared `INTEGER`. Every column is already `TEXT`,
`INTEGER`, or `REAL` — all valid STRICT types — so the conversion needs **zero type remapping**. The
migration author should still dump `PRAGMA table_info(speech_segments)` on a fully-migrated DB to
confirm before writing the `CREATE`:

```
id TEXT PRIMARY KEY              created_at TEXT NOT NULL DEFAULT (datetime('now'))
audio_path TEXT NOT NULL         updated_at TEXT NOT NULL DEFAULT (datetime('now'))
raw_transcript TEXT NOT NULL DEFAULT ''   session_id TEXT
normalized_transcript TEXT       confidence REAL        ctc_score REAL
annotated_transcript TEXT        clipping_ratio REAL    rms_db REAL      snr_db REAL
alignment_json TEXT              split TEXT             ood_score REAL
duration_ms INTEGER NOT NULL DEFAULT 0    verdict TEXT   verdict_transcript TEXT
speaker_id TEXT                  rationale TEXT         evidence_json TEXT
verified INTEGER NOT NULL DEFAULT 0        agent_confidence REAL
escalated INTEGER NOT NULL DEFAULT 0       human_decision TEXT   corrected_at TEXT
is_gold INTEGER NOT NULL DEFAULT 0         alignment_quality TEXT
model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry'
confidence_source TEXT NOT NULL DEFAULT 'unknown'
cloud_call INTEGER NOT NULL DEFAULT 0      decoder_config_hash TEXT   normalizer_version TEXT
```

Keep the same `NOT NULL` + `DEFAULT` on each column so the `INSERT..SELECT` copy of existing rows
passes. Column *order* is free — reads use the explicit `SEGMENT_SELECT_COLUMNS` list, not
`SELECT *`, so the index-based row mapper is unaffected by physical order.

## Pre-ship checklist for the supervised pass

- [ ] Framework: add an FK-off migration mode (run outside the wrapping txn; steps 1/11/12 above).
- [ ] Migration test on a **populated** copy: rows survive with data intact; `rowid` preserved;
      FTS search still finds a row by transcript; STRICT now rejects a type-violating raw write;
      `integrity_check()=="ok"`; `foreign_key_check` empty; `PRAGMA index_list(speech_segments)`
      returns the **same 10 indexes** before and after; all seven children still present and their FK
      still resolves to the renamed table — CASCADE children still cascade AND SET NULL children
      (`correction_memory`, `corrections`) still null on parent delete.
- [ ] Update `initialize()`'s base `CREATE` is **not** required (it stays 11-col non-strict; the
      migration owns the STRICT shape) — but confirm a fresh boot ends STRICT and a second boot is a
      no-op.
- [ ] Adversarial workflow: FTS desync, partial-failure rollback, FK integrity, WITHOUT-ROWID? (no).
- [ ] Back up / snapshot the real DB before first launch on it (the restore path already exists).
