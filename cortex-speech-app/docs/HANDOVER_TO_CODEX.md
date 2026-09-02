# Handover — Claude → Codex, 2026-08-21

> [!IMPORTANT]
> **SUPERSEDED AS AN OPERATIONAL HANDOVER.** This file is preserved only as 2026-08-21 provenance.
> Its shared-worktree ownership, running-import, old focus/roster, hidden-check, and “highest-value
> unclaimed work” statements are not current. Use
> [`PRIVATE_PRODUCTION_10_COMPLETION_AUDIT_2026-08-24.md`](PRIVATE_PRODUCTION_10_COMPLETION_AUDIT_2026-08-24.md),
> `../../docs/OWNER_CANON.md`, the active immutable release pointer, and the generated schema-2 pool
> certification. The live authority is schema-63 flexible consensus with Rubar and Alle reviewing;
> hidden checks are not the flexible-pool completion authority.

> **Owner amendment after this handover:** section 3 records the earlier reject-zero proposal and is
> retained as history, not current canon. Canon revision 2 uses immutable policy id
> `review-iqd-v1-2026-08-21`:
> **18,000 IQD per full-equivalent audio hour; edit 100%, unchanged accept 10%, valid reject 10%,
> skip 0%.** Activity, corrected audio and payable credit are separate; old events are not silently
> repriced. The owner also authorized the additive 1,352 + 6,922 = **8,274-id** focus, but serving
> remains fail closed until release, provenance, playback and hidden-check gates pass.
> Documentation provenance: HEAD `8cbe84dd7795c9e6db45b4d9a22da503a223b9e9`, dirty shared
> implementation worktree; this amendment is not a certified release claim.

We are both working on **the same checkout, the same branch, the same uncommitted working tree**.
That has already cost one change (below). This file is the coordination surface: read §1 before your
next edit.

---

## 1. What just went wrong, so it does not happen twice

I made a one-word fix to `Database::reviewed_audio_ms` and left it uncommitted. A later write to
`db.rs` **silently reverted it**, along with its test. I have re-applied it on top of your version.

Neither of us did anything unreasonable — we just both hold the same dirty tree. Two rules fix it:

1. **Commit early and often.** An uncommitted change is not yours, it is a race. Small commits are
   cheap; a lost fix is not.
2. **Before rewriting a whole file, `git diff` it first.** If a hunk is not yours, it is mine and it
   is load-bearing.

If you would rather not coordinate at all, say so and I will move to a worktree — but note the app,
the champion server and the running import all point at this checkout, so the split is not free.

---

## 2. What you have done that I have now built on (thank you — genuinely good)

I read your uncommitted work before touching anything. Three pieces are strictly better than what I
had planned, and I have dropped my versions in favour of yours:

* **`record_spot_check` scoring.** You replaced "the text changed at all" with
  `action != "reject" && learning_text_key(submitted) == learning_text_key(expected)`. I had planned a
  weaker edit-distance rule. Yours kills the one-keystroke exploit outright *and* stops a
  reject-spammer sorting to the top of the trust report. Both were on my fix list; both are now done.
* **First spot-check answer immutable** (`ON CONFLICT DO NOTHING`). A reviewer can no longer improve a
  failed hidden check by answering again. I had not thought of that.
* **Spot-check candidates respect the voice focus.** Correct, and it closes a serving-path hole in the
  exact class that has bitten this repo four times.

Your export-provenance work (`human_decision_for_export`, `verified_without_a_human_decision_is_not_exported`,
`an_is_gold_answer_key_is_not_exported_as_training_audio`) also addresses part of the blind-accept
laundering problem I document in `docs/PLAN_LAMO_GOLD_SESSION.md` §2.5. Please keep going on it.

---

## 3. Historical reject-zero change — superseded by compensation v2

`Database::reviewed_audio_ms` now sums `action IN ('accept', 'edit')` — **`'reject'` removed.**

> This paragraph describes commit `e68c9f9`, not the prospective owner policy. Do not restore its
> reject-zero interpretation and do not make `reviewed_audio_ms` a weighted money counter. The v2
> implementation needs a durable semantic action and a policy-versioned, idempotent credit/reversal
> ledger; full reviewed activity and edit-only corrected audio remain independent projections.

This is not a style preference. It is the owner's pay rule, stated 2026-08-21:

> *"18K per hour of audio corrected, excluding the rejected ones. Reviewers get paid to correct the
> sentences; the ones that are bad they reject and they don't get paid."*

A rejected clip produces no corrected transcript, so it is unpaid — the same reasoning that already
excludes `skip`. `db_tests::reviewed_audio_ms_counts_each_clip_once_per_reviewer` pins it (a 21 s
reject must not accrue while a 9 s correction does). The doc comment above the function carries the
owner's words verbatim.

It also matters for safety: reject is the only action that destroys corpus permanently, and until
your scoring fix it was *rewarded*. Unpaid + correctly-scored removes both motives.

---

## 4. Owner preferences you should treat as binding

Learned the hard way in this session; none of it is in CLAUDE.md yet.

* **Never claim done without a real run.** Not "tests pass" — the actual measured thing. If part of a
  job failed, say exactly what and stop. An honest halt is always acceptable; a flattering "finished"
  never is.
* **Verify at the SERVING path.** Four incidents in this repo, all the same shape: a write-path check
  passed while the thing the user actually receives was wrong. The most recent — the owner heard guest
  clips on the desktop while the phone queues were correctly narrowed. Before claiming "X sees Y",
  replicate the consumer's exact query against the live DB.
* **Never weaken a gate to make something pass.** If a gate is wrong, fix the gate and say why in the
  commit; do not loosen a threshold.
* **A gate that never presents a credential proves nothing.** `check_supervision_live.py` read OK
  continuously through nine days in which six of eight reviewers could not log in at all. See
  `scripts/check_reviewer_links_live.py` for the shape that actually tests the claim.
* **Fix the greppable pin in the same commit as the rename.** A renamed symbol reds three CI platforms
  and the failure looks unrelated.
* **The owner reads commit messages.** Explain *why*, with the measured number that motivated it.

---

## 5. Historical machine state at the handover (not a live-health source)

> The bullets in this section captured commit `e68c9f9` on 2026-08-21. They are preserved for
> provenance only. Process, port, watchdog, database-schema and executable-freshness gates must be
> measured again immediately before a rollout; this section must never be used to claim the app is
> currently serving or an import is currently running.

**Was running at handover time** (the original collision warning):

* the standalone champion server on `127.0.0.1:8799` (a `wsl.exe` process I own — the app is CLOSED,
  and the watchdog is **disabled** deliberately; do not re-enable it until the import finishes);
* `batch_importer.exe` transcribing `D:\ZAR_Lamo_15H_Gold_TTS\wavs` (6,922 clips, ~2 h left).

**Was owned by the handover author at that time:**

* the reviewer earnings/coins panel in `assets/couch.html` (spec in `PLAN_LAMO_GOLD_SESSION.md` §3);
* the dual TTS/ASR dataset export (§4 of the plan) — though see §6, your `export_bundle.rs` work may
  already be the better foundation, in which case I will build on it instead.

**Was assigned to Codex at that time:** `export_bundle.rs`, `export_audio/mod.rs`,
`check_spot_check_pool.py`, `check_review_serving_provenance.py`, the watchdog scripts.

**Was shared at that time:** `db.rs`, `couch.rs`, `dialect.rs`.

---

## 6. The highest-value thing still unclaimed

**The spot-check trap pool is exhausted, and nothing alerts.** Measured on the live DB:

```
verified = 1 AND raw_transcript <> ''                 →  668
AND (is_gold = 1 OR reviewed_by IS NULL)              →   53
AND the human text differs from the draft             →   26   ← the entire trap pool
```

* `is_gold = 0` on **all 23,492 rows**.
* 26 keys against 22,783 pending clips = **0.11 % lifetime coverage**.
* At 4 traps per 25-clip batch a reviewer burns all 26 in **about an hour**, after which the pool
  returns empty and measurement ends **silently** — counted, never alerted.
* Every reviewer gets the same 26 in `ORDER BY id ASC`: the same clips, in the same order.

The original ~7× figure assumed a full-value bare accept and is superseded by accept's 10% v2 weight.
Blind acceptance can still launder champion text into apparent gold, so the hidden-check and playback
gates remain mandatory even though that old payout estimate no longer applies.

Your scoring fix made each key *meaningful*. Refilling the pool is what makes the measurement *exist*.
The handover's old ≥200 global target is now superseded: satisfy the live per-reviewer requirement
computed for the exact activated focus, and keep exhaustion fail closed. You have already been in
`check_spot_check_pool.py`, so it is your ground.

---

## 7. Context you may not have

* **Champion is locked.** OmniASR-7B via WSL for every draft; never fall back to the fine-tuned MMS or
  a CTC model, never propose Qwen/Voxtral (no `ckb`). Cloud judge is Gemini 2.5 Pro only.
* **Verbatim law.** `training_transcript` = human ▸ champion-raw. `normalized_transcript` is
  LLM-refined and is evidence only. `annotated_transcript` is human-only by law.
* **The current job** is a 15.2 h single-speaker TTS corpus (speaker "Lamo", Sorani, owner-confirmed).
  Full spec: `docs/TASK_LAMO_TTS_15H.md`. Plan and threat model:
  `docs/PLAN_LAMO_GOLD_SESSION.md`.
* **TTS needs differ from ASR needs** in ways that affect export: transcripts must be verbatim-to-audio
  *and* orthographically normalized (ه vs ە, ك→ک, ي→ی — one shared function used for training AND
  inference), punctuation must track *delivery* not grammar, and the audio must come from the **24 kHz
  masters**. Note that today **every Rust exporter emits 16 kHz and reduces the path to a basename**,
  so the masters cannot be recovered from any app export. That is the gap the dual export has to close.
