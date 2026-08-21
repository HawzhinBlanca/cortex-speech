# Handover — Claude → Codex, 2026-08-21

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

## 3. The one change of mine now in the tree — do not revert it again

`Database::reviewed_audio_ms` now sums `action IN ('accept', 'edit')` — **`'reject'` removed.**

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

## 5. Where I am, so we do not collide

**Running right now** (do not restart the app or kill these):

* the standalone champion server on `127.0.0.1:8799` (a `wsl.exe` process I own — the app is CLOSED,
  and the watchdog is **disabled** deliberately; do not re-enable it until the import finishes);
* `batch_importer.exe` transcribing `D:\ZAR_Lamo_15H_Gold_TTS\wavs` (6,922 clips, ~2 h left).

**Mine, in progress — please don't start these:**

* the reviewer earnings/coins panel in `assets/couch.html` (spec in `PLAN_LAMO_GOLD_SESSION.md` §3);
* the dual TTS/ASR dataset export (§4 of the plan) — though see §6, your `export_bundle.rs` work may
  already be the better foundation, in which case I will build on it instead.

**Yours, as far as I can tell — I will not touch:** `export_bundle.rs`, `export_audio/mod.rs`,
`check_spot_check_pool.py`, `check_review_serving_provenance.py`, the watchdog scripts.

**Shared, so announce before a big rewrite:** `db.rs`, `couch.rs`, `dialect.rs`.

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

We are about to open ~15 h of paid review where the fastest paid action is a bare accept — worth
roughly a 7× arbitrage over honest work — and the only thing that detects it is those 26 keys.

Your scoring fix made each key *meaningful*. Refilling the pool is what makes the measurement *exist*.
If you want one thing to take next, take that: build the pool to ≥ 200 keys and add an alert when it
runs dry. You have already been in `check_spot_check_pool.py`, so it is your ground.

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
