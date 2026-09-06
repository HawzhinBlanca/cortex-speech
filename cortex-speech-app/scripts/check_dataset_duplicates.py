#!/usr/bin/env python3
"""The same-recording-under-different-names audit, on the LIVE library.

FOUND BY THE OWNER'S EARS, 2026-08-17, not by any gate — which is why this exists. The library held
one recording under THREE filenames (Lamofull2_00086400_A01 / Lamofull00086400_A01 / _A02): the
files are different ENCODES, so the byte-level audio fingerprint (v50/51) sees three distinct files,
and every clip cut from them imported as new work. ~65 duplicate sentences entered the corpus, 33 of
them were REVIEWED TWICE (paid twice), and the same content in nominally-different recordings can
straddle a train/test split — silent leakage that invalidates any measurement taken across it.

TWO SIGNALS — the second exists because the owner's ears beat the first within the hour:

RULE A — EXACT TEXT, any offset. The same >= 25-char champion transcript in two DIFFERENT files is
the same recording: real decodes of genuinely different recordings always drift somewhere in a
sentence that long. The first version required matching source offsets too, and the owner then
heard the SAME sentence again on the very page the audit had "deduplicated": the library holds a
THIRD encode (`A1-0032_PODCAST-001.mp4`) whose timeline is shifted by a constant 137.8 s, so
offset agreement was blind to it. The Lamofull*.flac files are the FULL-LENGTH recordings; the
`A1-00xx` episode files are cuts of the same material — two generations of one corpus, both
imported.

RULE B — same offset, DRIFTED text. Twins at the same source-clock position (within 500 ms) whose
transcripts are >= 90% similar: different encodes decode a letter apart (`بەڵێ`/`بەلێ`), so exact
matching misses them exactly where rule A's premise (decodes drift) works against us. Offset
agreement carries the burden of proof here instead.

Exit 1 when duplicate content EXCEEDS the recorded baseline (a new duplicate import happened);
otherwise reports the count, which must only ever go DOWN. Baseline ratchets to 0 after the cleanup.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path, PureWindowsPath
from typing import Callable

# The duplicates that existed the day this gate was written, awaiting the owner-gated cleanup.
# After the cleanup, set to 0 — from then on a single new duplicate is a RED sweep.
#
# CORRECTED 70 -> 170 the same day, and the correction is the honest part: the first measurement
# required offset agreement, which the mp4's shifted clock defeated — the owner HEARD the remaining
# twins on a page the audit had "deduplicated". 170 is what rules A+B measure on 2026-08-17's
# library. This number may only ever be lowered (the cleanup) — raising it again requires the
# owner's `change canon:`, because a higher baseline is indistinguishable from waving through a
# fresh duplicate import.
KNOWN_BASELINE = 0
MIN_TEXT_CHARS = 25
OFFSET_BUCKET_MS = 500
RULE_B_VECTOR_THRESHOLD = 1_024
RULE_B_VECTOR_BLOCK_ROWS = 128
RULE_B_COARSE_BINS = 20

# RULE C — THE AUDIO DECIDES (2026-08-18). Rules A and B match TEXT, and text alone cannot tell a
# duplicated import from a narrator saying the same sentence twice. The audiobook corpus makes that
# distinction load-bearing: every episode of `bangewazek_bo_behesht` opens by announcing the series
# title, and a ghazal collection repeats verses across chapters. Measured on the first 5 books
# imported, ALL THREE flagged groups were legitimate repeats — with 134 books to import, this gate
# would have gone permanently red and been ignored, which is how a real duplicate gets through.
#
# So a text match is now a CANDIDATE, and the clip audio confirms it. Two readings of one sentence
# differ everywhere; a duplicated import is the same samples. This STRENGTHENS the gate: every true
# positive still fails it, and the false positives stop.
#
# Correlation of the two clips' normalised waveforms at the BEST LAG inside ±AUDIO_MAX_LAG_MS, over
# the overlapping span (v2, 2026-09-06). The v1 verdict compared at zero lag with a 0.98 bar: a twin
# cut even a few milliseconds differently scored near zero and was "cleared" as a re-read sentence.
# MEASURED on the live pool (16,990 canonical clips): 8,909 text-matched cross-file pairs, 88% at or
# above 0.6 at the best lag; 597 same-voice DIFFERENT-sentence control pairs never exceeded 0.2. Same
# recording through different encodes sits anywhere from 0.4 upward (a windowed drift-robust metric
# tracked the global one, so the spread is codec/bandwidth, not clock drift). Candidate bar 0.40 =
# twice the control ceiling. These are candidate bands, NOT sufficient exclusion authority.
# Whole-clip PCM equivalence is required below; correlated non-identical cuts remain unresolved.
AUDIO_DUPLICATE_CORRELATION = 0.40
AUDIO_PROBABLE_CORRELATION = 0.20
AUDIO_MAX_LAG_MS = 1500
# Two cuts of one recording may differ by whole words at either edge; compare the overlap, but only
# when at least this share of the shorter clip is covered — below it the "duplicate" is a fragment.
AUDIO_MIN_OVERLAP_RATIO = 0.60
# Two readings almost never match to the millisecond, but two CUTS of one recording easily differ by
# a second (measured: 6.6 s vs 6.4 s twins). Cheap pre-filter before the decode, and the reason a
# missing/undecodable clip degrades to "unconfirmed" rather than "clean".
AUDIO_DURATION_TOLERANCE_MS = 1500
# RULE A' (v2): the champion transcribes one recording slightly differently per cut, so exact text
# misses most twins. Measured: 6,396 near-text pairs (similarity 0.85..0.99) held 88% true duplicates,
# more than the exact-text rule found. Candidates only — the audio still decides.
TEXT_CANDIDATE_SIMILARITY = 0.70


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def duplicate_groups(rows: list[tuple[str, str, str, str, int]]) -> list[list[tuple[str, str]]]:
    """Groups of (segment_id, source_file) holding the same content across DIFFERENT files.

    `rows` = (id, audio_path, alignment_json, raw_transcript, verified).
    Pure so test_dataset_duplicates.py can pin it without a database. Union of RULE A (exact
    normalized text, any offset — the mp4's shifted clock taught us offsets prove nothing across
    encode generations) and RULE B (offset within 500 ms + >= 90% text similarity — drifted decodes
    of the same moment). Groups are merged when they share a member, so a sentence caught by both
    rules is one group, not two.
    """
    import difflib

    parsed: list[tuple[str, str, str, int]] = []  # (id, file, normalized text, offset)
    durations: dict[str, int] = {}
    for seg_id, path, alignment_json, raw, _verified in rows:
        text = " ".join((raw or "").split())
        if len(text) < MIN_TEXT_CHARS:
            continue
        try:
            meta = json.loads(alignment_json or "{}")
            offset = int(meta.get("source_start_ms", -1))
            end = int(meta.get("source_end_ms", -1))
        except (ValueError, TypeError, AttributeError):
            offset, end = -1, -1
        durations[seg_id] = end - offset if offset >= 0 and end > offset else -1
        # The library stores Windows drive paths; PureWindowsPath splits both separators, so the
        # basename is identical on Windows and deterministic when this audit runs on POSIX CI.
        parsed.append((seg_id, PureWindowsPath(path).name, text, offset))

    # Union-find over clip ids, so A-links and B-links merge into one group per real sentence.
    parent: dict[str, str] = {sid: sid for sid, _, _, _ in parsed}

    def find(x: str) -> str:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: str, b: str) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    # RULE A: identical normalized text in different files, offsets irrelevant.
    by_text: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for sid, fname, text, _off in parsed:
        by_text[text].append((sid, fname))
    for members in by_text.values():
        if len({f for _, f in members}) > 1:
            first = members[0][0]
            for sid, _ in members[1:]:
                union(first, sid)

    # RULE A' (v2): NEAR-identical text across different files whose clip durations sit inside the
    # audio tolerance. A word-trigram inverted index keeps this linear in the corpus; SequenceMatcher
    # decides similarity only for pairs sharing enough trigrams. Pure candidate generation — a pair
    # that is really two readings is cleared by the audio verdict, exactly like RULE A.
    grams: dict[str, set[str]] = {}
    postings: dict[str, list[str]] = defaultdict(list)
    text_of: dict[str, str] = {}
    file_of: dict[str, str] = {}
    for sid, fname, text, _off in parsed:
        words = text.split()
        trigrams = {" ".join(words[i : i + 3]) for i in range(len(words) - 2)}
        if not trigrams:
            continue
        grams[sid] = trigrams
        text_of[sid] = text
        file_of[sid] = fname
        for gram in trigrams:
            postings[gram].append(sid)
    for sid, trigrams in grams.items():
        shared: dict[str, int] = defaultdict(int)
        for gram in trigrams:
            bucket = postings[gram]
            # A trigram shared by hundreds of clips (a series title, a filler) carries no identity.
            if len(bucket) > 200:
                continue
            for other in bucket:
                if other > sid:
                    shared[other] += 1
        for other, count in shared.items():
            if file_of[other] == file_of[sid]:
                continue
            if count < 0.25 * min(len(trigrams), len(grams[other])):
                continue
            left_duration, right_duration = durations.get(sid, -1), durations.get(other, -1)
            if left_duration >= 0 and right_duration >= 0 and abs(left_duration - right_duration) > AUDIO_DURATION_TOLERANCE_MS:
                continue
            matcher = difflib.SequenceMatcher(None, text_of[sid], text_of[other])
            if matcher.quick_ratio() < TEXT_CANDIDATE_SIMILARITY or matcher.ratio() < TEXT_CANDIDATE_SIMILARITY:
                continue
            union(sid, other)

    # RULE B: same source-clock position (within the bucket), text >= 90% similar, different files.
    # Connected offset clusters limit the search surface without floor-bucket edge misses. Pair
    # eligibility is still the exact <= 500 ms distance, not mere membership in a transitive cluster.
    # The previous implementation compared every pair in a connected cluster. The 2026-08-24 live
    # library has 27,813 independently chunked files at offset zero, turning that into 386,767,578
    # SequenceMatcher calls and making the master verifier effectively unusable.
    with_offset = sorted((o, sid, f, t) for sid, f, t, o in parsed if o >= 0)
    cluster: list[tuple[int, str, str, str]] = []

    def exact_pair(left: tuple[int, str, str, str], right: tuple[int, str, str, str]) -> None:
        oa, sa, fa, ta = left
        ob, sb, fb, tb = right
        if ob - oa > OFFSET_BUCKET_MS or fa == fb:
            return
        # Rule A already joined exact text across files. Avoid paying for it again here.
        if ta != tb and difflib.SequenceMatcher(None, ta, tb).ratio() >= 0.90:
            union(sa, sb)

    def flush(current: list[tuple[int, str, str, str]]) -> None:
        if len(current) >= RULE_B_VECTOR_THRESHOLD:
            _vectorized_rule_b(current, exact_pair)
            return
        for i in range(len(current)):
            for j in range(i + 1, len(current)):
                if current[j][0] - current[i][0] > OFFSET_BUCKET_MS:
                    break
                exact_pair(current[i], current[j])

    for entry in with_offset:
        if cluster and entry[0] - cluster[-1][0] > OFFSET_BUCKET_MS:
            flush(cluster)
            cluster = []
        cluster.append(entry)
    flush(cluster)

    files_of = {sid: fname for sid, fname, _, _ in parsed}
    groups: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for sid, _, _, _ in parsed:
        groups[find(sid)].append((sid, files_of[sid]))
    return sorted(sorted(g) for g in groups.values() if len(g) > 1 and len({f for _, f in g}) > 1)


def _vectorized_rule_b(
    cluster: list[tuple[int, str, str, str]],
    exact_pair: Callable[[tuple[int, str, str, str], tuple[int, str, str, str]], None],
) -> None:
    """Conservatively reduce a large Rule-B cluster, then run the exact matcher.

    This is a no-false-negative prefilter for ``SequenceMatcher.ratio() >= 0.90``. If its matching
    blocks contain ``M`` characters and the two text lengths sum to ``T``, the ratio condition means
    ``2M/T >= 0.90``. Therefore:

    * ``2*min(len(a), len(b))/T >= 0.90`` is necessary; and
    * the L1 distance between character-count histograms is at most ``T - 2M <= 0.10T``.

    Aggregating characters into disjoint coarse bins can only lower that L1 distance (triangle
    inequality), so the coarse filter is also conservative. Every surviving pair is checked against
    the full histogram and then by the unchanged exact SequenceMatcher predicate in ``exact_pair``.
    """
    try:
        import numpy as np
    except ImportError as exc:
        raise RuntimeError(
            "dataset duplicate audit requires numpy for a large Rule-B cluster; refusing an "
            "unbounded quadratic scan"
        ) from exc

    max_text_len = max(len(entry[3]) for entry in cluster)
    if max_text_len >= 32_768:
        raise RuntimeError(
            "dataset duplicate audit encountered a >=32768-character segment; refusing an unsafe "
            "int16 histogram optimization"
        )

    characters = sorted({character for _, _, _, text in cluster for character in text})
    character_index = {character: index for index, character in enumerate(characters)}
    histograms = np.zeros((len(cluster), len(characters)), dtype=np.int16)
    global_frequency: Counter[str] = Counter()
    for row, (_, _, _, text) in enumerate(cluster):
        counts = Counter(text)
        global_frequency.update(counts)
        for character, count in counts.items():
            histograms[row, character_index[character]] = count

    # Preserve the most informative high-frequency characters as their own dimensions and merge the
    # sparse tail into one final bin. Any disjoint partition is conservative; this data-driven order
    # only improves speed. Character tie-breaking makes the matrix deterministic.
    ranked_characters = sorted(characters, key=lambda c: (-global_frequency[c], c))
    coarse = np.zeros((len(cluster), RULE_B_COARSE_BINS), dtype=np.int16)
    for rank, character in enumerate(ranked_characters):
        coarse_bin = min(rank, RULE_B_COARSE_BINS - 1)
        coarse[:, coarse_bin] += histograms[:, character_index[character]]

    offsets = np.asarray([entry[0] for entry in cluster], dtype=np.int64)
    lengths = np.asarray([len(entry[3]) for entry in cluster], dtype=np.int32)
    file_ids_by_name = {name: index for index, name in enumerate(sorted({entry[2] for entry in cluster}))}
    file_ids = np.asarray([file_ids_by_name[entry[2]] for entry in cluster], dtype=np.int32)
    fourgram_cache: dict[int, Counter[int]] = {}

    def fourgrams(row: int) -> Counter[int]:
        cached = fourgram_cache.get(row)
        if cached is not None:
            return cached
        codes = [character_index[character] for character in cluster[row][3]]
        # There are normally fewer than 100 characters in the alphabet. Packing four character IDs
        # into one Python integer avoids retaining millions of duplicate substring objects.
        if len(characters) <= 256:
            cached = Counter(
                (codes[index] << 24)
                | (codes[index + 1] << 16)
                | (codes[index + 2] << 8)
                | codes[index + 3]
                for index in range(len(codes) - 3)
            )
        else:
            cached = Counter(
                tuple(codes[index : index + 4]) for index in range(len(codes) - 3)
            )
        fourgram_cache[row] = cached
        return cached

    for start in range(0, len(cluster), RULE_B_VECTOR_BLOCK_ROWS):
        stop = min(len(cluster), start + RULE_B_VECTOR_BLOCK_ROWS)
        right = np.arange(start, stop)
        left = np.arange(stop)
        total_length = lengths[right, None] + lengths[None, :stop]
        eligible = (
            (left[None, :] < right[:, None])
            & (offsets[right, None] - offsets[None, :stop] <= OFFSET_BUCKET_MS)
            & (file_ids[right, None] != file_ids[None, :stop])
            & (20 * np.minimum(lengths[right, None], lengths[None, :stop]) >= 9 * total_length)
        )

        coarse_l1 = np.abs(coarse[right, None, :] - coarse[None, :stop, :]).sum(
            axis=2, dtype=np.int32
        )
        eligible &= 10 * coarse_l1 <= total_length
        candidate_right, candidate_left = np.nonzero(eligible)
        if not len(candidate_right):
            continue

        absolute_right = right[candidate_right]
        full_l1 = np.abs(histograms[absolute_right] - histograms[candidate_left]).sum(
            axis=1, dtype=np.int32
        )
        exact_candidates = 10 * full_l1 <= total_length[candidate_right, candidate_left]
        for right_index, left_index in zip(
            absolute_right[exact_candidates], candidate_left[exact_candidates], strict=True
        ):
            right_row, left_row = int(right_index), int(left_index)
            pair_total_length = len(cluster[left_row][3]) + len(cluster[right_row][3])
            minimum_matches = (9 * pair_total_length + 19) // 20
            # If SequenceMatcher reaches 90%, its ordered matching blocks contain at least M
            # characters and at most U+1 blocks, where U=T-2M. Each block contributes at least
            # length-3 four-grams, so the pair must share >= M-3(U+1) four-gram occurrences. This is
            # another necessary condition, not a replacement similarity metric.
            minimum_fourgram_overlap = 7 * minimum_matches - 3 * pair_total_length - 3
            left_fourgrams = fourgrams(left_row)
            right_fourgrams = fourgrams(right_row)
            if len(left_fourgrams) > len(right_fourgrams):
                left_fourgrams, right_fourgrams = right_fourgrams, left_fourgrams
            overlap = sum(
                min(count, right_fourgrams.get(gram, 0)) for gram, count in left_fourgrams.items()
            )
            if overlap < minimum_fourgram_overlap:
                continue
            exact_pair(cluster[left_row], cluster[right_row])


def _clip_pcm(audio_path: str, alignment_json: str):
    """The clip's own samples as `(mono, samplerate)`, or None when they cannot be read.

    Reads only the clip's span out of the source file rather than the whole recording — these are
    audiobook chapters, and decoding every one to compare two sentences would make the gate unusable.

    The rate travels WITH the samples because the library holds both the 48 kHz masters and 16 kHz
    WAVs of the same material: a caller that assumes one rate compares two different clocks.
    """
    try:
        import numpy as np
        import soundfile as sf
    except ImportError:
        return None
    try:
        meta = json.loads(alignment_json or "{}")
        start_ms = int(meta.get("source_start_ms", -1))
        end_ms = int(meta.get("source_end_ms", -1))
    except (ValueError, TypeError):
        return None
    if start_ms < 0 or end_ms <= start_ms or not os.path.isfile(audio_path):
        return None
    try:
        info = sf.info(audio_path)
        start = int(start_ms * info.samplerate / 1000)
        stop = min(int(end_ms * info.samplerate / 1000), info.frames)
        if stop <= start:
            return None
        data, _ = sf.read(audio_path, start=start, stop=stop, dtype="float32", always_2d=True)
    except Exception:
        return None
    mono = data.mean(axis=1)
    return (mono, int(info.samplerate)) if mono.size else None


def _resampled(x, src_rate: int, dst_rate: int, np):
    """`x` on `dst_rate`'s clock, by linear interpolation — no new dependency for one downsample."""
    if src_rate == dst_rate:
        return x.astype("float64")
    n = int(round(x.size * dst_rate / src_rate))
    if n <= 0:
        return x[:0].astype("float64")
    return np.interp(
        np.arange(n, dtype="float64") * (src_rate / dst_rate),
        np.arange(x.size, dtype="float64"),
        x.astype("float64"),
    )


def audio_alignment(a, b, a_rate: int = 16000, b_rate: int = 16000) -> tuple[float, int, float] | None:
    """(correlation, lag_ms, overlap_ratio) at the best lag inside ±AUDIO_MAX_LAG_MS, or no verdict.

    The lag is found by FFT cross-correlation; the reported correlation is the normalised dot product
    of the two clips over their overlapping span at that lag, so an identical recording scores ~1.0
    whether or not its cut boundaries agree. A candidate lag whose overlap covers less than
    AUDIO_MIN_OVERLAP_RATIO of the shorter clip is ignored. Two clips whose durations differ by more
    than AUDIO_DURATION_TOLERANCE_MS are declared unrelated (0.0) before any decode-heavy work.
    """
    try:
        import numpy as np
    except ImportError:
        return None
    if a is None or b is None or a.size == 0 or b.size == 0:
        return None
    if abs(a.size / a_rate - b.size / b_rate) * 1000 > AUDIO_DURATION_TOLERANCE_MS:
        return 0.0, 0, 0.0
    rate = min(a_rate, b_rate)
    x = _resampled(a, a_rate, rate, np).astype("float64")
    y = _resampled(b, b_rate, rate, np).astype("float64")
    x -= x.mean()
    y -= y.mean()
    if float(np.linalg.norm(x)) == 0.0 or float(np.linalg.norm(y)) == 0.0:
        return None
    size = 1 << (x.size + y.size - 2).bit_length()
    cross = np.fft.irfft(np.fft.rfft(x, size) * np.conj(np.fft.rfft(y, size)), size)
    # lags -(len(y)-1) .. len(x)-1: positive lag means x starts later than y.
    cross = np.concatenate([cross[-(y.size - 1) :], cross[: x.size]]) if y.size > 1 else cross[: x.size]
    lags = np.arange(-(y.size - 1), x.size)
    max_lag = int(AUDIO_MAX_LAG_MS * rate / 1000)
    window = np.abs(lags) <= max_lag
    if not window.any():
        return None
    shorter = min(x.size, y.size)
    best: tuple[float, int, float] = (0.0, 0, 0.0)
    # The raw cross-correlation peak is not normalised for overlap; score the few best candidates.
    for index in np.argsort(cross[window])[::-1][:5]:
        lag = int(lags[window][index])
        xs, ys = (x[lag:], y) if lag >= 0 else (x, y[-lag:])
        overlap = min(xs.size, ys.size)
        if overlap < AUDIO_MIN_OVERLAP_RATIO * shorter or overlap == 0:
            continue
        xs, ys = xs[:overlap], ys[:overlap]
        denom = float(np.linalg.norm(xs) * np.linalg.norm(ys))
        if denom == 0.0:
            continue
        score = float(np.dot(xs, ys)) / denom
        if score > best[0]:
            best = (score, round(lag * 1000 / rate), overlap / shorter)
    return best


def audio_correlation(a, b, a_rate: int = 16000, b_rate: int = 16000) -> float | None:
    """The exact normalised waveform score used by the duplicate verdict, or no verdict."""
    alignment = audio_alignment(a, b, a_rate, b_rate)
    return None if alignment is None else alignment[0]


def audio_says_duplicate(a, b, a_rate: int = 16000, b_rate: int = 16000) -> bool | None:
    """True/False when the audio can decide, None when unreadable or inconclusive.

    None is deliberately NOT False: missing audio OR correlated but non-identical complete clips
    remain unconfirmed. A high score cannot authorize discarding unique boundary content.
    """
    alignment = audio_alignment(a, b, a_rate, b_rate)
    return whole_clip_verdict(a, b, a_rate, b_rate, alignment)


def whole_clip_verdict(a, b, a_rate, b_rate, alignment):
    """Correlation nominates candidates; only complete PCM equivalence authorizes retirement.

    Ignore a constant DC offset, not gain, encoding noise, shifted boundaries, or differing words.
    No new empirical cutoff is invented: non-identical candidates stay UNKNOWN for review.
    This conservative rule intentionally trades automatic coverage for no unique-content loss.
    """
    if alignment is None:
        return None
    if alignment[0] < AUDIO_PROBABLE_CORRELATION:
        return False
    import numpy as np

    if a_rate == b_rate and a.size == b.size and a.size:
        left, right = a.astype("float64"), b.astype("float64")
        if np.isfinite(left).all() and np.isfinite(right).all() and np.array_equal(left - left[0], right - right[0]):
            return True
    return None


def confirm_groups_with_audio(groups, rows, *, include_proof: bool = False, include_probable: bool = False):
    """Split candidates into exact duplicate components, unconfirmed risks, and clear repeats.

    Only cross-file pairs are relevant. A true pair joins just those waveform-connected members; it
    must not condemn every other reading in the same transcript group. Unreadable cross-file pairs
    remain fail-closed as unconfirmed components. With `include_probable`, pairs scoring inside
    [AUDIO_PROBABLE_CORRELATION, AUDIO_DUPLICATE_CORRELATION) are returned as a trailing list of
    (left_id, right_id, correlation) — surfaced for a human ear, never excluded by machine.
    """
    by_id = {r[0]: r for r in rows}
    def classify(group):
        pcm_cache: dict[str, object] = {}

        def pcm_for(seg_id: str):
            if seg_id not in pcm_cache:
                _, path, alignment, _, _ = by_id[seg_id]
                pcm_cache[seg_id] = _clip_pcm(path, alignment)
            return pcm_cache[seg_id]

        parent = list(range(len(group)))

        def find(index: int) -> int:
            while parent[index] != index:
                parent[index] = parent[parent[index]]
                index = parent[index]
            return index

        def union(left: int, right: int) -> None:
            left_root, right_root = find(left), find(right)
            if left_root != right_root:
                parent[right_root] = left_root

        true_edges: list[tuple[int, int, object]] = []
        unknown_edges: list[tuple[int, int]] = []
        probable_edges: list[tuple[str, str, float]] = []
        compared = 0
        for i in range(len(group)):
            for j in range(i + 1, len(group)):
                if group[i][1] == group[j][1]:
                    continue
                compared += 1
                alignment = None
                pa, pb = pcm_for(group[i][0]), pcm_for(group[j][0])
                if include_proof or include_probable:
                    alignment = audio_alignment(pa[0], pb[0], pa[1], pb[1]) if pa and pb else None
                    verdict = whole_clip_verdict(pa[0], pb[0], pa[1], pb[1], alignment) if pa and pb else None
                    if alignment is not None and AUDIO_PROBABLE_CORRELATION <= alignment[0] < AUDIO_DUPLICATE_CORRELATION:
                        probable_edges.append((group[i][0], group[j][0], alignment[0]))
                else:
                    verdict = (
                        audio_says_duplicate(pa[0], pb[0], pa[1], pb[1]) if pa and pb else None
                    )
                if verdict is True:
                    true_edges.append((i, j, alignment))
                    union(i, j)
                elif verdict is None:
                    unknown_edges.append((i, j))

        true_components: dict[int, list] = defaultdict(list)
        true_members = {member for edge in true_edges for member in edge[:2]}
        for member in sorted(true_members):
            true_components[find(member)].append(group[member])
        confirmed = [
            component
            for component in true_components.values()
            if len({source_file for _, source_file in component}) > 1
        ]

        # Contract confirmed components to one root, then join every still-unknown relation. This
        # reports each connected uncertainty once while preserving any confirmed component it
        # touches. Overlap between confirmed/unconfirmed output is intentional provenance, not an
        # additional duplicate count.
        unknown_parent = list(range(len(group)))

        def unknown_find(index: int) -> int:
            while unknown_parent[index] != index:
                unknown_parent[index] = unknown_parent[unknown_parent[index]]
                index = unknown_parent[index]
            return index

        def unknown_union(left: int, right: int) -> None:
            left_root, right_root = unknown_find(left), unknown_find(right)
            if left_root != right_root:
                unknown_parent[right_root] = left_root

        for i, j, _ in true_edges:
            unknown_union(i, j)
        for i, j in unknown_edges:
            unknown_union(i, j)
        unknown_roots = {unknown_find(member) for edge in unknown_edges for member in edge}
        unknown_components = [
            [group[index] for index in range(len(group)) if unknown_find(index) == root]
            for root in sorted(unknown_roots)
        ]
        unconfirmed = [
            component
            for component in unknown_components
            if len({source_file for _, source_file in component}) > 1
        ]
        repeats = [group] if compared and not true_edges and not unknown_edges else []
        proof = []
        if include_proof:
            for component in confirmed:
                member_ids = {segment_id for segment_id, _ in component}
                edges = [
                    {
                        "leftSegmentId": group[i][0],
                        "rightSegmentId": group[j][0],
                        "correlationPpm": round(float(alignment[0]) * 1_000_000),
                        "lagMs": int(alignment[1]),
                        "overlapPpm": round(float(alignment[2]) * 1_000_000),
                    }
                    for i, j, alignment in true_edges
                    if group[i][0] in member_ids and group[j][0] in member_ids and alignment is not None
                ]
                proof.append({"members": component, "edges": edges})
        return confirmed, unconfirmed, repeats, proof, probable_edges

    confirmed, unconfirmed, repeats, proof, probable = [], [], [], [], []
    # Each segment belongs to one text-candidate group, so per-group caches avoid retaining decoded
    # PCM for the entire corpus. A small bounded worker pool overlaps independent disk reads without
    # competing with the live application or consuming a GPU.
    if len(groups) >= 32:
        with ThreadPoolExecutor(max_workers=min(8, os.cpu_count() or 1)) as executor:
            classified = executor.map(classify, groups)
            for group_confirmed, group_unconfirmed, group_repeats, group_proof, group_probable in classified:
                confirmed.extend(group_confirmed)
                unconfirmed.extend(group_unconfirmed)
                repeats.extend(group_repeats)
                proof.extend(group_proof)
                probable.extend(group_probable)
    else:
        for group in groups:
            group_confirmed, group_unconfirmed, group_repeats, group_proof, group_probable = classify(group)
            confirmed.extend(group_confirmed)
            unconfirmed.extend(group_unconfirmed)
            repeats.extend(group_repeats)
            proof.extend(group_proof)
            probable.extend(group_probable)
    result: tuple = (confirmed, unconfirmed, repeats)
    if include_proof:
        result += (proof,)
    if include_probable:
        result += (sorted(probable),)
    return result


def load_audit_rows(con: sqlite3.Connection) -> tuple[list[tuple[str, str, str, str, int]], str]:
    """Load the exact corpus that may still be served or exported.

    A v64+ pool retains excluded duplicates as immutable provenance rows. Auditing those rows as if
    they were canonical work rejects the exclusion authority itself. Verify the manifest and its
    exclusion counts first, then narrow to the canonical overlay. Missing or unapplied dedup
    authority keeps the full source pool in scope and therefore remains fail-closed.
    """
    pool_registry_exists = con.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_registry'"
    ).fetchone()
    pool_members_exist = con.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_members'"
    ).fetchone()
    active_pool = None
    if pool_registry_exists and pool_members_exist:
        active_pool = con.execute(
            "SELECT pool_id, focus_segment_count FROM review_pool_registry WHERE singleton_key = 1"
        ).fetchone()
    if not active_pool:
        rows = con.execute(
            "SELECT id, audio_path, alignment_json, raw_transcript, verified FROM speech_segments"
        ).fetchall()
        return rows, f"full live library ({len(rows)} clips)"

    pool_id, expected_count = active_pool
    expected_count = int(expected_count)
    actual_count = int(
        con.execute("SELECT COUNT(*) FROM review_pool_members WHERE pool_id = ?", (pool_id,)).fetchone()[0]
    )
    if actual_count != expected_count:
        raise ValueError(
            f"active pool {pool_id} is structurally incomplete: registry expects "
            f"{expected_count}, found {actual_count}"
        )

    manifest_table = con.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_dedup_manifests'"
    ).fetchone()
    exclusions_table = con.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_duplicate_exclusions'"
    ).fetchone()
    if bool(manifest_table) != bool(exclusions_table):
        raise ValueError("dedup authority tables are only partially present")

    dedup = None
    if manifest_table:
        dedup = con.execute(
            """SELECT source_focus_segment_count, canonical_count, excluded_count,
                      unconfirmed_risk_count
                 FROM review_pool_dedup_manifests
                WHERE pool_id = ?""",
            (pool_id,),
        ).fetchone()
        supersessions_table = con.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_dedup_supersessions'"
        ).fetchone()
        if dedup and supersessions_table:
            # Schema 70: a superseding (v2) manifest restates the whole dedup state; its counts win.
            latest = con.execute(
                """SELECT canonical_count, excluded_count, unconfirmed_risk_count
                     FROM review_pool_dedup_supersessions WHERE pool_id = ?
                    ORDER BY sequence DESC LIMIT 1""",
                (pool_id,),
            ).fetchone()
            if latest:
                dedup = (dedup[0], latest[0], latest[1], latest[2])
    if dedup:
        source_count, canonical_count, excluded_count, unconfirmed_risk_count = map(int, dedup)
        actual_excluded = int(
            con.execute(
                "SELECT COUNT(*) FROM review_pool_duplicate_exclusions WHERE pool_id = ?", (pool_id,)
            ).fetchone()[0]
        )
        if (
            source_count != expected_count
            or canonical_count + excluded_count != source_count
            or actual_excluded != excluded_count
            or unconfirmed_risk_count != 0
        ):
            raise ValueError(
                f"active pool {pool_id} has inconsistent dedup authority: source={source_count}, "
                f"canonical={canonical_count}, excluded={excluded_count}/{actual_excluded}, "
                f"unconfirmedRisk={unconfirmed_risk_count}, registry={expected_count}"
            )
        rows = con.execute(
            """SELECT s.id, s.audio_path,
                      printf('{\"source_start_ms\":%d,\"source_end_ms\":%d}',
                             p.source_start_ms, p.source_end_ms),
                      p.raw_transcript, s.verified
                 FROM review_pool_members p
                 JOIN speech_segments s ON s.id = p.segment_id
                 LEFT JOIN review_pool_duplicate_exclusions exclusion
                   ON exclusion.pool_id = p.pool_id AND exclusion.segment_id = p.segment_id
                WHERE p.pool_id = ? AND exclusion.segment_id IS NULL""",
            (pool_id,),
        ).fetchall()
        if len(rows) != canonical_count:
            raise ValueError(
                f"active pool {pool_id} canonical overlay expects {canonical_count}, found {len(rows)}"
            )
        return rows, f"active immutable canonical review pool {pool_id} ({canonical_count} clips)"

    rows = con.execute(
        """SELECT s.id, s.audio_path,
                  printf('{\"source_start_ms\":%d,\"source_end_ms\":%d}',
                         p.source_start_ms, p.source_end_ms),
                  p.raw_transcript, s.verified
             FROM review_pool_members p
             JOIN speech_segments s ON s.id = p.segment_id
            WHERE p.pool_id = ?""",
        (pool_id,),
    ).fetchall()
    return rows, f"active immutable source review pool {pool_id} ({actual_count} clips; dedup unapplied)"


def main() -> int:
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        print(f"DATASET DUPLICATES: SKIP-ENV (no library at {db})", flush=True)
        return 0
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows, scope = load_audit_rows(con)
        print(f"  scope: {scope}", flush=True)
    except (sqlite3.Error, ValueError) as error:
        print(f"DATASET DUPLICATES: FAIL\n  {error}", flush=True)
        return 1
    finally:
        con.close()

    candidates = duplicate_groups(rows)
    # RULE C: the audio decides. Text-matched groups whose clips are demonstrably DIFFERENT audio are
    # a narrator repeating a sentence, not a duplicated import.
    groups, unconfirmed, repeats, probable = confirm_groups_with_audio(candidates, rows, include_probable=True)
    if probable:
        print(
            f"  note: {len(probable)} cross-file pair(s) score between {AUDIO_PROBABLE_CORRELATION} and "
            f"{AUDIO_DUPLICATE_CORRELATION} at the best lag — above the control ceiling, below the confirmed "
            f"bar; listed for a human ear, never excluded by machine",
            flush=True,
        )
    confirmed_redundant = sum(len({source_file for _, source_file in group}) - 1 for group in groups)
    unconfirmed_risks = sum(
        len({source_file for _, source_file in group}) - 1 for group in unconfirmed
    )
    redundant = confirmed_redundant + unconfirmed_risks

    if repeats:
        print(
            f"  note: {len(repeats)} text-matched group(s) cleared by audio — the same sentence read "
            f"more than once (series intros, repeated verses), not a duplicated recording",
            flush=True,
        )

    if redundant > KNOWN_BASELINE:
        print("DATASET DUPLICATES: FAIL", flush=True)
        print(
            f"  {confirmed_redundant} confirmed redundant source file(s) across {len(groups)} "
            f"waveform-connected group(s), plus {unconfirmed_risks} fail-closed unconfirmed "
            f"cross-file risk(s) across {len(unconfirmed)} group(s) — ABOVE the "
            f"recorded baseline of {KNOWN_BASELINE}, so a duplicate recording has been imported since "
            f"this gate was written. Same recording, different encode: the byte fingerprint cannot see "
            f"it; this offset+text audit can.",
            flush=True,
        )
        if unconfirmed:
            print(
                f"  {len(unconfirmed)} of those could NOT be confirmed from audio (missing file, "
                f"unreadable span, missing dependencies, or non-identical full content) — unresolved "
                f"risk keeps this gate closed; it is not permission to exclude a clip",
                flush=True,
            )
        by_file: dict[frozenset, int] = defaultdict(int)
        for g in groups + unconfirmed:
            by_file[frozenset(f for _, f in g)] += 1
        for files, n in sorted(by_file.items(), key=lambda kv: -kv[1])[:10]:
            print(f"    {n:4} groups across: {', '.join(sorted(files))}", flush=True)
        return 1

    if redundant:
        print(
            f"DATASET DUPLICATES: OK-WITH-BASELINE ({redundant} known redundant clips, baseline "
            f"{KNOWN_BASELINE} — the owner-gated cleanup will ratchet this to 0; the count must only "
            f"ever go DOWN)",
            flush=True,
        )
    else:
        print("DATASET DUPLICATES: OK (no cross-file duplicate content)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
