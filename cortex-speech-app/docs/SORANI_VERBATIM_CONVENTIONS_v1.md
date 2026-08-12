# Sorani Verbatim Transcription Conventions — v1.0 (2026-08-12)

**Status:** ACTIVE. Every reviewer decision from this version onward is judged against these rules.
Version it: if a rule changes, this becomes v1.1 and the change is dated, because a dataset whose
transcription policy silently drifted is a dataset whose measurements cannot be compared over time.

**Why this document exists.** Transcription policy is a *controlled variable*, not a matter of taste.
When it is left implicit, research measured up to **60% of reported error rate** as style mismatch
rather than real recognition error (arXiv 2607.18934). Two reviewers following different unwritten
rules produce disagreement that looks like inaccuracy and poisons both the dataset and the agreement
metric. This document is the shared rule.

---

## THE ONE RULE

> **Write exactly what you hear. Not what the speaker meant. Not what is correct Kurdish.**

You are a **microphone**, not an editor. If the speaker says something wrong, awkward, repeated, or
in Arabic — you write it wrong, awkward, repeated, or in Arabic.

This dataset teaches a machine what Kurdish speech *sounds like in reality*. Every "improvement" you
make teaches it something false.

---

## 1. Repetitions and stutters — KEEP THEM

The speaker's repeats are real speech.

| You hear | ✅ Write | ❌ Never write |
|---|---|---|
| کە... کە... کە چووم | `کە کە کە چووم` | `کە چووم` |
| من من نازانم | `من من نازانم` | `من نازانم` |

## 2. Filler words — KEEP THEM

`ئێە`, `ئەم`, `یانی`, `واتە`, `چۆن بڵێم` — if it was said, it is written.

| You hear | ✅ Write | ❌ Never write |
|---|---|---|
| ئێە... من یانی نازانم | `ئێە من یانی نازانم` | `من نازانم` |

## 3. Loanwords — WRITE THE WORD THAT WAS SAID

**This is the most important rule, and the one most often broken.** Kurdish speakers use Arabic,
Persian, English and Turkish words constantly. That is real Kurdish speech.

| Speaker says | ✅ Write | ❌ Never "correct" to |
|---|---|---|
| مەعاش | `مەعاش` | `مووچە` |
| تەلەفون | `تەلەفون` | `پەیوەندی` |
| مەکتەب | `مەکتەب` | `نووسینگە` |
| کۆمپیوتەر | `کۆمپیوتەر` | `ژمێریار` |

If the speaker used the Arabic word, the Arabic word **is** the transcript. Replacing it is
translation, and translation destroys the clip's value.

English/Latin-script words spoken inside Kurdish: write them in **Latin script as spoken**
(`ئەو فایلەکە download کرد`).

## 4. Numbers — WRITE THEM AS SPOKEN

| Speaker says | ✅ Write | ❌ Never write |
|---|---|---|
| "دوو هەزار و چواردە" | `دوو هەزار و چواردە` | `٢٠١٤` or `2014` |
| "بیست و پێنج" | `بیست و پێنج` | `٢٥` |

Write the *words* you hear. Never convert to digits — and never convert digits to words either: it
is always about what was **said**.

## 5. Punctuation — DO NOT ADD IT

The champion draft carries almost none, and that is correct. Do **not** add `.` `،` `؟` `!` to make
it look like writing. Punctuation is invented information — a machine cannot hear a comma.

The only exception: a mark the speaker explicitly says out loud (rare).

## 6. Grammar and word order — DO NOT FIX

If the sentence is broken, unfinished, or grammatically wrong — that is what was said.

| You hear | ✅ Write | ❌ Never write |
|---|---|---|
| من چووم... نەخێر، ئەو چوو | `من چووم نەخێر ئەو چوو` | `ئەو چوو` |

## 7. Dialect — WRITE THE DIALECT

Badini, Hewlêrî, Silêmanî, Serdeştî pronunciations are written as pronounced. Do **not** standardize
to Silêmanî/"proper" Sorani. The dataset's dialect coverage is a feature; standardizing erases it.

## 8. Script and characters

- Use the **Kurdish Arabic** letters: `ک` (not `ك`), `ی` (not `ي`), `ە`, `ڕ`, `ڵ`, `ۆ`, `ێ`, `ھ`/`ه` as your keyboard produces
- Do not fight the app over character variants — normalization handles `ه`/`ھ`, `ك`/`ک`, `ي`/`ی` at export
- One space between words; no double spaces; no leading/trailing spaces

## 9. Unclear audio — use REJECT, never guess

If you genuinely cannot hear it: **do not invent a plausible word.**

- Mostly unclear / two people talking over each other / unusable audio → **reject** (مارکی خراپ)
- You cannot judge it at all → **skip** (it goes to another reviewer)
- A guess is worse than a skip, because nothing downstream can tell a guess from a fact.

## 10. Non-speech sounds

Laughter, coughs, background noise: do **not** transcribe them and do not add tags. Write only the
words spoken. If noise makes the speech unintelligible, reject the clip (rule 9).

---

## The three buttons

| Button | Use it when | It means |
|---|---|---|
| **دروستە** (Looks good) | The draft matches the audio **exactly**, by every rule above | This text is now training truth |
| **پاشەکەوت** (Save correction) | Anything differs — even one letter | Your typed text becomes training truth |
| **خراپ** (Bad) | Unusable audio, unintelligible, wrong language | Excluded from the dataset |

**On "دروستە":** you are certifying every word. If you fixed something and *then* pressed دروستە,
that is correct and expected — the button records whatever text is in the box at that moment.

**On honesty:** some clips are hidden checks with a known answer, used to measure the review process
(not to judge you personally). Reviewing carefully is the only strategy that works — and it is the
only one we want.

---

## Calibration (before you start)

Every reviewer completes a **20-clip calibration batch** whose answers were adjudicated by the owner.
Your calibration CER is recorded. This is not a test of you — it is how we prove the *dataset* is
consistent, and it is required by the agreement standards this corpus is measured against
(κ ≥ 0.75 before scaling; SOTA corpora use 2 independent passes + adjudication for gold).

If your calibration shows a systematic difference from these rules, we talk about it and — where you
are right — **this document changes**. The conventions serve the corpus, not the other way round.
