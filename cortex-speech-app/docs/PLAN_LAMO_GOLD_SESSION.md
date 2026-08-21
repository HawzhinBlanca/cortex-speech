# PLAN — the Lamo gold session: reviewer incentives, and a corpus VoxCPM2 can actually learn from

**Written 2026-08-21.** Every number below was measured on the live database or the live audio, not
estimated. Where something is an owner decision it says **OWNER**.

---

## The one thing to read if you read nothing else

**Do not ship the coin panel before the four integrity fixes in §2.** Not for tidiness — because of a
specific, measured migration:

* The system pays on `reviewed_audio_ms`, whose SQL whitelist is `action IN ('accept','edit','reject')`.
* `db.rs` sets `noticed = 1` **unconditionally** when a spot-check answer is a reject, and the
  trustworthiness report sorts by `noticed ASC` — so a reviewer who rejects everything sorts to the
  **top** of the trust list.
* Therefore, the moment money is on screen, **"reject everything" becomes the globally optimal
  strategy**: highest pay, lowest effort, best trust score, and it destroys clips permanently.

Adding a visible price to this pipeline without fixing that is not a UI change. It is an instruction.

---

## 1. What is true today (measured)

### The corpus

| | |
|---|---|
| segments / audio in library | 23,451 / 54.89 h |
| **human-decided so far** | **668 (1.66 h) — 2.8 %** |
| clip duration p10 / median / p90 | 6.45 s / 8.45 s / 10.45 s |
| Lamo gold set | 6,922 clips / 15.20 h, 0 duplicates, 0 clipping, SNR median 34.7 dB |

### The pay arithmetic, honestly

At 18,000 IQD per hour of **audio reviewed**, one clip is worth **≈42 IQD**. But honest review runs at
about **2.5× realtime**, so:

| reviewer | decisions | median s/clip | accept % | spot-checks noticed | **IQD per hour of their life** |
|---|---|---|---|---|---|
| Rubar | 482 | 21.0 | **56 %** | **0.33** | 7,236 |
| Sewa | 168 | 21.6 | 24 % | 0.88 | 7,052 |
| Hawzhin (owner) | 159 | 42.2 | 11 % | **1.00** | **3,600** |
| Lamo | 32 | 26.7 | 16 % | 0.50 | 5,706 |
| Roza | 6 | 18.2 | 57 % | 0.00 | 8,356 |

Two facts that must shape the design:

1. **The most careful reviewer earns the least** — 3,600 IQD/hour, exactly 20 % of the headline
   number. A panel advertising "18,000/hour" to someone receiving 3,600 does not just expose the
   arbitrage, it manufactures the grievance that justifies taking it.
2. **The highest-volume reviewer already has the worst judgement score** — 56 % of all decided work,
   56 % bare accepts, missing two traps in three. That gradient is being climbed *today, with no money
   on screen.* The panel would supply the reward signal that is currently missing.

**OWNER DECISION — the headline rate.** Either
(a) keep 18,000 IQD per **audio-hour** and label the panel honestly as such, so nobody reads it as an
hourly wage; or
(b) pay per **labour-hour** at a rate you choose, which removes the speed incentive entirely and
makes careful work pay the same as fast work. **(b) is what I would choose** — it is the only option
where the careful reviewer is not punished, and it deletes exploits 1–3 at the root rather than
policing them.

### The measurement capacity nobody knows is exhausted

Reconstructing the spot-check filter chain against the live database:

```
verified = 1 AND raw_transcript <> ''                 →  668
AND (is_gold = 1 OR reviewed_by IS NULL)              →   53
AND the human text differs from the draft             →   26     ← the entire trap pool
```

* `is_gold = 0` on **all 23,492 rows** — there is no gold in this database.
* Lifetime coverage is **26 / 22,783 = 0.11 %** of remaining work. "1 in 8" is a serving cadence, not
  a coverage rate.
* At 4 traps per 25-clip batch a reviewer **burns all 26 keys in about one hour**, after which the
  pool returns empty and measurement ends **silently** — it is counted, never alerted.
* The pool does not grow with headcount: every reviewer gets the same 26 keys in `ORDER BY id ASC`,
  i.e. the same clips in the same order. Two reviewers comparing notes defeat it in one conversation.

**This is the real blocker on paying anyone by results.** Fix it in §2.4.

---

## 2. Integrity fixes — prerequisites for the coin panel

Ordered by (damage prevented ÷ effort). The first three are one-line changes.

### 2.1 Stop paying for rejects — **one word**

`reviewed_audio_ms` whitelists `'reject'`. A reject is a claim that no work product exists; paying for
it funds deletion. The same file already excludes `skip` for exactly this reason — *"counting those
would credit somebody for work they explicitly did not do."*

→ Remove `'reject'` from the whitelist. Reviewers are still **expected** to reject bad clips; they are
simply not paid by the hour for doing it. Pair with §2.2 so honesty is not punished.

### 2.2 A reject on a spot check is WRONG, not right — **one boolean**

A served spot check is by construction a clip with a real human transcript. Rejecting it is a wrong
answer. Today `action == "reject"` sets `noticed = 1` unconditionally, so a 100 %-rejector scores
perfect and sorts to the top of the trust report.

→ On a served spot check, a reject scores `noticed = 0`.

### 2.3 `noticed` must mean "found the error", not "typed something" — **small**

Today `noticed` is true if *any character changed*. A trailing space, a doubled letter or a swapped
ZWNJ scores 1.00 forever at accept-level effort — which defeats the only mechanism catching blind
accepts.

→ Score by edit distance against the answer key: the submitted text must be **closer to the key than
the draft was**. Anything else is not noticing.

### 2.4 Refill the trap pool — **the real work, and it unblocks everything**

26 keys cannot measure 15 hours of paid review. Two sources, both already supported:

* **Gold-flag the owner's own decided rows.** `is_gold` is 0 everywhere; the flag exists and the
  query already honours it (`is_gold = 1 OR reviewed_by IS NULL`).
* **Plant known-answer clips from the Lamo set itself** as the owner reviews the first tranche.

Target: **≥ 200 keys before review opens**, and an **alert when the pool runs dry** instead of today's
silence. Without this, everything downstream is unmeasured.

### 2.5 Close the blind-accept laundering path — **important, subtle**

`training_transcript_with_source` returns `(raw_transcript, "human_verified")` for a bare accept with
no verdict text. A blind accept therefore stamps **champion output at 7.03 % CER as human gold**. Three
compounding harms:

* Fine-tuning on it is **self-distillation on our own errors** — a floor the model can never get below.
* Every CER measured against that gold **understates true error**: the champion is graded against its
  own transcripts, so the better the exploit scales, the better it looks.
* It is invisible to every content guard, and `authoritative_decision` **cannot fire by construction** —
  it reclassifies an accept whose text matches *no* ASR hypothesis, and a blind accept matches the
  champion hypothesis exactly.

→ Minimum: record, per decision, whether the accepted text was **byte-identical to the draft**, and
expose the per-reviewer bare-accept rate in `reviewer_throughput` (one extra column on a query that
already walks every event). The 56 %-vs-11 % spread is already stored and **no query reads it**.

---

## 3. The reviewer panel — spec

A single panel under `#progress` in `couch.html`. The page has **no `data-testid` anywhere** and is
addressed by `id`; keep that convention.

### 3.1 What it shows

```
  ⏱  اسکەکان: 12m 40s        ← time worked this session (wall clock, live)
  🎧  دەنگ: 1h 12m            ← audio reviewed (reviewedMs — already sent by the server)
  🪙  ٥٤,٠٠٠ IQD              ← earnings, from the same number the owner pays on
  ▓▓▓▓▓▓▓░░  85%             ← listening progress for THIS clip
```

Four deliberate choices:

1. **Seconds worked is wall clock, session-local.** The server has no wall-clock notion of a session
   and should not grow one; the phone can measure it honestly and it resets on reload. Label it
   "this session" so it is never mistaken for a payroll record.
2. **Earnings derive from `reviewed_audio_ms`, the exact number the owner pays on.** No second
   accounting. `reviewedMs` already arrives on every queue fetch, decision reply and undo — the panel
   needs **no new endpoint**.
3. **The listening bar is the important part and it is new value, not decoration.** The 0.85 playback
   gate is currently *invisible until it refuses*: the reviewer taps accept, gets a 428, and sees
   "you must listen" with no idea how close they were. Showing progress toward the bar converts a
   punishment into guidance and will reduce refusals immediately.
4. **Coins are a counter, not a game.** No streaks, no combos, no leaderboard. A leaderboard on this
   pipeline would be a race, and §1 shows exactly who wins a race.

### 3.2 The gate that makes it safe

Coins **stop accruing**, with a visible reason, when the reviewer's rolling spot-check score over
their last 10 checks falls below 0.6. One `GROUP BY` over `spot_checks`, data already stored. On
today's numbers it trips exactly one person and nobody else.

Displayed as *"checks: 7/10 — coins paused"*, never as an accusation. The reviewer can always see why
and can always recover.

### 3.3 Implementation notes (learned from the page, not guessed)

* Strings go in the inline `<script id="i18n">` table — **en, ckb and source key sets must match
  exactly** or `test_couch_page_i18n.py` fails. A `source: null` key must also be added to that test's
  acknowledged-unreviewed list, in the same commit.
* Any new panel must re-render from inside `applyLocale()` or it will not follow the language toggle.
* `[hidden] { display: none !important }` exists because `#card` sets `display:flex` — a new panel with
  its own `display` rule inherits that trap.
* The page is `dir="rtl"`, `lang="ckb"`, Kurdish-first. Numerals should render in the same convention
  as the rest of the page.

---

## 4. The corpus VoxCPM2 needs — what changes versus an ASR corpus

### 4.1 Do this before anything else

**Measure tokens-per-character of Sorani under VoxCPM2's own tokenizer.** VoxCPM-class backbones are
trained overwhelmingly on English and Chinese; Sorani in Arabic script may be out-of-distribution for
the **tokenizer**, not merely the acoustics. If it fragments above ~1.5 tokens/char we are at
byte-fallback: sequences inflate 3–4×, the text encoder has almost no prior, and **the fine-tune is
teaching the language, not just the voice**.

This single measurement moves every other number in this plan — including whether 15 h is enough
(a voice in a language the model knows needs ~30 min; a language it does not know needs **6–10 h
minimum, 10–30 h for robustness**). 15 h is a sound target *if* tokenization is reasonable.

### 4.2 Transcript rules the reviewers must follow

The invariant is **bidirectional**: everything audible must be recoverable from the text, and
everything in the text must be present in the audio.

| | rule |
|---|---|
| Numbers, dates, currency, units, ordinals, % | **spell out in Kurdish words**, never digits (not ASCII, not ٠١٢, not ۰۱۲) |
| Abbreviations / acronyms | expand to spoken form |
| Foreign / loan words | transliterate into Kurdish script as **this speaker** says them — no Latin left in an RTL stream |
| Disfluencies, stutters, false starts, laughter, coughs | **mark the clip bad** — do not transcribe them, do not silently omit them |
| Ezafe | transcribe **every pronounced** ezafe |

The catastrophic failure is not any single rule — it is **inconsistency**. Sometimes-marked is worse
than never-marked.

### 4.3 Sorani orthographic normalization — the defect unique to this language

The tokenizer is deterministic over codepoints, so two spellings of one word are two disjoint token
sequences sharing **no parameters**. This silently halves effective examples per word form.

One shared function, used for training text **and** inference text from the same code path:

* **ه (U+0647) vs ە (U+06D5)** — word-final *e* must be **ە**. The most common Sorani error in found text.
* **ك (U+0643) → ک (U+06A9)**, **ي (U+064A)/ى (U+0649) → ی (U+06CC)**
* verify **ڕ ڵ ۆ ێ** are real codepoints, not base letter + combining mark
* strip tatweel, harakat, bidi controls; decide ZWNJ policy and enforce it **everywhere or nowhere**
* NFC-normalize, collapse whitespace
* fix **و** conventions once (attached/separate conjunction; وو for long /uː/)

**This is a code change, not a reviewer instruction** — reviewers must not be asked to hand-normalize
codepoints. Normalize on save, and show the normalized text back to them.

### 4.4 Punctuation is the model's only prosody control

* It must be **present** — an unpunctuated corpus yields a model with no pause control.
* It must be **accurate to delivery, not to grammar**: a comma where the speaker actually paused, a
  period where F0 actually fell, a question mark only on real question intonation. Grammar-derived
  punctuation decorrelates the token from the acoustic event and drives its learned effect toward zero.
* **Small closed inventory**: normalize ، vs `,` and ؟ vs `?`; delete quotes, parentheses, dashes,
  ellipses, semicolons.

This is a **reviewer instruction** and belongs in their brief.

### 4.5 Audio

Our set is already 24 kHz / 16-bit / mono / −20 LUFS with zero clipping — good. Remaining points:

* **Resample once, from the master**, with a high-quality resampler, to VoxCPM2's native rate. Never
  upsample. Keep the 24 kHz masters archived so a different rate can be re-cut without re-deriving.
* Verify **true peak ≤ −1 dBTP** (headroom for inter-sample peaks after resampling) and that loudness
  was applied **per session, not per clip** — per-clip normalization flattens the speaker's natural
  dynamics, which is exactly the prosody we want the model to learn.
* Export must pair transcripts with the **24 kHz masters**, never the 16 kHz ASR working copies.

---

## 5. Production sequence

1. **Purge the old-path Lamo rows.** The gold clips are byte-identical copies at a new path (verified
   400/400). Leaving both means reviewers are served the same audio twice. **Hard prerequisite.**
2. Finish the champion draft of the 6,922 gold clips (running).
3. Land §2.1–2.3 (three tiny changes) and §2.5's accounting column.
4. Build the trap pool to ≥ 200 keys (§2.4) — **the gate on paying anyone by results**.
5. Ship the panel (§3) with its rolling-score gate.
6. **OWNER**: decide the pay basis (§1) and the focus (below).
7. Open review to all eight reviewers with the brief from §4.2/§4.4.
8. Export: verbatim human transcripts + 24 kHz masters + the normalization function's output.

**OWNER — the focus.** The Kawa_Hawleri focus narrows every queue to 1,352 ids; no Lamo clip reaches
anyone until Lamo's ids join it or it is retired. Nothing in §5 past step 6 can start without this.

---

## 6. What I would add that nobody asked for

1. **A reviewer brief in Kurdish**, one page, with five worked examples of the §4.2 rules. Reviewers
   cannot follow rules they have not been given, and inconsistency is the top-ranked corpus defect.
2. **A "why was this refused" line** wherever the system refuses a decision. The 428 already exists;
   the reviewer currently gets a toast with no path forward.
3. **A pool-exhaustion alert.** Measurement ending in silence is the failure that made §1's table
   possible.
4. **Per-reviewer accept-rate in the throughput view.** The 56 %-vs-11 % spread is stored today and
   read by nothing.
5. **A tokens-per-character check committed as a script**, so §4.1 is re-run whenever the tokenizer
   changes rather than being a one-off.
