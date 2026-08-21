# TASK — Build a 15-hour, human-verified TTS fine-tuning set for the speaker "Lamo"

**For:** the next agent picking this up. You do not need the conversation that produced it.
**Owner:** Hawzhin. Anything marked **OWNER-GATED** stops until he answers — do not decide it yourself.
**Written:** 2026-08-21, after the measurement pass described in §1. Re-measure anything older than a
day; the source folder changes constantly.

---

## 0. Read these first (non-negotiable, they override anything below)

* `cortex-speech-app/CLAUDE.md` — the honesty law, the model lock, halt-on-first-failure.
* `docs/OWNER_CANON.md` — approved-and-final decisions. Changing one needs the owner writing
  `change canon:` himself.
* Key rules that bite on this task specifically:
  * **The champion drafts everything.** `asr_model_size = WSL7B`. Never let a clip fall to the
    fine-tuned MMS or a CTC model. Never suggest a different ASR family (Qwen/Voxtral have no `ckb`).
  * **Stop on the first failure.** No skipping a bad clip and carrying on. A partly-drafted set that
    looks finished is worse than a run that stopped.
  * **Never invent a number.** Every figure you report comes from a real run you can point at.
  * **Never weaken a gate** to make something pass.

---

## 1. What is already true (measured — do not re-derive, do not contradict without re-measuring)

**The dataset** lives at `D:\ZAR_Lamo_TTS_Dataset\`:

| | |
|---|---|
| `wavs\` | the working set — 24 kHz 16-bit mono PCM, loudness-normalised to −20 LUFS |
| `metadata.csv` | `audio_path\|speaker\|duration\|similarity_score` — **no transcript column**; that is what this task produces |
| `duplicates_exact\` | 4,348 exact-duplicate clips quarantined 2026-08-21 (see §2) |
| `rejected_other_speakers\`, `quarantined_reverify\` | the owner's own earlier rejects — leave alone |
| `quality_report.csv` | output of `scripts/audit_tts_clip_quality.py` |

**Audio quality is already gold. There is nothing to cull on acoustic grounds.** Measured across
20,582 clips (43.92 h):

```
SNR dB       : min 15.9 | p10 25.6 | median 32.5 | max 78.2   (the app calls <5 dB poor)
clipping     : 0 clips have a single clipped sample
RMS dBFS     : p10 −25.6 | median −22.4 | p90 −19.8
malformed    : 0
tail silence : max 0.5 s
similarity   : 100% scored, min 0.650, median 0.738
```

**The one real defect, and nothing else checks it:** ~9% of clips are cut through a word
(4.7% start mid-word, 5.0% end mid-word, 91.1% clean on both edges; random sample of 3,000).
SNR and speaker-similarity are both blind to it. For TTS it is corrosive — the model learns to chop
onsets and swallow word endings.

**Selection arithmetic** (from `quality_report.csv`):

```
similarity floor   clips    hours     within 3–11 s
        0.65      20,582    43.92     19,583 / 40.98 h
        0.70      15,419    33.77     14,597 / 31.24 h
        0.72      12,764    28.30     12,053 / 26.10 h
        0.75       8,594    19.40      8,079 / 17.78 h
        0.80       2,831     6.67      2,605 /  5.95 h
```

**Already in the library:** 9,650 Lamo rows imported, 1:1 with their source wavs. **2,104 of them
(21.8%) now point at audio the owner has since deleted** — see §3, they must be purged.

---

## 2. Design decisions already made, with the reasoning (do not silently reverse these)

1. **Similarity is a FLOOR, never a RANKING.** Speaker embeddings score highest on clean, steady,
   unexpressive speech, so ranking by them selects a narrow slice of the speaker's range and the
   resulting voice sounds flat. Use ≥ 0.72 to exclude doubt; do not sort by it.
2. **The transcript is the binding constraint, not the audio.** The champion is ~7% CER on Sorani —
   excellent for ASR, not good enough to train a voice on unread. Human verification is the point of
   this task, not an optional polish step.
3. **One wav = one segment.** `max_segment_duration_ms` was raised 10000 → **15000** in
   `%APPDATA%\cortex-speech\settings.json`. Every clip is under 12 s, so `needs_chunking` is false and
   each file becomes exactly one DB row. **At 10000 this VAD-splits 670 clips into several segments
   and destroys the 1-wav = 1-transcript pairing TTS needs.** Verify this setting before every import.
4. **The source is mapped as `sorani`** in `src-tauri/src/dialect.rs`
   (`(r"ZAR_Lamo_TTS_Dataset\", SORANI)`), on the evidence of the `ZAR` prefix (ZarPodcast, filed by
   the owner under `Kurdish Corpora\sorani\`). An UNMAPPED source fails closed — every restricted
   reviewer is served nothing from it, silently. **OWNER-GATED if it turns out Lamo is Hawleri:** one
   line, but ask before changing.
5. **Exact duplicates are quarantined, not deleted.** 4,348 moved to `duplicates_exact\`. The
   extractor emits them at 22.4% with a systematic offset-56 pattern
   (`lamo_005630`↔`005686`, `005631`↔`005687`, …). Expect more; re-scan before each import.

---

## 3. The steps

### Step 0 — confirm the source has stopped moving

**Do not start while the owner's extractor is running.** Measured 2026-08-21: it added ~1,860
clips/hour and deleted thousands more, while the importer sustains ~970 clips/hour. Under that churn
21.8% of transcription work was thrown away and the import halted on a file that vanished mid-run.

```bash
ls "D:/ZAR_Lamo_TTS_Dataset/wavs" | wc -l    # run twice, 10 minutes apart
```

Identical counts twice → proceed. Different → **stop and ask the owner**; nothing below converges
against a moving folder.

### Step 1 — purge the dead library rows

Rows whose audio the owner deleted. They are unplayable, so queues silently skip them, and they carry
a real hazard: if the extractor regenerates a filename with **different** audio, the resume logic in
Step 4 skips that path as "already imported" and the old transcript stays attached to new speech —
a transcript/audio mismatch nothing would flag.

Confirm they are machine-only before removing anything:

```bash
# expect: any human touch = 0, review_events = 0
python - <<'EOF'
import os, sqlite3
from pathlib import Path
d = Path(os.environ['APPDATA'])/'cortex-speech'
db = sqlite3.connect(f"file:{d/'cortex-speech.db'}?mode=ro", uri=True)
rows = db.execute("""SELECT id, audio_path, verified, COALESCE(human_decision,''),
                            COALESCE(reviewed_by,''), COALESCE(is_gold,0)
                     FROM speech_segments WHERE audio_path LIKE '%ZAR_Lamo%'""").fetchall()
dead = [r for r in rows if not Path(r[1]).is_file()]
print("dead:", len(dead), "| human-touched:", sum(1 for r in dead if r[2] or r[3] or r[4] or r[5]))
EOF
```

If any row is human-touched, **stop and ask** — a human verdict is never discarded silently.
Otherwise delete them through the app's own deletion path (`delete_segments_batch`), not raw SQL:
raw `DELETE` desynchronises the FTS index and the child tables.

### Step 2 — re-scan and quarantine duplicates

```bash
python scripts/audit_tts_clip_quality.py "D:/ZAR_Lamo_TTS_Dataset/wavs" \
    --out "D:/ZAR_Lamo_TTS_Dataset/quality_report.csv"
```

Then hash decoded audio, group, and **move** (never delete) redundant copies to `duplicates_exact\`,
keeping the copy already in the library if there is one. **Never move a file the database points at**
— it breaks that row's `audio_path`.

### Step 3 — build the candidate pool (~24 h, not 44 h)

Apply, in order:

1. `similarity >= 0.72`
2. `3.0 <= duration <= 11.0` seconds
3. **drop boundary-truncated clips** — speech energy in the first or last 30 ms, measured against the
   clip's own noise floor (threshold: `max(noise_floor * 4, 0.01)`). ~9% will fail.

Expected: ~11,000 clips / ~23.7 h. Write the surviving filenames to a manifest; do **not** move the
losers out of `wavs\` (moving them changes what Step 4 imports and what future runs see).

**This is ~45% less GPU than transcribing everything, and it removes the only defect that damages TTS.**

### Step 4 — transcribe with the champion

The 7B server is a **child of the running app**, and `batch_importer` refuses to run while the app is
open (shared instance lock). So the app must be closed *and* the server started standalone:

```powershell
# 1. stop the watchdog FIRST or it relaunches the app mid-import and everything collides
Disable-ScheduledTask -TaskName CortexWatchdog
Stop-Process -Name cortex-speech-app -Confirm:$false

# 2. verify settings (app is closed, so settings.json is safe to read/edit)
#    max_segment_duration_ms == 15000, asr_model_size == WSL7B,
#    use_finetuned_asr == false, llm_mode == None

# 3. standalone champion — MUST be launched from PowerShell, not Git Bash:
#    Git Bash rewrites /home/ai/... into C:/Program Files/Git/home/ai/... and it dies with exit 127.
$ptr = (wsl -- wslpath -a ((Join-Path $env:APPDATA 'cortex-speech\champion.json') -replace '\','/')).Trim()
$srv = (wsl -- wslpath -a ((Join-Path $PWD 'scripts\cortex_7b_server.py') -replace '\','/')).Trim()
wsl -- env CORTEX_7B_CHAMPION_POINTER=$ptr CORTEX_7B_PORT=8799 `
    /home/ai/.venv-wsl-whisper/bin/python $srv
```

Hold that process alive for the whole import — it is not detached.

**Before importing, verify the served model is the pinned champion.** Send `{"op":"health"}\n` to
127.0.0.1:8799 and require `deploymentSha256` to equal
`champions["omniasr-7b"].deploymentSha256` in `%APPDATA%\cortex-speech\champion.json`
(currently `ae33143ec8b25f45e393f4aa484c3a3d165850f0dc15e95254dd6e4cb4c05cbf`). A mismatch means you
are about to draft a corpus with the wrong model — **stop**.

Then:

```powershell
$env:CORTEX_7B_PORT = "8799"
& ".\target\release\batch_importer.exe" "D:\ZAR_Lamo_TTS_Dataset\wavs"
```

**Re-running is a RESUME, not a fresh import.** `batch_importer` now hands the pipeline the set of
paths the library already holds; it prints `Resuming: N file(s) …`. If you ever see
`Fresh import:` when the library already holds clips from that folder, **stop** — without the resume
set, `AudioFingerprint::new()` starts empty, cannot see the previous run, and re-persists every file
a second time under the same `audio_path` (this doubled 494 reviewed clips on 2026-08-14).

**Expect halts. They are the system working.** After each one: read the reason, fix that cause, re-run
(the resume makes it cheap). Two seen so far — `Duplicate audio content` (Step 2 fixes it) and
`I/O error: cannot find the file` (Step 0 fixes it).

**GPU:** two replicas, ~19 GB per card of 24.5 GB. Never start a third GPU workload alongside. Clock
locking (`nvidia-smi -lgc 1500,2055`) needs Administrator and will be refused — a continuous import
keeps the cards clocked up anyway, so do not chase it.

### Step 5 — verify the import before going further

```bash
python - <<'EOF'
import os, sqlite3
from pathlib import Path
d = Path(os.environ['APPDATA'])/'cortex-speech'
db = sqlite3.connect(f"file:{d/'cortex-speech.db'}?mode=ro", uri=True)
rows = db.execute("""SELECT audio_path, COUNT(*) c FROM speech_segments
                     WHERE audio_path LIKE '%ZAR_Lamo%' GROUP BY audio_path""").fetchall()
bad = [p for p, c in rows if c != 1]
print("wavs:", len(rows), "| not 1:1:", len(bad))
EOF
```

Required: **not 1:1 == 0**, zero empty transcripts, zero rows whose audio is missing. Anything else
means Step 3 or 4 went wrong — fix it before a human spends time reviewing.

### Step 6 — cut the final 15 h from the TRANSCRIPTS

Only now, with text available:

* **Maximise grapheme and sentence-length coverage.** This is what makes a TTS voice pronounce
  unfamiliar words correctly; a 15 h set of short flat declaratives renders worse than a balanced 10 h.
* **Drop anomalous characters-per-second.** Sorani here runs ~13 chars/s (99 chars over ~7.5 s).
  Far outside that band means a wrong transcript, music, or dead air — a reliable automatic signal.
* Keep the pool's prosodic spread; do not re-sort by similarity here either.

### Step 7 — human review (the actual quality step)

~7,200 clips ≈ **55 person-hours**.

**Lamo's clips are Sorani, so ALL EIGHT reviewers can judge them** — including the five who have had
an empty queue for days. That is ~7 hours each.

**OWNER-GATED — nothing reaches any reviewer until this is answered.** The active voice focus
(`%APPDATA%\cortex-speech\voice_focus.json`, currently a different speaker, 1,352 ids) narrows
**every** queue — phone and desktop — to those ids. Ask the owner to either:
  * **(a)** add Lamo's segment ids to the focus so both collections run and the idle reviewers get
    work — use `scripts/activate_voice_focus.py`, never hand-edit the file; or
  * **(b)** keep the current focus exclusive, and Lamo waits.

Do not edit `voice_focus.json` yourself. A focus that exists but cannot be parsed serves **nothing**
to **anyone** (fail-closed), so a careless edit stops all eight reviewers at once.

Verify reviewers can actually work before telling anyone to start:

```bash
python scripts/check_reviewer_links_live.py     # presents each reviewer's real credential
python scripts/check_reviewer_queues_live.py    # what each of them would actually be served
```

### Step 8 — export

Pair the **verbatim human-verified** transcript with the **24 kHz master** (never the 16 kHz working
copy — the app downsamples for ASR only; the originals in `wavs\` are the training audio).

Verbatim law: transcript = human ▸ champion-raw. Refined/LLM text is evidence only and never becomes
the transcript. `llm_mode` stays `None`.

---

## 4. Acceptance criteria

- [ ] Source folder stable across two counts 10 minutes apart.
- [ ] Zero library rows pointing at missing audio.
- [ ] Every imported wav is exactly one segment (`not 1:1 == 0`).
- [ ] Zero empty or placeholder transcripts in the delivered set.
- [ ] Zero exact-duplicate audio in the delivered set.
- [ ] Zero boundary-truncated clips in the delivered set.
- [ ] Every delivered clip carries a **human** decision (verified, attributed, with playback evidence).
- [ ] Export pairs 24 kHz masters with verbatim transcripts.
- [ ] Every number in the final report traceable to a command that produced it.

## 5. Do NOT

- Do not weaken `max_segment_duration_ms` back to 10000 "to keep clips short".
- Do not let a clip fall to a non-champion ASR, ever.
- Do not turn on LLM refinement to "clean up" transcripts.
- Do not delete anything from the owner's dataset folder — **move** to a sibling folder.
- Do not move or delete a file the database points at.
- Do not hand-edit `voice_focus.json` or `reviewer_dialects.json`.
- Do not run a second GPU workload beside the two champion replicas.
- Do not report progress in clips when the owner asked for hours, or estimate either.
