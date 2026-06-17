# The Listening Jury + The Review Inbox
### The two mechanisms that make CORTEX the #1 Kurdish data app

> **The noble feature, in one sentence:** every segment the machine is unsure about
> arrives in your inbox **already adjudicated by an AI jury that *listened to the
> audio*** — suspect word highlighted, the clip cued to that exact moment, a proposed
> fix, the evidence behind it (dictionary, re-listen, web), and a one-line reason —
> so the human's job collapses from **transcribe** to **approve**, and every approval
> teaches the jury to need the human less.

This document is the result of dated 2025–2026 research (citations at the end). It
is the active, human-facing half of the **Disagreement Refinery** (see
`THE_DISAGREEMENT_REFINERY.md`): the Refinery *measures* disagreement; the Jury
*resolves* it; the Inbox *presents* the residue to a human in seconds.

---

## 0. The convergence that is actually world-changing

For a low-resource language the bottleneck is not models — it is **expert hours**.
The win condition is a **self-narrowing funnel**:

```
  10,000 segments
      │  Refinery: phonetic consensus + IRT          → 6,500 auto-certified (proven)
      ▼
   3,500 disagreements
      │  Listening Jury (cascade, listens, argues)   → 3,050 agent-resolved + reasoned
      ▼
     450 genuine escalations  ──►  HUMAN INBOX  (each pre-argued, 1 keystroke to clear)
      │
      └──► every human keystroke → few-shot memory + DPO-lite → next week the Jury
           escalates 300, then 180, then 90 … at the SAME certified quality.
```

The miracle is not any single model. It is that **one expert's afternoon produces
what used to take a labeled-data team months**, with a *provable* quality bar — and
the human's share shrinks every cycle. That is the thing no annotation tool on Earth
does for Kurdish.

---

# PART 1 — The Listening Jury (the autonomous validator)

## 1.1 The single most important architectural decision

**Because you own the source, the agent must call your functions, not drive your
GUI.** GUI-driving agents (Claude/OpenAI *computer-use*) exist to operate software
that has *no API*. They top out at ~38% (OpenAI OSWorld) / ~22% (early Claude) on
full computer tasks, are officially "error-prone," and burn **400K–2M tokens per
task** on screenshots. Over thousands of clips that failure rate and cost are
disqualifying.

→ **Expose your Rust pipeline as structured tools (MCP) and let the agent call
them.** Reserve vision/computer-use only for an irreducible step with no programmatic
surface (you have none — you own it). ([Anthropic computer-use beta, 2024-10-22](https://www.anthropic.com/news/3-5-models-and-computer-use); [OpenAI Operator/CUA benchmarks, 2025](https://venturebeat.com/programming-development/openai-unveils-responses-api-open-source-agents-sdk-letting-developers-build-their-own-deep-research-and-operator))

**Framework pick:**
- **Default — Claude Agent SDK with in-process SDK MCP tools**, run as a **Tauri
  sidecar**. The `tool()` helper exposes typed functions; returning `isError: true`
  (instead of throwing) lets one bad clip fail *without killing the batch* — essential
  for unattended runs. You own the for-loop; the agent owns each verdict.
  ([Claude Agent SDK custom tools, 2026](https://code.claude.com/docs/en/agent-sdk/custom-tools); [MCP donated to Linux Foundation, Dec 2025](https://en.wikipedia.org/wiki/Model_Context_Protocol))
- **Offline / air-gapped option — Nous `hermes-agent`** (open weights, native MCP,
  runs locally). Lower frontier reliability, but $0/token and fully sovereign — the
  only option that preserves your offline-first promise end-to-end.
- **Antigravity / Cursor / Codex are the wrong tool here** — they are agentic *coding
  IDEs* for *building* software, not runtimes you embed to validate data. Use them to
  *write* the Jury; don't use them to *be* it.

## 1.2 The cascade — don't make an LLM the transcriber

The research is blunt: **make a specialist ASR transcribe; make the LLM a listening
*judge* that fires only on disagreement.** Whisper zero-shot is wrong for Sorani.

| Tier | What runs | Cost | Job |
|---|---|---|---|
| **T0 Transcribe** | OmniASR-7B (primary) + a 2nd model (XLS-R/1B) | self-host | Auto-accept where models **agree** *and* text is fluent (lexicon + perplexity clean). **Disagreement is the escalation trigger** — this sidesteps the well-documented "cheap-model confidence is miscalibrated" trap. |
| **T1 Tool judge (text)** | small LLM + tools | cheap | For disagreements: lexicon lookup, n-gram/perplexity, named-entity web search. Localizes the suspect span; fixes trivial cases; escalates the rest. Threshold calibrated by **conformal prediction on the gold set**, not raw confidence. |
| **T2 Listening jury (audio)** | **Gemini 2.5 Pro** (audio in) | ~$0.037/min | Gets the **audio span + candidate + T1 evidence**, *re-transcribes the contested region by ear*, returns **edit + cited reason + confidence**. **Self-consistency N=3.** |
| **T2+ Debate** | diverse judges | costly | **Only** when T2 disagrees with itself or with T0. Debate helps on genuinely hard cases but *wastes money on easy ones and can amplify confident-wrong agreement* — so it is gated, not default. |
| **Human** | you | your time | Sees only T2 escalations, each pre-argued. One keystroke. |

Cascades cut cost 45–85% while keeping ~95% of quality; the trick is using
**model-agreement (not self-confidence)** as the escalation signal.

## 1.3 The judge must *argue*, not score

A judge that returns "0.7" is weak. A judge that returns **{suspect span, proposed
edit, evidence[], one-line reason, confidence}** is what makes the human pass fast.
Tool-using judges beat text-only judges because tools allow *exact* checks (look the
word up; re-run ASR on just that span; hear it again) instead of error-prone text
inference. Audit the judge's *trajectory*, not just its verdict (chain-of-thought can
be unfaithful). ([Tool-Integrated RL for judges, arXiv:2510.23038, Oct 2025](https://arxiv.org/pdf/2510.23038); [Gaming the Judge, arXiv:2601.14691, 2026](https://arxiv.org/pdf/2601.14691))

## 1.4 LLM-as-judge biases — design around them (they are real)

- **Position bias:** verdict flips on order-swap in **10–30%** of pairwise calls →
  randomize order and **require swap-agreement** or declare a tie.
- **Self-preference & verbosity:** judges favor familiar/longer text → use
  **rubric/reference-anchored** scoring against the audio anchor, not free preference.
- **Overconfidence:** verbalized confidence is systematically over-confident →
  route on **conformal thresholds / agreement / hidden-state probes**, never on the
  raw number. ([Zheng et al. MT-Bench, arXiv:2306.05685, 2023](https://arxiv.org/abs/2306.05685); [Self-preference bias, arXiv:2410.21819, 2024](https://arxiv.org/html/2410.21819v2))

## 1.5 Debate vs self-consistency — the honest finding

Default multi-agent debate **rarely beats** strong single-agent **self-consistency**
at equal compute, and can amplify shared errors. Debate *does* help on the hard,
high-disagreement tier and with weaker models (e.g. Gemini-2.0-Flash on LLMBar
77.75%→81.83% with stability-gated debate). → **Self-consistency is the workhorse;
debate is the escalation-tier specialist with *diverse* judges (audio-LLM + a
re-ASR), because diverse errors are less correlated.** ([Stay Focused / Problem Drift, arXiv:2502.19559, 2025](https://arxiv.org/pdf/2502.19559); [Multi-Agent Debate for Judges, NeurIPS 2025, arXiv:2510.12697](https://arxiv.org/html/2510.12697v1))

## 1.6 The learning loop (this is the highest-ROI piece)

Every human decision in the final pass becomes signal:
1. **Few-shot memory (instant):** store (audio-feature, wrong-transcript, human-fix);
   retrieve similar cases into the judge's context.
2. **DPO-lite (weekly):** batch the preference pairs into a small update of the
   *escalation router* and the judge's scoring head. DPO > full RLHF here: simpler,
   stable, ~66% less train time.
3. **Active learning:** pick the next clips to surface by judge uncertainty.

Targeted feedback is dramatically label-efficient — **RLTHF reaches
full-annotation-level alignment with ~6–7% of the human effort.** North-star metric:
**escalation rate ↓ at fixed precision.** ([ActiveDPO, arXiv:2505.19241, 2025](https://arxiv.org/pdf/2505.19241); RLTHF 2025)

---

# PART 2 — The Review Inbox (UX for non-technical curators)

## 2.1 The reframe: an approval inbox, not an editor

Your user is a **reviewer, not an annotator**. Every source (Prodigy, Label Studio,
Snorkel, Common Voice) converges: **pre-fill everything, one item at a time, make
accept/reject a single keystroke, order the queue riskiest-first.**

**Steal Prodigy's keyboard map verbatim:** `a` accept · `e` edit · `x` reject ·
`space` skip · `f` flag for second pass · `backspace` undo · `1–9` pick option. One
focus card on screen. The card defaults to *confirm* because the AI transcript +
proposed fix are already there. ([Prodigy web-app docs](https://prodi.gy/docs/api-web-app); [Label Studio review workflows](https://humansignal.com/blog/enhancing-data-quality-with-label-studio-automated-workflows/))

Add two QC ideas from speech-corpus practice: **a "Flag → second-pass adjudication"
lane** so ambiguity never blocks throughput, and **Common-Voice-style seeded known-bad
items** (deliberate audio↔text mismatches) sprinkled in to keep reviewers honest.
([Common Voice corpus, LREC 2020](https://aclanthology.org/2020.lrec-1.520.pdf))

## 2.2 The agent must show its work (6 patterns to implement)

From the 2026 agentic-UX literature — implement all six:
1. **Intent Preview** — user types intent ("build a 5-hour clean read-speech set");
   agent shows a 3–5 step plan with **reversibility badges** and Approve/Reject
   *before* acting. Never auto-dismiss; irreversible steps visually heavier.
2. **Autonomy Dial** — per task type: *Observe → Propose → Act-with-confirm → Act-auto*.
3. **Explainable Rationale** — "because you asked for clean speech, I removed 12 noisy
   clips."
4. **Confidence Signal** — visible uncertainty to fight automation bias.
5. **Action Audit & Undo** — chronological agent log, prominent persistent undo
   ("mistakes feel cheap" = trust).
6. **Escalation Pathway** — when ambiguous, the agent *asks*, not guesses.
([Smashing Magazine — agentic UX patterns, Feb 2026](https://www.smashingmagazine.com/2026/02/designing-agentic-ai-practical-ux-patterns/); [Intent Preview pattern](https://www.aiuxdesign.guide/patterns/intent-preview))

## 2.3 Never show a raw metric — number → band → icon → verb

| Metric | ❌ don't show | ✅ show |
|---|---|---|
| Confidence | "0.96" | "AI is fairly sure (96%) the text matches — quick listen 👂" |
| WER | "WER 12%" | "About 1 in 8 words may not match the audio — worth a listen ⚠" |
| SNR | "SNR 8 dB" | "Background noise: High — hard to hear over noise 🔊" |

Every metric pairs with a **verb** so a novice knows the *action*, not the number.
Showing confidence + a short rationale measurably raises novice trust over a bare
number. ([Mindee — confidence scores in plain language](https://www.mindee.com/blog/how-use-confidence-scores-ml-models))

## 2.4 RTL / Sorani — fix the thing that's currently broken

The audit found **zero `dir` handling** — ironic for a Kurdish app. Do it properly:
- Root: `dir="rtl" lang="ckb"`; **CSS logical properties everywhere**
  (`margin-inline-start`, `text-align: start`, `inset-inline-start`).
- Font stack: `'Vazirmatn', 'Noto Naskh Arabic', system-ui, sans-serif`;
  **line-height ≥ 1.8**, regular weight ≥ 400, **never `letter-spacing`** (breaks
  joining), `hyphens: none`, body size +10–15% vs Latin.
- Inputs that hold mixed content: `dir="auto"`; wrap Latin (model names, file IDs,
  timecodes) in `<bdi>`.
- **Keep LTR:** waveform, timecodes (00:03.250), numbers. **Mirror:** nav, progress,
  bullets. **Don't mirror:** logos, media transport controls, checkmarks.
- ⚠ **Verify Sorani glyphs** (ۆ ێ ڕ ڵ ژ گ چ پ ک, Kurdish yeh/heh) render in Vazirmatn
  in your actual Tauri webview before locking the stack.
([Vazirmatn](https://fonts.google.com/specimen/Vazirmatn); [RTL/Arabic UI guide](https://aivensoft.com/en/blog/rtl-arabic-website-design-guide))

## 2.5 Accessibility (WCAG 2.2)

Keyboard-operable everything (you already are), **synchronized interactive
transcripts** (highlight word as spoken, click to seek — both an a11y win and a
faster-review win), **live-region status announcements** for agent progress (not just
toasts), visible focus, no color-only meaning. ([W3C WAI transcripts](https://www.w3.org/WAI/media/av/transcripts/); [WCAG 2.2, Oct 2025](https://www.audioeye.com/post/wcag-22/))

## 2.6 The primary screen — "Review Inbox" (RTL)

Single-item focus card + queue rail (ordered riskiest-first) + plain-language metric
strip + Prodigy verb bar + agent status pill → activity feed. Supporting screens via
progressive disclosure: NL command bar (→ Intent Preview), new-dataset wizard (Sorani
preset, advanced knobs collapsed), activity/audit feed, dataset dashboard.

---

# PART 3 — Integration plan against the existing code

1. **MCP sidecar** (Tauri sidecar, TS/Node with Claude Agent SDK). In-process
   `tool()` definitions, each calling your Rust core over a localhost IPC/CLI bridge.
   Tools: `next_segment`, `get_audio_span(id,start,end)`, `get_hypotheses(id)`
   (the N transcripts from the Refinery), `lexicon_lookup(word)`,
   `rerun_asr(model,span)`, `web_search(q)`, `propose_edit`, `flag_for_human`,
   `commit_verdict`. Read tools → `readOnlyHint`. Handlers return `isError`, never
   throw.
2. **`asr.rs`** → multi-backend hypothesis provider (already heading there with the
   consensus work): sherpa 300M/1B/7B + Gemini HTTP + optional shadow model.
3. **`db.rs` + migration** → add `verdict`, `rationale`, `evidence_json`,
   `agent_confidence`, `escalated`, `human_decision`, `corrected_at`; new
   `agent_examples` table (few-shot memory) and `eval_runs` (gold-set tracking).
4. **`quality.rs` / `validation/`** → home of conformal thresholds + the
   escalation router; quality gates become statistical, not heuristic.
5. **Frontend** → rebuild the main view as the **Review Inbox**; the stubbed
   **"Compare ASR"** button becomes the **Jury panel** (all hypotheses aligned,
   color-coded by agreement, the proposed verdict + evidence, accept/edit/reject).
   Add the agent **activity feed** (Intent Preview + Action Audit) and **RTL**.
6. **Gold-set eval first** (the interlock) — see Part 4.

---

# PART 4 — Honest truths (loyal & wise)

1. **Prove Sorani quality before trusting any of it.** There is **no public benchmark**
   for Central Kurdish (ckb) on Gemini 2.5 Pro or OmniASR-7B. Your first-hand report
   is the best signal that exists — so **build the held-out gold set and measure WER
   per model on YOUR data first.** Everything downstream (cascade thresholds,
   conformal bounds, IRT priors) calibrates off it. This is non-negotiable.
2. **Cloud breaks offline-first.** The Gemini listening tier and any cloud agent need
   network. Gate them behind an explicit setting; keep the **Hermes + local audio
   model** path for air-gapped operation. Be honest in the UI about what leaves the
   machine.
3. **Conformal guarantees are marginal** (average error), not per-clip. Say so on the
   data card.
4. **The learning loop can rot** (self-training amplifies its own errors) → keep a
   **permanently fresh human-gold holdout**; the Jury never certifies alone.
5. **Debate can amplify confident-wrong agreement** → keep it gated to the hard tier
   with diverse judges; audit trajectories.
6. **Cost is real but bounded** by the cascade — only the hard tail hits the paid
   audio-LLM. Track $/certified-hour as a first-class metric.
7. **Model versions drift monthly** (Opus 4.6→4.8, Gemini revs) — re-verify the
   current model at build time; the architecture is model-agnostic by design.

---

# PART 5 — What to build, in order

1. **Gold-set eval harness** (interlock + truth source) — measures Sorani WER per
   model; calibrates everything.
2. **T0 disagreement gate** — the Refinery's phonetic consensus already produces this;
   auto-accept agreements, queue disagreements.
3. **Review Inbox UX + RTL** — the human-facing win; works even before the Jury, on
   raw disagreements. Immediate, visible quality-of-life jump.
4. **MCP sidecar + T1 tool judge** — the agent's hands; resolves trivial disagreements.
5. **T2 listening jury (Gemini audio)** + self-consistency — the signature feature.
6. **Learning loop** (few-shot → DPO-lite → active learning) — the compounding that
   makes the human pass vanish.
7. **Debate tier + autonomy dial** — last, for the hard tail and power users.

---

## Citations (selected, dated)
- Claude Agent SDK / MCP tools, 2026 — code.claude.com/docs/en/agent-sdk
- MCP → Linux Foundation, Dec 2025 — en.wikipedia.org/wiki/Model_Context_Protocol
- Computer-use limits — Anthropic (Oct 2024); OpenAI Operator/CUA (2025)
- Meta Omnilingual ASR (1600 langs), arXiv:2511.09690 (Nov 2025)
- Audio-LM eval (FLEURS variance) — AHELM, arXiv:2508.21376 (Sep 2025)
- LLM-as-judge biases — Zheng et al., arXiv:2306.05685 (2023); self-preference arXiv:2410.21819 (2024)
- Debate vs self-consistency — arXiv:2502.19559 (2025); NeurIPS 2025 arXiv:2510.12697
- Tool-using judges — arXiv:2510.23038 (2025); unfaithful CoT arXiv:2601.14691 (2026)
- Label-efficient feedback — ActiveDPO arXiv:2505.19241 (2025); RLTHF (2025)
- Cascades / routing — survey arXiv:2603.04445 (Apr 2026); FrugalGPT
- UX — Prodigy docs; Label Studio; Snorkel; Common Voice (LREC 2020); Smashing agentic UX (Feb 2026); Intent Preview; NN/g progressive disclosure; Mindee confidence copy; Aivensoft RTL guide; Vazirmatn; W3C WAI transcripts; WCAG 2.2 (Oct 2025)

*Drafted 2026-06-12. The active half of the Disagreement Refinery: a listening,
arguing, self-improving validator funnel behind an inbox a non-coder can clear in
minutes.*
