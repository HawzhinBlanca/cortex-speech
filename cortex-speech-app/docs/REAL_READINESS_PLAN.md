# Cortex — The Real Readiness Plan (to a *measured* 10/10)

Written after a hard, fair correction from the user: too much was called "done" / "10/10" on the
basis of passing tests, while the app the user actually touches was not usable and nothing had been
measured on their data. This plan fixes the standard, not just the features.

## The standing rule — brutal reality check (binding)

1. **Nothing is "done" until it is either USER-OBSERVABLE (you can see/use it) or MEASURED against
   your real audio with a number + confidence interval.**
2. **"Tests pass", "clippy clean", "verify_10 ALL GATES GREEN" are NECESSARY but are NOT "done" and
   NOT "10/10".** They mean the code compiles and narrow CI checks pass — nothing about whether the
   app is good for you. The `verify_10` gate checks manifests/licenses/ledger schema, period.
3. **Every status starts with the honest reality, then the progress.** No leading with the rosy view.
4. **I will never again mark a feature "done" or call the system "10/10" on tests alone**, or invite
   you into something as if it were finished when it is a foundation.

## Where we actually are (honest, today)

- **Built + tested (the engine room, invisible to you):** the correction-learning machinery, the
  model registry + WER∧CER promotion gate, calibration measurement (ECE), the gold-regression gate
  mechanism, provenance. Real code, real tests — but you can't see or use any of it.
- **NOT real for you:** the transcription is the weak built-in CTC engine; your 7B / Gemini /
  ElevenLabs are NOT wired in as the transcriber; the editing screen is half-built (no clean inline
  editing); the agents/cloud aren't surfaced; and **nothing has been measured on your audio.**
- **Net: a strong foundation, an unusable product, zero measured quality.** ~6–7/10 engineering, not
  a 10/10 product.

## What "truly ready / 10/10" means — YOUR bar (all observable/measured)

1. Import audio → a **good** draft (measured better than baseline on your gold, p<0.05).
2. Correct it **fast and painlessly** (click a word, type, spaces work, keyboard flow).
3. The agents + cloud **actually run and you can SEE them** (auto-accepted vs sent-to-you funnel).
4. Fix a word once → the app **stops making that mistake** (measured, over-trigger ≤ 0).
5. **Real accuracy numbers** that beat the named baselines, with the safety gate live and PR-blocking.
6. Your **fine-tuned 7B comes in and measurably beats the old one** on your audio.

## The plan — every phase ends in a gate YOU can see (not "tests pass")

| Phase | What | Observable gate (how we KNOW it's real) | Needs from you |
|---|---|---|---|
| **0 · Measure the truth** | Finish the gold set; run engines against it; turn the gold-regression gate + calibration ON | I show you a real number: "on your audio, engine X = Y% CER ±CI" — the honest starting truth | **Gold set** (Nawras draft in progress; later more audio) |
| **1 · Good transcription** | Wire your real engines (7B + Gemini + Scribe) as the actual fused transcriber (today just a side script) | You import audio and the draft is GOOD — and it's MEASURED to beat the weak default (p<0.05) | **Cloud API keys** (Gemini, ElevenLabs) and/or **your 7B in WSL** |
| **2 · Painless correcting** | Real inline editor: click-a-word, spaces, keyboard, suspect-word highlight, audio sync | You correct a file without fighting the UI, in a time you find acceptable | **Decision:** the FE agent does it, or you authorize me to work on the frontend (.svelte) |
| **3 · Agents visible + loop closes** | Surface the jury/agents + Gemini; turn LOOP-0 firing ON behind the proven over-trigger gate | You fix a word once → the app stops repeating that mistake, and you can SEE the agents working | **Cloud keys**; the gold set (for the over-trigger check) |
| **4 · Your 7B improves, measured** | Corrections → training data → your retrained 7B → import → passes the gate → promoted | Your new model **measurably beats** your old one on your audio, shown in the app | **WSL + your training run** |
| **5 · Ship-ready robustness** | Crash hook, resumable jobs, audit/undo | Kill it mid-batch → it resumes, nothing lost | — |

I will run a brutal reality check at the start of every phase and at every gate: *what is actually
true now, measured or observed* — before claiming anything.

## What I need from you to even start (the real blockers, honestly)

1. **The gold set** — fix the Nawras draft I gave you and send it back (in progress). Later, a bit
   more of your real podcast audio for a stronger benchmark.
2. **Cloud API keys — Gemini + ElevenLabs.** Without these (or your 7B wired in WSL) I **cannot**
   make the transcription good. This is the #1 blocker; everything "good draft" depends on it.
3. **Your fine-tuned 7B** — where it is, and whether training is done — for the best engine and the
   improvement loop.
4. **The editing UI decision** — it's the frontend (.svelte), which I've been treating as another
   agent's lane. Tell me to take it on, or confirm who does.

Phase 0 needs only #1, which you're already doing. The moment you send the corrected Nawras text,
I run the first real measurement and we have an honest number — the start of a real 10/10.
