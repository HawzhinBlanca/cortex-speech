"""Gate: machine text must never sit in the human-only field, and untouched clips must serve champion text.

Why this gate exists (incident, 2026-08-12): the phone review server's ``review_text()`` rightly
prefers ``annotated_transcript`` — the field reserved for a HUMAN's typed correction — over the raw
champion draft. Machine writers had filled that field on 348 rows (the batch persist path seeded it
via ``COALESCE(annotated_transcript, machine_draft)`` — a LIVE writer at the time, removed and
pinned by ``test_machine_never_writes_annotated_policy.py``; the desktop re-transcribe handlers did
the same), so reviewers were served a stale machine paraphrase while every fresh champion +
refinement draft sat invisible in ``raw_transcript`` / ``normalized_transcript``. Words the speaker
never said reached human reviewers labeled as the draft.

The law this gate enforces, on the LIVE database, at the serving path's own precedence:

  1. ``annotated_transcript`` is human-only: a non-empty value requires human evidence on that row
     (a review event, a recorded human_decision, or verified=1).
  2. Every clip with NO human evidence must fall back to text the CURRENT registry champion
     actually produced: its ``raw_transcript`` must byte-match (modulo surrounding whitespace) a
     stored hypothesis carrying that champion's content-addressed id. The one pre-registry id is
     accepted only while the registry points at the exact deployment it historically aliases. No
     champion hypothesis on file = FAIL (hard-stop law: an undrafted clip may not sit in the queue
     looking drafted).
  3. An ACCEPT may only freeze text some engine actually produced. "Accept" means *this draft is
     what the audio says* — the reviewer typed nothing — so the stored text must match one of the
     segment's own ASR hypotheses. Text matching none of them was authored by a model that is not an
     ASR (the LLM refiner), and an accept laundered it into human-verified status.

Why invariant 3 exists (measured 2026-08-13, after the 348-row clear): 36 accepts still held text
matching NO engine on file — not the champion, not the fine-tuned MMS, not CTC-300M/1B — while
invariant 1 passed them, because a recorded ``human_decision`` counts as "human evidence" for the
row. It is evidence a human clicked a button; it is NOT evidence a human authored the words. Those
rows exported as ``transcript_source = human_verified`` carrying a refiner's paraphrase (measured
median 5.2%, max 14.2% character divergence from the champion: inserted punctuation, substituted
conjunctions, repaired grammar). Verbatim law says a decision inherits the text a machine wrote only
when that machine was an ASR reporting what it heard.

Run:  python scripts/check_review_serving_provenance.py [db_path]
      (defaults to %APPDATA%/cortex-speech/cortex-speech.db, override with CORTEX_DB)
"""

import os
import sqlite3
import sys

ASR_CHAMPION_FAMILY = "omniasr-7b"

# Historical recognition, never model selection. Before the registry existed, this exact incumbent
# wrote hypotheses under the generic engine id below. The bootstrap later content-addressed the SAME
# measured deployment. Admit the old id only while BOTH immutable identity pins are the current
# registry champion; a promotion, a changed deployment, or an empty/broken registry drops the alias.
LEGACY_ALIAS_MODEL_ID = "omniasr-wsl-7b"
PROVEN_LEGACY_CANONICAL_MODEL_ID = "omniasr-7b-legacy-c348ade8a816"
PROVEN_LEGACY_DEPLOYMENT_SHA256 = "ae33143ec8b25f45e393f4aa484c3a3d165850f0dc15e95254dd6e4cb4c05cbf"


class ChampionRegistryError(RuntimeError):
    """The current ASR champion cannot be resolved to one canonical, pinned identity."""


def _valid_model_id(value: object) -> bool:
    """Mirror the app's identifier boundary: bounded alphanumeric plus ``_.-`` only."""
    return (
        isinstance(value, str)
        and 0 < len(value) <= 256
        and all(character.isalnum() or character in "_.-" for character in value)
    )


def _valid_sha256(value: object) -> bool:
    """Deployment identities are canonical lowercase SHA-256, never an unpinned label."""
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def current_champion_model_ids(conn: sqlite3.Connection) -> tuple[str, ...]:
    """Resolve the one current ASR champion and its narrowly proven pre-registry alias.

    The model registry is authoritative. ``champion.json`` is only its startup mirror and can be
    between syncs, so this database invariant must not silently fall back to that file or to a fixed
    engine label. Every malformed/ambiguous state raises and therefore fails the gate closed.
    """
    try:
        rows = conn.execute(
            """
            SELECT id, checkpoint_sha256
            FROM model_versions
            WHERE family = ? AND status = 'champion'
            ORDER BY id
            """,
            (ASR_CHAMPION_FAMILY,),
        ).fetchall()
    except sqlite3.Error as exc:
        raise ChampionRegistryError(f"could not read model registry: {exc}") from exc

    if len(rows) != 1:
        raise ChampionRegistryError(
            f"expected exactly one {ASR_CHAMPION_FAMILY} champion, found {len(rows)}"
        )

    model_id, deployment_sha256 = rows[0]
    if not _valid_model_id(model_id):
        raise ChampionRegistryError("current ASR champion has an invalid model id")
    if not _valid_sha256(deployment_sha256):
        raise ChampionRegistryError("current ASR champion has no canonical deployment SHA-256")

    allowed = [model_id]
    if (model_id, deployment_sha256) == (
        PROVEN_LEGACY_CANONICAL_MODEL_ID,
        PROVEN_LEGACY_DEPLOYMENT_SHA256,
    ):
        allowed.append(LEGACY_ALIAS_MODEL_ID)
    return tuple(allowed)


def default_db_path() -> str:
    override = os.environ.get("CORTEX_DB")
    if override:
        return override
    return os.path.join(os.environ["APPDATA"], "cortex-speech", "cortex-speech.db")


def human_field_violations(conn: sqlite3.Connection) -> list:
    """Rows where the human-only field is populated with no human evidence anywhere."""
    return conn.execute(
        """
        SELECT s.id
        FROM speech_segments s
        WHERE TRIM(COALESCE(s.annotated_transcript, '')) <> ''
          AND COALESCE(s.human_decision, '') = ''
          AND s.verified = 0
          AND NOT EXISTS (SELECT 1 FROM review_events e WHERE e.segment_id = s.id)
          AND NOT EXISTS (
                SELECT 1 FROM decision_log d
                WHERE d.segment_id = s.id AND COALESCE(d.human_decision, '') <> ''
          )
        ORDER BY s.id
        """
    ).fetchall()


def non_champion_fallbacks(conn: sqlite3.Connection, champion_model_ids: tuple[str, ...]) -> list:
    """Untouched rows whose served fallback text is not the champion's own output."""
    if not champion_model_ids:
        raise ChampionRegistryError("current ASR champion resolved to an empty model-id set")
    placeholders = ",".join("?" for _ in champion_model_ids)
    rows = conn.execute(
        f"""
        SELECT s.id, s.raw_transcript,
               EXISTS (
                   SELECT 1 FROM segment_hypotheses h
                   WHERE h.segment_id = s.id AND h.model_id IN ({placeholders})
               ) AS has_champion,
               EXISTS (
                   SELECT 1 FROM segment_hypotheses h
                   WHERE h.segment_id = s.id AND h.model_id IN ({placeholders})
                     AND TRIM(COALESCE(h.transcript, '')) = TRIM(COALESCE(s.raw_transcript, ''))
               ) AS champion_matches_raw
        FROM speech_segments s
        WHERE COALESCE(s.human_decision, '') = ''
          AND s.verified = 0
          AND NOT EXISTS (SELECT 1 FROM review_events e WHERE e.segment_id = s.id)
          AND NOT EXISTS (
                SELECT 1 FROM decision_log d
                WHERE d.segment_id = s.id AND COALESCE(d.human_decision, '') <> ''
          )
        """,
        (*champion_model_ids, *champion_model_ids),
    ).fetchall()
    bad = []
    for seg_id, _raw, has_champion, champion_matches_raw in rows:
        if not champion_matches_raw:
            bad.append((seg_id, "raw != champion" if has_champion else "no champion hypothesis"))
    return bad


def _norm(text) -> str:
    """Compare on words, not spacing: a differing space is not a differing transcript."""
    return " ".join((text or "").split())


def laundered_accepts(conn: sqlite3.Connection) -> list:
    """Accepts whose frozen text matches none of that segment's own ASR hypotheses.

    Precedence mirrors ``quality::training_transcript_with_source`` — the exact text the export
    ships for a human-decided row — so this reads what the consumer reads, not what a writer wrote.
    """
    rows = conn.execute(
        """
        SELECT s.id,
               COALESCE(NULLIF(TRIM(s.verdict_transcript), ''),
                        NULLIF(TRIM(s.annotated_transcript), ''),
                        s.raw_transcript),
               s.reviewed_by
        FROM speech_segments s
        WHERE LOWER(COALESCE(s.human_decision, '')) IN ('accept', 'human_accept')
           OR LOWER(COALESCE(s.verdict, '')) = 'human_accept'
        """
    ).fetchall()
    bad = []
    for seg_id, shipped, who in rows:
        hyps = [
            _norm(t)
            for (t,) in conn.execute(
                "SELECT transcript FROM segment_hypotheses WHERE segment_id = ?", (seg_id,)
            )
        ]
        if not hyps:
            bad.append((seg_id, who, "no ASR hypothesis on file to justify the accept"))
        elif _norm(shipped) not in hyps:
            bad.append((seg_id, who, "accepted text matches no engine output"))
    return bad


def _run_checks(conn: sqlite3.Connection) -> int:
    try:
        champion_model_ids = current_champion_model_ids(conn)
    except ChampionRegistryError as exc:
        print(f"FAIL [champion-registry]: {exc}")
        print("REVIEW SERVING PROVENANCE: 1 invariant(s) violated")
        return 1

    failures = 0

    ghosts = human_field_violations(conn)
    if ghosts:
        failures += 1
        print(f"FAIL [human-field]: {len(ghosts)} rows hold text in annotated_transcript with no human evidence")
        for (seg_id,) in ghosts[:10]:
            print(f"  {seg_id}")
        if len(ghosts) > 10:
            print(f"  ... and {len(ghosts) - 10} more")
    else:
        print("PASS [human-field]: every populated annotated_transcript has human evidence")

    drifted = non_champion_fallbacks(conn, champion_model_ids)
    if drifted:
        failures += 1
        print(f"FAIL [champion-fallback]: {len(drifted)} untouched rows would serve non-champion text")
        for seg_id, why in drifted[:10]:
            print(f"  {seg_id}: {why}")
        if len(drifted) > 10:
            print(f"  ... and {len(drifted) - 10} more")
    else:
        print("PASS [champion-fallback]: every untouched row serves the champion's own transcript")

    laundered = laundered_accepts(conn)
    if laundered:
        failures += 1
        print(f"FAIL [accept-provenance]: {len(laundered)} accepts freeze text no ASR engine produced")
        for seg_id, who, why in laundered[:10]:
            print(f"  {seg_id} (by {who or 'desktop'}): {why}")
        if len(laundered) > 10:
            print(f"  ... and {len(laundered) - 10} more")
    else:
        print("PASS [accept-provenance]: every accept freezes text an ASR engine actually produced")

    if failures:
        print(f"REVIEW SERVING PROVENANCE: {failures} invariant(s) violated")
        return 1
    print("REVIEW SERVING PROVENANCE: all invariants hold")
    return 0


def main() -> int:
    db_path = sys.argv[1] if len(sys.argv) > 1 else default_db_path()
    if not os.path.exists(db_path):
        print(f"FAIL: database not found: {db_path}")
        return 1
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return _run_checks(conn)
    finally:
        conn.close()


if __name__ == "__main__":
    sys.exit(main())
