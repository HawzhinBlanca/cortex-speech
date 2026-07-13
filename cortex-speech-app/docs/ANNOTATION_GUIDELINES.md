# Sorani Transcription & Annotation Guidelines — ڕێنمایی نووسینەوەی دەق

**The one law: the transcript states exactly what was spoken.** Everything a training
pipeline needs later (clean text, filler-free "premium" sets, readable subtitles) is derived
*downstream* from verbatim truth by `scripts/build_premium_dataset.py` — never by guessing
in review. Text can always be cleaned later; speech that was never transcribed is lost forever.

These rules bind **every** annotator (the owner and any second reviewer). Inter-annotator
agreement is only measurable when both reviewers follow the same law.

## 1. Verbatim rules — یاساکانی دەقی وشە بە وشە

| You hear | You write | ڕوونکردنەوە |
|---|---|---|
| A word spoken twice: «بۆ هەموو مانگێکیش بۆ هەموو مانگێکیش» | Write it **twice**, exactly as spoken | دووبارەکردنەوە هەر وەک گوتراوە بنووسە |
| Legitimate reduplication: «زۆر زۆر پێخۆشە» | Write it twice — it is real Kurdish, not an error | دووبارەکردنەوەی ڕاستەقینەی کوردی |
| A false start / partial word: the speaker says «دە...» then «دەڕوات» | Write the fragment with a **trailing hyphen**: `دە- دەڕوات` | پارچە وشە بە هێما `-` لە کۆتایی |
| Hesitation sounds (um/uh/mm) | Write the **canonical filler token** from §2 | دەنگی دوودڵی بە نووسینی ستاندارد لە §2 |
| Numbers | Write digits as digits (`500`), spoken number-words as words (`پێنج سەد`) — whichever was actually said | ژمارە وەک گوتراوە |
| Unintelligible speech | Do **not** guess. Reject the segment (or edit out only if the rest is certain) | هەرگیز مەزەندە مەکە |
| Speech in another language | Reject the segment — this corpus is Central Kurdish only | تەنیا کوردیی ناوەندی |

## 2. Canonical filler tokens — نووسینی ستانداردی دەنگە دوودڵیەکان

**Why these exact spellings:** `ئەم` is also the demonstrative "this" and `ئا` can be a
lexical interjection — a machine can never safely tell the hesitation from the real word.
So hesitations are always written with **elongated spellings that no real Kurdish word
uses**. This one convention is what makes automatic filler-filtering 100% safe downstream.

| Sound heard | Write EXACTLY | Never write |
|---|---|---|
| "um / em" (nasal hesitation) | `ئەمم` | ~~ئەم~~ (that is the word "this") |
| "ah / aa" (open hesitation) | `ئاا` | ~~ئا~~ (lexical interjection) |
| "uh / eh" | `ئەە` | — |
| "mm / hm" (closed mouth) | `ئمم` or `همم` | — (either heh key is fine: the app canonicalizes ه→ھ at export, and the premium builder detects **both** byte-forms) |
| long drawn-out vowel ("uuu/eee") | `ئووو` / `ئێێ` | — |

Rules of thumb:
- If the hesitation is long, do **not** stack more letters — the canonical token is fixed
  regardless of duration (`ئەمم`, never `ئەمممم`).
- Discourse words that are real vocabulary (`یەعنی`, `چوزانم`, `باشە`…) are **words** —
  transcribe them normally; they are NOT fillers under this convention.

## 3. Audio quality decisions in review — بڕیاری کوالیتی دەنگ

While reviewing you are also the audio gate. **Reject** (do not verify) a segment when:
- speech is masked by noise/music such that you are not ≥95% certain of every word;
- the clip is cut mid-word at either boundary and the sentence is not recoverable;
- multiple speakers overlap for most of the clip;
- it is not Central Kurdish.

A rejected segment is excluded from every training export automatically
(`quality::is_human_rejected`) — rejecting is honest work, not lost work.

## 4. What "premium" means downstream — دەرهاویشتەی پاک

`scripts/build_premium_dataset.py` builds the highest-grade training set from an app
JSONL export. It keeps only segments that are human-verified AND pass, among others:
- no canonical filler tokens (§2) and no `X-` false-start fragments (§1) in the text;
- duration, chars-per-second, clipping/SNR/RMS bounds (the app's own measured metrics);
- optional owner-supplied blockword list.

**Alignment note (important).** The app's own `trainingReady`/GOLD grade *also* requires
MMS-precise word timestamps. That model (`mms_aligner.onnx`) is not installed, so every clip
is graded `review` for `energy_heuristic_alignment` — including clips you verify. Because ASR
audio→text training does **not** use word timestamps, the premium builder deliberately does
**not** gate on `trainingReady`: a human-verified clip with clean audio and clean text is
premium regardless of alignment source. It still excludes the app's hard `reject` grade and any
real audio review-risk (clipping/low-RMS/low-SNR). If you later want precise word timings (for
CTC/forced-alignment training), install the aligner and re-process — that lifts clips to GOLD.

That script — not the annotator — is where "no fillers, clean audio, full sentences"
happens. **Your job in review is only ever the truth of §1–§3.** A verbatim segment
containing `ئەمم` is a *correct, valuable* segment; the premium builder simply routes it
to the verbatim tier instead of the premium tier.

## 5. Quick reference card — کارتی خێرا

1. Write what was said — all of it, only it. — هەرچی گوترا بنووسە، هەموو و تەنیا ئەوە
2. Repeats: write twice. Fragments: `X-`. Fillers: §2 spellings only.
3. Not sure of a word? Listen again. Still not sure? Reject.
4. Digits as spoken; punctuation only where prosody clearly marks it.
5. Never "improve" grammar — the speaker's grammar IS the data.
