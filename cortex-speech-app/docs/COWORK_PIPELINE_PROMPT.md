# Cowork pipeline prompt — drive Cortex like a real user, on real Kurdish audio

This is the canonical, copy-paste prompt that makes a Cowork session **operate the real
Cortex Speech desktop app like a human user**: bring your own Kurdish (Sorani) audio, run
the full import -> VAD -> ASR pipeline, and hand back the results for **100% approval** —
both inside the app (real `AudioPlayer` + Verify) and as a self-contained **chat review
page with a play button per segment**.

It is deliberately honest: the agent reports only what the app actually produced. If a
transcript comes back blank or the engine errors, it says so — it never invents text.

---

## How to use

1. Make sure the app is built on this machine (`src-tauri/target/release/cortex-speech-app.exe`)
   and the ONNX models are present (`src-tauri/models/`). Both already exist in this repo's checkout.
2. Have your Kurdish audio file ready (wav/mp3/flac/m4a/ogg/opus/mp4/...). Note its full path.
3. Paste the prompt below into Cowork. Replace `<PATH-TO-YOUR-AUDIO>` with your file path.
   Optionally set how many segments to surface for review.

---

## The prompt (copy from here)

```
You are operating my real Cortex Speech desktop app as if you were me, a curator preparing
a Sorani speech dataset. Follow my project rules in CLAUDE.md and AGENT_CHARTER.md — above
all, the honesty law: report only what the app actually produces, never invent or "improve"
a transcript, and if anything comes back blank or errored, tell me plainly.

INPUT
- Audio file: <PATH-TO-YOUR-AUDIO>
- Surface up to N = 12 segments for my review (raise/lower as needed).
- Cloud engines stay OFF unless I explicitly say otherwise in this chat (default fully offline).

DRIVE THE APP LIKE A USER (computer-use)
1. Request computer-use access to the Cortex Speech app window. If it is not open, launch
   src-tauri/target/release/cortex-speech-app.exe, then wait for the UI (the app-root /
   segment list) to render.
2. Switch the UI to English (locale toggle) for unambiguous control, then confirm the
   segment list is empty or note what is already loaded.
3. Import my audio: use Open File (Ctrl+O) and select <PATH-TO-YOUR-AUDIO> (or Import Folder,
   Ctrl+I, for a directory). Let the background import + Silero VAD chunking finish — long
   files are split into many segments; wait for the first segment cards to appear and for
   the import status to settle. Do not time out early; long media can take minutes.
4. Report the real segment count produced by VAD. Select the first segment and run
   Transcribe (local Meta OmniASR CTC). Wait for inference to finish and read back the
   actual model hypothesis text. Repeat for up to N segments (or run batch transcribe if
   available), capturing each real transcript and its duration.
5. Do NOT enable Scribe/Gemini/OpenRouter. If you believe cloud refinement would help, ask
   me first and only proceed after I acknowledge — it sends my audio/text to a provider.

VERIFY (no fabrication)
6. Cross-check what you captured against the database: read the segment rows
   (%APPDATA%/cortex-speech/cortex-speech.db, table speech_segments) and confirm the
   segment count and that each surfaced transcript is non-empty. If any are blank, list
   them honestly as "engine returned empty" rather than filling them in.

HAND BACK FOR 100% APPROVAL (both surfaces)
7. In-app: leave the app on the segment list with the first reviewed segment open, so I can
   press play on the real AudioPlayer (bounded clip playback) and click Verify per segment.
8. Chat review page: export the surfaced segments to a manifest and build the review page:
      python scripts/build_review_page.py --manifest <run.jsonl> --out review.html --embed-audio
   Then show it to me with the file card. It must have, per segment: a play button (audio
   embedded so it plays in chat), the draft transcript, an editable correction box, and
   approve / approve-all / export-approved controls. State clearly that every transcript is
   raw machine output from <engine/model id>, not human-verified.

REPORT
9. Summarize honestly: file, real segment count, how many transcribed, how many non-empty,
   anything that errored, and exactly which engine/model produced the drafts. Give me the
   in-app review state and the review.html. Then stop and let me approve.
```

(copy to here)

---

## Agent playbook / notes (for the Cowork session, not part of the paste)

- **Stable selectors** for computer-use or CDP: `app-root`, `segments-empty-state`,
  `segment-card` (with `data-id`), `verify-btn`, `validate-btn`, `settings-btn`,
  `locale-toggle`, `search-input`, `review-inbox-btn`. Transcribe is a button labelled
  `Transcribe` (EN) / the CKB equivalent; the raw transcript field is `#raw-ts`.
- **Fallback drive method (repeatable, less "user-like"):**
  `CORTEX_AUDIO="<path>" CORTEX_OUT="<dir>" node e2e_real_app.cjs` spawns the real `.exe`
  with a remote-debug port and drives the same flow via Playwright. It fails on a blank
  transcript (no-fabrication guard), so a green run is a real signal.
- **Honesty stops:** never paste a "cleaned up" transcript as if it were the model output;
  never claim a segment is verified that the user has not verified; if VAD yields 0 segments
  or ASR errors, surface that as the result.
- **Privacy stops:** do not toggle `cloud_llm_opt_in` / `cloud_stt_opt_in` / `jury_cloud_opt_in`
  or enter API keys without explicit in-chat consent; the default offline path must stay offline.
- **Long-audio reality:** the pipeline caps decoded PCM (~1000 s) and ASR chunks at 30 s
  windows; for very long media expect many segments and minute-scale waits — don't abort.
- **Where outputs go:** write `run.jsonl` and `review.html` into the repo (the selected
  folder) so the user can reopen them; deliver `review.html` via the file card.
