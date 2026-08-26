"""Fail closed when public model metrics outrun immutable model evidence.

The integrated release has no current model attestation yet. Historical measurements may remain for
reproducibility, but the duplication-weighted N=922 result must never read as a current headline.
When a current attestation is introduced, this policy requires a canonical SHA-256 identity before
any public surface can reference it.
"""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = ROOT.parent
ATTESTATION = ROOT / "docs" / "eval" / "current-model-attestation.json"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def test_duplication_weighted_score_is_never_a_current_headline() -> None:
    evaluation = read(ROOT / "docs" / "EVAL.md")
    required = (
        "HISTORICAL — DUPLICATION-WEIGHTED — NON-PRIMARY",
        "922 rows contain 348 distinct clips",
        "No current headline",
        "clean N=348",
    )
    for marker in required:
        if marker not in evaluation:
            raise AssertionError(f"docs/EVAL.md lost required historical-metric marker: {marker}")
    forbidden = ("measured-best local engine", "FLEURS 7.03% is the\nhonest headline")
    for claim in forbidden:
        if claim in evaluation:
            raise AssertionError(f"docs/EVAL.md revived an unattested current claim: {claim}")

    guide = read(ROOT / "docs" / "CORTEX_APP_FLOW_GUIDE.html")
    seven_lines = [line.strip() for line in guide.splitlines() if "7.03%" in line]
    if not seven_lines:
        raise AssertionError("flow guide lost the historical provenance record entirely")
    for line in seven_lines:
        if "historical" not in line.casefold() or "non-primary" not in line.casefold():
            raise AssertionError(
                "flow guide presents the duplication-weighted 7.03% without an inline historical, "
                f"non-primary label: {line}"
            )


def test_public_runtime_sources_do_not_embed_an_unattested_metric() -> None:
    offenders: list[str] = []
    for path in sorted((ROOT / "src").rglob("*")):
        if path.suffix.lower() not in {".svelte", ".ts", ".js", ".html"} or not path.is_file():
            continue
        text = read(path)
        if re.search(r"\b7\.03\s*%|\b32\.93\s*%", text):
            offenders.append(str(path.relative_to(ROOT)))
    if offenders:
        raise AssertionError(
            "runtime UI embeds the unattested duplicated-row metric: " + ", ".join(offenders)
        )

    readme = read(WORKSPACE / "README.md")
    if "Historical diagnostic accuracy (not production-champion evidence)" not in readme:
        raise AssertionError("README no longer labels its stock-300M number as historical")


def test_active_engine_guidance_does_not_depend_on_historical_metrics() -> None:
    active_sources = {
        "CLAUDE.md": read(ROOT / "CLAUDE.md"),
        "src-tauri/src/pipeline.rs": read(ROOT / "src-tauri" / "src" / "pipeline.rs"),
    }
    for label, source in active_sources.items():
        if re.search(r"\b7\.03\s*%|\b9\.32\s*%|\b21\.00\s*%", source):
            raise AssertionError(f"{label} embeds a historical metric in active engine guidance")
        if not re.search(r"Historical\s+(?:\S+\s+){0,2}duplication-weighted", source):
            raise AssertionError(f"{label} lost the explicit historical-evidence boundary")

    doctrine = read(ROOT / "docs" / "ACCURACY_USEFULNESS_LOOP.md")
    for forbidden in ("at/above verifiable Sorani SOTA", "protects the 7.03% headline"):
        if forbidden in doctrine:
            raise AssertionError(f"accuracy doctrine revived an unattested claim: {forbidden}")
    for required in ("No current model attestation exists", "922 manifest", "348 distinct"):
        if required not in doctrine:
            raise AssertionError(f"accuracy doctrine lost evidence boundary: {required}")

    measurements = read(ROOT / "docs" / "MEASUREMENTS.md")
    normalized_measurements = re.sub(r"\s+", " ", re.sub(r"(?m)^>\s?", "", measurements))
    for required in (
        "Historical measurement archive, not a current model attestation",
        "duplication-weighted",
        "do not use it as a current headline",
    ):
        if required not in normalized_measurements:
            raise AssertionError(f"measurement archive lost historical marker: {required}")

    scan = read(ROOT / "docs" / "ASR_TECH_SCAN_2026-07-23.md")
    if "Superseded 2026-08-26" not in scan or "No current SOTA or release claim is authorized" not in scan:
        raise AssertionError("dated ASR scan still lacks an explicit supersession boundary")

    active_metric_offenders: list[str] = []
    active_suffixes = {".cjs", ".js", ".mjs", ".py", ".rs", ".ts"}
    this_policy = Path(__file__).resolve()
    for source_root in (ROOT / "src-tauri" / "src", ROOT / "scripts"):
        for path in sorted(source_root.rglob("*")):
            if (
                not path.is_file()
                or path.suffix.lower() not in active_suffixes
                or path.resolve() == this_policy
            ):
                continue
            if re.search(r"\b7\.03\s*%|\b32\.93\s*%", read(path)):
                active_metric_offenders.append(str(path.relative_to(ROOT)))
    if active_metric_offenders:
        raise AssertionError(
            "active implementation or policy source embeds an unattested duplicated-row metric: "
            + ", ".join(active_metric_offenders)
        )


def test_current_attestation_identity_is_canonical_when_present() -> None:
    if not ATTESTATION.exists():
        return
    try:
        value = json.loads(read(ATTESTATION))
    except json.JSONDecodeError as error:
        raise AssertionError(f"current model attestation is not valid JSON: {error}") from error
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise AssertionError("current model attestation must be a schema-1 object")
    for field, length in (("fullGitSha", 40), ("modelSha256", 64), ("scorecardSha256", 64)):
        candidate = value.get(field)
        if not isinstance(candidate, str) or not re.fullmatch(rf"[0-9a-f]{{{length}}}", candidate):
            raise AssertionError(f"current model attestation has no canonical {field}")


def main() -> None:
    test_duplication_weighted_score_is_never_a_current_headline()
    test_public_runtime_sources_do_not_embed_an_unattested_metric()
    test_active_engine_guidance_does_not_depend_on_historical_metrics()
    test_current_attestation_identity_is_canonical_when_present()
    print("model-claim attestation policy passed")


if __name__ == "__main__":
    main()
