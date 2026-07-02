# M2 · Instrument-before-marathon (Structure & Checklist)

**Purpose**: Land all instrumentation **before** M3 (the owner's gold marathon, weeks of real review). Every decision logged, every verdict recorded, every timing measured.

## M2.1 · Decision timing (decision_log table)

**Database migration v27** (add to `src-tauri/src/migrations/mod.rs`):
```sql
CREATE TABLE decision_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    segment_id TEXT NOT NULL,
    decision_type TEXT NOT NULL, -- 'accept', 'reject', 'edit'
    timestamp_ms INTEGER NOT NULL, -- ms since segment came into focus
    human_decision TEXT, -- the enum value (accept/escalate/reject)
    created_at TEXT DEFAULT (datetime('now')),
    FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
);
CREATE INDEX idx_decision_log_segment_id ON decision_log(segment_id);
```

**Code**: Record timing in `src-tauri/src/commands.rs` at `record_human_decision()` call site.

**Gate**: After 10 real decisions, stats panel shows median s/segment from decision_log rows.

## M2.2 · Per-segment T0/T1 verdict rows

**Add to `speech_segments` table schema** (migration v28):
```sql
ALTER TABLE speech_segments ADD COLUMN auto_accept_verdict TEXT; -- 'T0_ACCEPT' | 'T1_ESCALATE' | NULL
ALTER TABLE speech_segments ADD COLUMN verdict_computed_at TEXT; -- when jury ran
CREATE INDEX idx_segments_verdict ON speech_segments(auto_accept_verdict);
```

**Code**: Persist jury verdict at import in `src-tauri/src/pipeline.rs` after `run_jury()`.

**Gate**: After one import, every segment has a verdict row visible in DB query.

## M2.3 · LOOP-0 shadow logging

**Add to decision_log table**:
```sql
-- Add column to track would-fire events without mutating
ALTER TABLE decision_log ADD COLUMN loop0_would_fire BOOLEAN;
```

**Code**: In `src-tauri/src/pipeline.rs` `apply_loop0_firing()`, when shadow mode is ON, log events to decision_log instead of mutating. Count would-fire mismatches with human decisions later in M3.

## M2.4 · Alignment at import (background job)

**Code**: After each segment's ASR in import (pipeline.rs), enqueue `align_segment()` on a background low-priority worker.

**Gate**: Fresh import → review immediately → word chips present, no "Aligning words..." spinner; 100% coverage within 5 min.

## M2.5 · Suspect-first queue

**Code**: Order `ReviewInbox` segments by:
1. Jury escalated (highest priority)
2. Correction-memory term hits (second)
3. Duration outliers (third)
4. Default: chronological

**Gate**: Measured in M3 (same session, switch queue mode, compare decisions/hour).

## M2.6 · Full session restore

**Code**: Persist review cursor (segment ID, scroll offset, active filter, queue mode) on every decision keystroke in `src-tauri/src/commands.rs` `record_human_decision()`.

**Gate (drill)**: Kill exe mid-review, relaunch → lands on same segment with same queue order.

---

## Why M2 Must Land Before M3

M3 is the owner's review marathon (≥500 human decisions, weeks). Each decision:
- IS a **timing sample** (decision_log)
- IS a **jury ground truth** (verdict rows)
- IS a **LOOP-0 validation** (shadow log)
- IS a **training pair** (corrections export)
- IS a **domain gold** (boundary-aligned by construction)

If M2 instrumentation isn't in place, the entire M3 marathon produces ZERO measurable data. All 500 decisions are wasted effort.

---

## Checklist (to complete M2 from this scaffold)

- [ ] M2.1: Migration v27 (decision_log table) — write & test
- [ ] M2.1: Record timing in record_human_decision()
- [ ] M2.1: Stats panel shows median s/segment
- [ ] M2.2: Migration v28 (verdict column on speech_segments)
- [ ] M2.2: Persist jury verdicts at import
- [ ] M2.3: LOOP-0 shadow logging (decision_log column)
- [ ] M2.4: Alignment at import (background worker)
- [ ] M2.5: Suspect-first queue (ReviewInbox ordering)
- [ ] M2.6: Session restore (persist review cursor)
- [ ] All: Tests pass (cargo test, vitest)
- [ ] All: E2E smoke test (import → review → 10 decisions → stats visible)

**Estimated effort**: 6-8 code hours (migrations + command updates + background worker + UI changes).

---

## M3: The Owner's Review Marathon

Once M2 is landed, M3 begins: the owner imports real audio, 7B drafts, uses the keyboard-fast review flow to verify ≥500 segments. Every decision is quintuple-counted (gold, jury truth, LOOP-0 validation, timing, training pair). M3 spans weeks of normal use; no extra effort needed beyond reviewing what you would review anyway. M2 just makes every review **measurable**.
