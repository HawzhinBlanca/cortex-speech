# Machine Second-Look Calibration Plan — v1 (2026-09-06)

**Status: proposed to the owner; applies to any model asked to check review drafts (today: the
cloud judge candidates `gemini-2.5-pro` (pinned) and `gemini-3.1-pro-preview`).**

## 1. What a machine second-look is, and is not

Owner canon (2026-08-12, 2026-08-29): a transcript's truth order is **human verdict ▸ annotated ▸
champion raw**; machine code never writes the annotated field; a sentence is decided by **any two
different human reviewers**; nothing enters learning from a single reviewer's raw decision.

Therefore a machine second-look is **evidence**, on the same footing as the champion draft, the
speaker-change score, and the overlap nominator. Lawful uses:

- nominate clips for a finalizer-grade human ear (accepts by lenient reviewers that the machine
  disputes; drafts the machine flags as overlap, second speaker, cut word, non-Kurdish);
- triage the two-human disagreements waiting for a third human;
- order queues; seed hidden-check candidates;
- text-only hints shown as advisory in the review UI (the existing "advisory: you still decide").

Forbidden without a literal owner canon change: counting as one of the two agreeing reviewers,
writing `annotated_transcript` or any decision table, minting gold, paying or docking anyone.

## 2. Why a 25-clip self-graded run is not calibration

The 2026-09-06 test picked the 10 shortest Lamo, 10 shortest Kawa and 5 shortest Halwest clips,
had human text for 2 of 25, and graded itself. Shortest clips are the easiest; two comparisons
prove nothing; and on one of those two the model moved the text further from the human than the
raw draft was. A calibration needs an **independent answer key**, a **representative sample**, a
**frozen configuration**, and **human baselines** measured the same way.

## 3. Answer keys available today (read-only counts, 2026-09-06)

| Tier | Definition | Count | Use |
|---|---|---|---|
| A | keep decision (accept/edit) by a finalizer-grade reviewer (owner, or the two reviewers the owner trusts) on a canonical pool clip | 188 clips (Lamo 171, Kawa 14, Halwest 3) | primary accuracy key |
| B | two different humans kept the clip and their texts are identical after NFC/whitespace | 273 clips (Lamo 265, Kawa 6, Halwest 2) | secondary key; also the **over-edit** test (a machine edit here is a false alarm unless it fixes a forgiven-class or a genuine miss both humans made — the owner adjudicates a sample) |
| C | two humans kept the clip with different texts | 222 clips (Lamo 205, Kawa 15, Halwest 2) | "picks the better of two" test; not a key |
| R | human rejects (BAD) in the pool and library | 24 pool + 80 library; owner's 14 pool rejects included | reject **recall**: does the machine flag what humans threw out |
| O | owner's 32-clip blind overlap calibration set (8 controls clean, 24 flagged; owner verdicts on file) | 32 | overlap / second-speaker flag precision |

Kawa and Halwest have almost no key yet. The plan therefore reports Lamo with confidence and marks
Kawa/Halwest as *indicative* until the owner blind-judges 20 + 20 machine outputs for those voices
(served through the listen list, exactly as the reviewer audits were).

Tier B is mostly pairs of the two lenient reviewers, so B alone would flatter a lenient machine. A
is the number that counts; B is the false-alarm test.

## 4. Sample and cost

Run **all** of A, B, C, R, O: ~740 clips, about 1.7 hours of audio. At the measured 10.6 s per
clip that is ~2.2 h single-stream or ~20 min with 8 parallel calls; API cost is a few dollars per
model. Run **both** candidate models on the identical set. Hold out a random 30% of A ∪ B (seeded)
that is never used for prompt iteration; report final numbers on the hold-out only.

Stratify every report by voice and by clip-length tercile (Lamo p10/p50/p90 = 4.6 / 7.9 / 9.9 s;
Kawa 4.1 / 7.9 / 11.8 s; Halwest 4.2 / 8.5 / 13.2 s).

## 5. Frozen configuration (validity conditions)

- Exact model id string recorded per record; temperature 0; fixed thinking budget; one prompt
  whose SHA-256 is recorded per record. Any change to any of these starts a new run id.
- **Blind**: the model receives audio + the raw champion draft + the voice name only. Never the
  human text, never the reviewer's action, never the hidden-check flag.
- Prompt = the verbatim conventions v1.0 + the spelling convention page, verbatim, plus a
  structured output contract: `{"text": "...", "action": "ACCEPT|EDIT|BAD", "flags":
  ["overlap","second_speaker","cut_word","non_kurdish","unintelligible","noise"]}`.
- Self-consistency: 50 random clips run twice; report the identical-output rate.
- Database opened **read-only** (`?mode=ro`); API key sent in a header, never in the URL; no
  reviewer names in any output or log; outputs under `release-inputs/machine-opinions/<run-id>/`
  (outside the tracked tree) as JSONL with `segment_id, audio_sha256, raw_sha256, model,
  prompt_sha256, output, action, flags, latency_ms, created_at`.
- Checkpointed and idempotent on `(segment_id, audio_sha256, model, prompt_sha256)`; resume never
  re-bills a finished clip.

## 6. Metrics (all scored with the repo basis `normalize_for_metrics` + punctuation stripped)

| Metric | Definition | Human baseline to print beside it |
|---|---|---|
| letter-perfect rate | output == key after folds | owner audits of the two lenient reviewers (15% and 20% letter-perfect on their blind accepts) and the finalizer-grade reviewers' hidden-check record |
| CER vs key | char edit distance / key length | the champion draft's CER vs the same key (the machine must beat the draft it was given) |
| direction | share of clips where the output is closer to the key than the raw draft / same / further | — |
| over-edit rate | Tier B accepts changed by the machine, minus owner-adjudicated genuine catches | reviewers' no-op-edit rate |
| reject recall | Tier R clips the machine marks BAD or flags | — |
| overlap precision | Tier O flags vs owner verdicts | speaker-change probe (0.59) and the overlap nominator (~55% precision) |
| consistency | identical output on the double run | — |
| latency, cost | per clip | — |

## 7. Pass bar (proposed; the owner sets the final numbers)

A model qualifies as a **nominator** when, on the hold-out:

1. CER vs key is **lower than the champion draft's** on every voice with a key, and
2. letter-perfect rate on Tier A is **at least that of the better lenient reviewer**, and
3. over-edit rate on Tier B is **≤ 10%**, and
4. reject recall on Tier R is **≥ 70%**, and
5. the double-run identical-output rate is **≥ 90%**.

Failing 1 or 3 means the prompt is rewriting, not checking: iterate the prompt (max three rounds,
each on the non-hold-out part only) and re-run. Passing does not make it a reviewer; it makes its
flags worth a human's minute.

Model choice follows the numbers on the identical set, not the narrative. The judge pin
(`gemini-2.5-pro`) changes only on the owner's word after the two-model table is on the record.

## 8. After calibration: the full-pool runner

Only after §7 passes: run the whole canonical pool (11,244 clips, ~24 h audio) under the frozen
configuration into `machine-opinions/`, then build the **nomination list** — machine disputes of
lenient accepts, BAD/overlap flags, disagreement triage — and feed it to finalizer-grade reviewers
through the existing listen list. Humans still decide every clip; the machine only chooses what
they hear first.

## 9. Reporting

One table per model per voice per tercile, plus the human baselines, in the progress ledger and the
vault, with run id, model id, prompt hash and the exact SQL that produced the key set. Any claim of
"gold-quality review" without this table is a narrative, not a measurement.
