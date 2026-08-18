#!/usr/bin/env python3
"""Adversarial regression tests for the fail-closed promotion gate."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
from copy import deepcopy
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from promotion_gate import (  # noqa: E402
    ChallengerRunBinding,
    GateInputError,
    Scorecard,
    SliceFile,
    decide,
    inspect_eval_manifest,
    load_provenance,
    load_challenger_run,
    load_scorecard,
    load_slices,
    main as gate_main,
    paired_significance,
)

BASIS = "nfc_lower_space_kept"
SNAPSHOT = "1" * 64
MANIFEST_SHA = "2" * 64
INDEX_SHA = "3" * 64
N = 30


def digest(label: str) -> str:
    return hashlib.sha256(label.encode("utf-8")).hexdigest()


def scorecard(
    *,
    n: int = N,
    char_dist: int = 20,
    word_dist: int = 6,
    basis: str = BASIS,
    label: str = "scorecard",
    char_fn=None,
    word_fn=None,
) -> Scorecard:
    rows = {
        clip: (
            char_fn(clip) if char_fn else char_dist,
            100,
            word_fn(clip) if word_fn else word_dist,
            20,
        )
        for clip in range(n)
    }
    return Scorecard(rows, basis, digest(label))


def provenance(role: str, card: Scorecard, **overrides) -> dict:
    suffix = "incumbent" if role == "champion" else "candidate"
    value = {
        "schema_version": 1,
        "role": role,
        "scorecard_sha256": card.sha256,
        "snapshot_id": SNAPSHOT,
        "eval_manifest_sha256": MANIFEST_SHA,
        "eval_manifest_rows": len(card.rows),
        "model_family": "omniasr-7b",
        "model_id": f"omniasr-7b-{suffix}",
        "model_sha256": digest(f"model-{suffix}"),
        "base_model_id": "omniasr-7b-owner-base-v1",
        "base_model_sha256": digest("shared-base"),
        "adapter_id": f"sorani-adapter-{suffix}",
        "adapter_sha256": digest(f"adapter-{suffix}"),
        "adapter_config_id": f"adapter-config-{suffix}.json",
        "adapter_config_sha256": digest(f"adapter-config-{suffix}"),
        "tokenizer_id": "omniasr-tokenizer-v1",
        "tokenizer_sha256": digest("shared-tokenizer"),
        "normalizer_id": "cortex-sorani-normalizer-v1",
        "normalizer_sha256": digest("shared-normalizer"),
        "norm_basis": card.norm_basis,
        "challenger_artifact_manifest_sha256": digest("challenger-artifact-manifest"),
        "challenger_deployment_manifest_sha256": digest("challenger-deployment-manifest"),
        "training_run_id": digest("challenger-training-run"),
    }
    value.update(overrides)
    return value


def run_binding(**overrides) -> ChallengerRunBinding:
    values = {
        "snapshot_id": SNAPSHOT,
        "training_run_id": digest("challenger-training-run"),
        "artifact_manifest_sha256": digest("challenger-artifact-manifest"),
        "deployment_file_sha256": digest("challenger-deployment-manifest"),
        "model_family": "omniasr-7b",
        "model_id": "omniasr-7b-candidate",
        "model_sha256": digest("model-candidate"),
        "base_model_id": "omniasr-7b-owner-base-v1",
        "base_model_sha256": digest("shared-base"),
        "adapter_id": "sorani-adapter-candidate",
        "adapter_sha256": digest("adapter-candidate"),
        "adapter_config_id": "adapter-config-candidate.json",
        "adapter_config_sha256": digest("adapter-config-candidate"),
        "tokenizer_id": "omniasr-tokenizer-v1",
        "tokenizer_sha256": digest("shared-tokenizer"),
    }
    values.update(overrides)
    return ChallengerRunBinding(**values)


def protected_groups(n: int = N) -> dict[str, list[int]]:
    clips = list(range(n))
    return {
        "source:all": clips.copy(),
        "speaker:all:unassigned": clips.copy(),
        "dialect:all": clips.copy(),
        "audio:unmeasured": clips.copy(),
    }


def slice_file(*, n: int = N, slices=None, **metadata_overrides) -> SliceFile:
    metadata = {
        "schema_version": "1",
        "eval_manifest_sha256": MANIFEST_SHA,
        "eval_manifest_rows": str(n),
        "min_clips": "5",
        "library_index_sha256": INDEX_SHA,
    }
    metadata.update(metadata_overrides)
    groups = protected_groups(n) if slices is None else slices
    return SliceFile(groups, metadata, digest("slice-file"))


def run_decision(
    champion: Scorecard,
    challenger: Scorecard,
    *,
    champion_provenance=None,
    challenger_provenance=None,
    slices: SliceFile | None = None,
    snapshot: str = SNAPSHOT,
    manifest_sha: str = MANIFEST_SHA,
    manifest_rows: int | None = None,
    challenger_run: ChallengerRunBinding | None = None,
):
    rows = len(champion.rows) if manifest_rows is None else manifest_rows
    return decide(
        champion,
        challenger,
        champion_provenance or provenance("champion", champion, eval_manifest_rows=rows),
        challenger_provenance or provenance("challenger", challenger, eval_manifest_rows=rows),
        slices or slice_file(n=rows),
        challenger_run or run_binding(),
        expected_snapshot_id=snapshot,
        eval_manifest_sha256=manifest_sha,
        eval_manifest_rows=rows,
    )


def expect_input_error(callable_) -> str:
    try:
        callable_()
    except GateInputError as error:
        return str(error)
    raise AssertionError("expected GateInputError")


def test_clear_paired_cer_and_wer_win_promotes() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    report = run_decision(champion, challenger)
    assert report["verdict"] == "PROMOTE", report["reasons"]
    assert report["champion_cer"] == 0.2 and report["challenger_cer"] == 0.1
    assert report["champion_wer"] == 0.3 and report["challenger_wer"] == 0.1
    assert {report["significance"][metric]["method"] for metric in ("cer", "wer")} == {
        "exact_two_sided_sign"
    }
    assert 0 < report["significance"]["cer"]["p_value"] < 0.05


def test_no_improvement_is_rejected() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(label="challenger")
    report = run_decision(champion, challenger)
    assert report["verdict"] == "REJECT"
    assert any("CER did not improve" in reason for reason in report["reasons"])
    assert any("WER did not improve" in reason for reason in report["reasons"])


def test_wer_regression_blocks_a_cer_win() -> None:
    report = run_decision(
        scorecard(label="champion"),
        scorecard(char_dist=10, word_dist=8, label="challenger"),
    )
    assert report["verdict"] == "REJECT"
    assert any("WER did not improve" in reason for reason in report["reasons"])


def test_insignificant_cer_and_wer_improvements_are_rejected() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(
        label="challenger",
        char_fn=lambda clip: 2 if clip % 2 == 0 else 35,
        word_fn=lambda clip: 1 if clip % 2 == 0 else 10,
    )
    report = run_decision(champion, challenger)
    assert report["champion_cer"] > report["challenger_cer"]
    assert report["champion_wer"] > report["challenger_wer"]
    assert report["verdict"] == "REJECT"
    assert any("CER improvement is not significant" in reason for reason in report["reasons"])
    assert any("WER improvement is not significant" in reason for reason in report["reasons"])


def test_zero_variance_fallback_is_exact_not_fabricated_p_zero() -> None:
    champion = scorecard(n=5, label="champion")
    challenger = scorecard(n=5, char_dist=19, word_dist=5, label="challenger")
    for metric in ("cer", "wer"):
        result = paired_significance(champion.rows, challenger.rows, list(range(5)), metric)
        assert result["method"] == "exact_two_sided_sign"
        assert result["z"] is None
        assert result["p_value"] == 0.0625, result


def test_regressed_protected_slice_blocks_an_overall_win_in_both_metrics() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(
        label="challenger",
        char_fn=lambda clip: 5 if clip < 25 else 50,
        word_fn=lambda clip: 1 if clip < 25 else 15,
    )
    groups = protected_groups()
    del groups["source:all"]
    del groups["dialect:all"]
    groups.update(
        {
            "source:majority": list(range(25)),
            "source:minority": list(range(25, 30)),
            "dialect:majority": list(range(25)),
            "dialect:minority": list(range(25, 30)),
        }
    )
    slices = slice_file(slices=groups)
    report = run_decision(champion, challenger, slices=slices)
    assert report["champion_cer"] > report["challenger_cer"]
    assert report["champion_wer"] > report["challenger_wer"]
    assert report["verdict"] == "REJECT"
    assert any("dialect:minority" in reason and "CER REGRESSED" in reason for reason in report["reasons"])
    assert any("dialect:minority" in reason and "WER REGRESSED" in reason for reason in report["reasons"])


def test_slice_wer_regression_alone_blocks_promotion() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(
        label="challenger",
        char_fn=lambda _clip: 10,
        word_fn=lambda clip: 1 if clip < 25 else 10,
    )
    groups = protected_groups()
    del groups["source:all"]
    groups.update({"source:a": list(range(25)), "source:b": list(range(25, 30))})
    slices = slice_file(slices=groups)
    report = run_decision(champion, challenger, slices=slices)
    assert report["verdict"] == "REJECT"
    assert any("source:b" in reason and "WER REGRESSED" in reason for reason in report["reasons"])


def test_zero_slices_is_invalid() -> None:
    report = run_decision(
        scorecard(label="champion"),
        scorecard(char_dist=10, word_dist=2, label="challenger"),
        slices=slice_file(slices={}),
    )
    assert report["verdict"] == "INVALID"
    assert any("zero protected slices" in reason for reason in report["reasons"])


def test_generic_all_clip_slice_cannot_impersonate_protected_dimensions() -> None:
    report = run_decision(
        scorecard(label="champion"),
        scorecard(char_dist=10, word_dist=2, label="challenger"),
        slices=slice_file(slices={"all": list(range(N))}),
    )
    assert report["verdict"] == "INVALID"
    assert any("mandatory dimensions" in reason for reason in report["reasons"])
    for dimension in ("source", "speaker", "dialect", "audio"):
        assert any(f"protected {dimension} slices" in reason for reason in report["reasons"])


def test_undersized_slice_is_invalid_even_when_another_slice_covers_everything() -> None:
    groups = protected_groups()
    groups["source:too-small"] = list(range(4))
    slices = slice_file(slices=groups)
    report = run_decision(
        scorecard(label="champion"), scorecard(char_dist=10, word_dist=2, label="challenger"), slices=slices
    )
    assert report["verdict"] == "INVALID"
    assert any("source:too-small" in reason and "at least 5" in reason for reason in report["reasons"])


def test_unprotected_manifest_rows_are_invalid() -> None:
    slices = slice_file(
        slices={
            "source:partial": list(range(25)),
            "speaker:partial:unassigned": list(range(25)),
            "dialect:partial": list(range(25)),
            "audio:partial": list(range(25)),
        }
    )
    report = run_decision(
        scorecard(label="champion"), scorecard(char_dist=10, word_dist=2, label="challenger"), slices=slices
    )
    assert report["verdict"] == "INVALID"
    assert any("unprotected" in reason for reason in report["reasons"])


def test_fewer_than_thirty_paired_rows_is_invalid() -> None:
    champion = scorecard(n=29, label="champion")
    challenger = scorecard(n=29, char_dist=10, word_dist=2, label="challenger")
    report = run_decision(champion, challenger, slices=slice_file(n=29), manifest_rows=29)
    assert report["verdict"] == "INVALID"
    assert any("at least 30" in reason for reason in report["reasons"])


def test_unpaired_or_incomplete_scorecard_is_invalid() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(n=29, char_dist=10, word_dist=2, label="challenger")
    challenger_provenance = provenance("challenger", challenger, eval_manifest_rows=N)
    report = run_decision(
        champion,
        challenger,
        challenger_provenance=challenger_provenance,
        manifest_rows=N,
    )
    assert report["verdict"] == "INVALID"
    assert any("unpaired evaluation" in reason for reason in report["reasons"])
    assert any("not the exact eval manifest" in reason for reason in report["reasons"])


def test_null_or_cross_basis_is_invalid() -> None:
    champion = scorecard(label="champion")
    null_card = scorecard(char_dist=10, word_dist=2, basis="", label="challenger-null")
    null_report = run_decision(champion, null_card)
    assert null_report["verdict"] == "INVALID"
    assert any("norm_basis" in reason for reason in null_report["reasons"])

    raw_card = scorecard(char_dist=10, word_dist=2, basis="raw", label="challenger-raw")
    raw_report = run_decision(champion, raw_card)
    assert raw_report["verdict"] == "INVALID"
    assert any("normalization basis differs" in reason for reason in raw_report["reasons"])


def test_reference_length_mismatch_is_invalid() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    changed = dict(challenger.rows)
    changed[0] = (10, 99, 2, 20)
    challenger = Scorecard(changed, BASIS, challenger.sha256)
    report = run_decision(champion, challenger)
    assert report["verdict"] == "INVALID"
    assert any("different reference lengths" in reason for reason in report["reasons"])


def test_missing_provenance_field_is_invalid() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    challenger_provenance = provenance("challenger", challenger)
    del challenger_provenance["tokenizer_sha256"]
    report = run_decision(champion, challenger, challenger_provenance=challenger_provenance)
    assert report["verdict"] == "INVALID"
    assert any("missing mandatory" in reason and "tokenizer_sha256" in reason for reason in report["reasons"])


def test_scorecard_tamper_breaks_provenance_binding() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger-after-tamper")
    stale = provenance("challenger", challenger, scorecard_sha256=digest("challenger-before-tamper"))
    report = run_decision(champion, challenger, challenger_provenance=stale)
    assert report["verdict"] == "INVALID"
    assert any("scorecard_sha256 does not match" in reason for reason in report["reasons"])


def test_cross_snapshot_is_invalid() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    challenger_provenance = provenance("challenger", challenger, snapshot_id="9" * 64)
    report = run_decision(champion, challenger, challenger_provenance=challenger_provenance)
    assert report["verdict"] == "INVALID"
    assert any("snapshot_id" in reason for reason in report["reasons"])


def test_challenger_run_artifact_and_deployment_linkage_is_exact_and_propagated() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    valid = run_decision(champion, challenger)
    assert valid["challenger_artifact_manifest_sha256"] == digest("challenger-artifact-manifest")
    assert valid["challenger_deployment_manifest_sha256"] == digest("challenger-deployment-manifest")
    assert valid["training_run_id"] == digest("challenger-training-run")
    assert valid["identities"]["challenger"]["adapter_config_sha256"] == digest(
        "adapter-config-candidate"
    )
    assert valid["identities"]["shared"]["normalizer_sha256"] == digest("shared-normalizer")

    mismatched = provenance(
        "challenger",
        challenger,
        challenger_artifact_manifest_sha256=digest("different-artifact-manifest"),
        challenger_deployment_manifest_sha256=digest("different-deployment-manifest"),
        training_run_id=digest("different-training-run"),
    )
    invalid = run_decision(champion, challenger, challenger_provenance=mismatched)
    assert invalid["verdict"] == "INVALID"
    for field in (
        "challenger_artifact_manifest_sha256",
        "challenger_deployment_manifest_sha256",
        "training_run_id",
    ):
        assert any(field in reason and "shared identity" in reason for reason in invalid["reasons"])


def test_base_tokenizer_and_normalizer_must_match_exactly() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    challenger_provenance = provenance(
        "challenger",
        challenger,
        base_model_sha256=digest("other-base"),
        tokenizer_id="other-tokenizer",
        normalizer_sha256=digest("other-normalizer"),
    )
    report = run_decision(champion, challenger, challenger_provenance=challenger_provenance)
    assert report["verdict"] == "INVALID"
    for field in ("base_model_sha256", "tokenizer_id", "normalizer_sha256"):
        assert any(field in reason and "shared identity" in reason for reason in report["reasons"])


def test_owner_7b_family_lock_and_distinct_candidate_are_mandatory() -> None:
    champion = scorecard(label="champion")
    challenger = scorecard(char_dist=10, word_dist=2, label="challenger")
    family = provenance("challenger", challenger, model_family="other-asr")
    report = run_decision(champion, challenger, challenger_provenance=family)
    assert report["verdict"] == "INVALID"
    assert any("owner 7B family lock" in reason for reason in report["reasons"])

    same = provenance("challenger", challenger)
    incumbent = provenance("champion", champion)
    for field in (
        "model_id",
        "model_sha256",
        "adapter_id",
        "adapter_sha256",
        "adapter_config_id",
        "adapter_config_sha256",
    ):
        same[field] = incumbent[field]
    report = run_decision(
        champion,
        challenger,
        champion_provenance=incumbent,
        challenger_provenance=same,
    )
    assert report["verdict"] == "INVALID"
    distinct_fields = {
        "model_id",
        "model_sha256",
        "adapter_id",
        "adapter_sha256",
        "adapter_config_id",
        "adapter_config_sha256",
    }
    assert all(
        any(field in reason and "identical" in reason for reason in report["reasons"])
        for field in same
        if field in distinct_fields
    )


def test_scorecard_loader_rejects_duplicate_short_and_invalid_rows() -> None:
    header = "clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\tnorm_basis\n"
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "score.tsv"
        path.write_text(header + "0\t1\t10\t1\t2\tx\n0\t2\t10\t1\t2\tx\n", encoding="utf-8")
        assert "repeats clip_index" in expect_input_error(lambda: load_scorecard(path))

        path.write_text(header + "0\t1\t10\t1\t2\n", encoding="utf-8")
        assert "field(s)" in expect_input_error(lambda: load_scorecard(path))

        path.write_text(header + "0\t-1\t10\t1\t2\tx\n", encoding="utf-8")
        assert "negative edit distance" in expect_input_error(lambda: load_scorecard(path))

        path.write_text(header + "0\t1\t0\t1\t2\tx\n", encoding="utf-8")
        assert "non-positive reference" in expect_input_error(lambda: load_scorecard(path))

        path.write_text(header + f"0\t{2**63}\t10\t1\t2\tx\n", encoding="utf-8")
        assert "signed 64-bit" in expect_input_error(lambda: load_scorecard(path))

        path.write_text(header + "0\t1\t10\t1\t2\t\n", encoding="utf-8")
        assert "norm_basis" in expect_input_error(lambda: load_scorecard(path))


def test_scorecard_loader_rejects_mixed_basis_and_slice_loader_rejects_duplicates() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        score = root / "score.tsv"
        score.write_text(
            "clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\tnorm_basis\n"
            "0\t1\t10\t1\t2\ta\n1\t1\t10\t1\t2\tb\n",
            encoding="utf-8",
        )
        assert "mixes norm_basis" in expect_input_error(lambda: load_scorecard(score))

        slices = root / "slices.tsv"
        slices.write_text(
            "# schema_version=1\n# eval_manifest_sha256=" + MANIFEST_SHA + "\n"
            "# eval_manifest_rows=30\n# min_clips=5\n# library_index_sha256=" + INDEX_SHA + "\n"
            "slice_name\tclip_index\na\t0\na\t0\n",
            encoding="utf-8",
        )
        assert "repeats" in expect_input_error(lambda: load_slices(slices))


def test_manifest_and_provenance_parsers_reject_ambiguous_duplicate_evidence() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        manifest = root / "manifest.tsv"
        manifest.write_text("a.wav\ttruth\na.wav\ttruth again\n", encoding="utf-8")
        assert "repeats audio path" in expect_input_error(lambda: inspect_eval_manifest(manifest))
        manifest.write_text("a.wav\t\n", encoding="utf-8")
        assert "empty reference" in expect_input_error(lambda: inspect_eval_manifest(manifest))

        prov = root / "p.json"
        prov.write_text('{"role":"champion","role":"challenger"}', encoding="utf-8")
        assert "duplicate key" in expect_input_error(lambda: load_provenance(prov))


def _write_scorecard(path: Path, *, char_dist: int, word_dist: int) -> Scorecard:
    lines = ["clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\tnorm_basis"]
    lines += [f"{clip}\t{char_dist}\t100\t{word_dist}\t20\t{BASIS}" for clip in range(N)]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return load_scorecard(path)


def _write_challenger_run(root: Path) -> tuple[ChallengerRunBinding, Path]:
    def write_component(name: str, payload: bytes) -> dict:
        path = root / name
        path.write_bytes(payload)
        return {"path": name, "sha256": hashlib.sha256(payload).hexdigest(), "size_bytes": len(payload)}

    base_path = root / "omniasr-7b-base.bin"
    base_path.write_bytes(b"immutable-owner-7b-base")
    base_hash = hashlib.sha256(base_path.read_bytes()).hexdigest()
    adapter = write_component("adapter.safetensors", b"candidate-adapter")
    tokenizer = write_component("tokenizer.json", b"shared-tokenizer")
    adapter_config = write_component("adapter_config.json", b"candidate-adapter-config")
    artifacts = [
        {"role": "adapter", **adapter},
        {"role": "tokenizer", **tokenizer},
        {"role": "adapter_config", **adapter_config},
    ]
    artifact_path = root / "artifact_manifest.json"
    artifact_path.write_text(json.dumps({"schema": 1, "artifacts": artifacts}), encoding="utf-8")
    artifact_hash = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
    source_evidence = {
        "training_contract_sha256": digest("training-contract"),
        "trainer_evidence_sha256": digest("trainer-evidence"),
        "artifact_manifest_sha256": artifact_hash,
    }
    training_run_id = hashlib.sha256(
        json.dumps(source_evidence, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    model_id = f"omniasr-7b-challenger-{training_run_id[:24]}"
    deployment = {
        "schema": 1,
        "model_id": model_id,
        "family": "omniasr-7b",
        "model_card": "soranivoice_omniASR_LLM_7B_v2_local",
        "language": "ckb_Arab",
        "runtime": {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288},
        "base_checkpoint": {
            "id": "omniasr-7b-owner-base-v1",
            "path": str(base_path.resolve()),
            "sha256": base_hash,
            "size_bytes": base_path.stat().st_size,
        },
        "adapter": adapter,
        "tokenizer": tokenizer,
        "adapter_config": adapter_config,
        "server_compatibility": {"protocol": "cortex-omniasr-adapter", "version": 1},
        "provenance": {
            "snapshot": {"id": SNAPSHOT, "root_sha256": digest("snapshot-root")},
            "training_run_id": training_run_id,
            "source_evidence": source_evidence,
        },
    }
    deployment["manifestSha256"] = hashlib.sha256(
        json.dumps(deployment, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    ).hexdigest()
    deployment_path = root / "deployment_manifest.json"
    deployment_path.write_text(
        json.dumps(deployment, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    deployment_file_hash = hashlib.sha256(deployment_path.read_bytes()).hexdigest()
    run_record = {
        "schema": 2,
        "status": "trained",
        "snapshot_id": SNAPSHOT,
        "training_run_id": training_run_id,
        "model_id": model_id,
        "base_id": "omniasr-7b-owner-base-v1",
        "base_sha256": base_hash,
        "artifact_manifest": {"path": artifact_path.name, "sha256": artifact_hash},
        "artifact_manifest_sha256": artifact_hash,
        "deployment_manifest": {
            "path": deployment_path.name,
            "sha256": deployment_file_hash,
            "manifestSha256": deployment["manifestSha256"],
        },
    }
    run_path = root / "challenger_run.json"
    run_path.write_text(json.dumps(run_record), encoding="utf-8")
    return load_challenger_run(run_path), run_path


def test_run_loader_rehashes_deployment_and_rejects_tamper_or_missing_component() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        binding, run_path = _write_challenger_run(root)
        assert binding.model_sha256 and binding.deployment_file_sha256
        deployment_path = root / "deployment_manifest.json"
        with deployment_path.open("a", encoding="utf-8") as handle:
            handle.write(" ")
        assert "deployment_manifest bytes" in expect_input_error(lambda: load_challenger_run(run_path))

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        _, run_path = _write_challenger_run(root)
        (root / "adapter.safetensors").unlink()
        assert "adapter file is missing" in expect_input_error(lambda: load_challenger_run(run_path))


def test_cli_promotes_bound_evidence_then_detects_scorecard_tamper() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        manifest_path = root / "gold.tsv"
        manifest_path.write_text("".join(f"clip-{i}.wav\ttruth {i}\n" for i in range(N)), encoding="utf-8")
        manifest = inspect_eval_manifest(manifest_path)
        champion_path, challenger_path = root / "champion.tsv", root / "challenger.tsv"
        champion = _write_scorecard(champion_path, char_dist=20, word_dist=6)
        challenger = _write_scorecard(challenger_path, char_dist=10, word_dist=2)
        binding, run_path = _write_challenger_run(root)
        common = {
            "base_model_id": binding.base_model_id,
            "base_model_sha256": binding.base_model_sha256,
            "tokenizer_id": binding.tokenizer_id,
            "tokenizer_sha256": binding.tokenizer_sha256,
            "challenger_artifact_manifest_sha256": binding.artifact_manifest_sha256,
            "challenger_deployment_manifest_sha256": binding.deployment_file_sha256,
            "training_run_id": binding.training_run_id,
        }
        champion_provenance = provenance(
            "champion", champion, eval_manifest_sha256=manifest.sha256, eval_manifest_rows=N, **common
        )
        challenger_provenance = provenance(
            "challenger",
            challenger,
            eval_manifest_sha256=manifest.sha256,
            eval_manifest_rows=N,
            model_id=binding.model_id,
            model_sha256=binding.model_sha256,
            adapter_id=binding.adapter_id,
            adapter_sha256=binding.adapter_sha256,
            adapter_config_id=binding.adapter_config_id,
            adapter_config_sha256=binding.adapter_config_sha256,
            **common,
        )
        Path(f"{champion_path}.provenance.json").write_text(
            json.dumps(champion_provenance), encoding="utf-8"
        )
        Path(f"{challenger_path}.provenance.json").write_text(
            json.dumps(challenger_provenance), encoding="utf-8"
        )
        slices_path = root / "slices.tsv"
        slices_path.write_text(
            f"# schema_version=1\n# eval_manifest_sha256={manifest.sha256}\n"
            f"# eval_manifest_rows={N}\n# min_clips=5\n# library_index_sha256={INDEX_SHA}\n"
            "slice_name\tclip_index\n"
            + "".join(
                f"{name}\t{i}\n" for name in protected_groups() for i in range(N)
            ),
            encoding="utf-8",
        )
        verdict_path = root / "verdict.json"
        args = [
            str(champion_path),
            str(challenger_path),
            "--snapshot-id",
            SNAPSHOT,
            "--eval-manifest",
            str(manifest_path),
            "--slices",
            str(slices_path),
            "--challenger-run-record",
            str(run_path),
            "--out",
            str(verdict_path),
        ]
        assert gate_main(args) == 0
        assert json.loads(verdict_path.read_text(encoding="utf-8"))["verdict"] == "PROMOTE"

        with challenger_path.open("a", encoding="utf-8") as handle:
            handle.write("\n")
        assert gate_main(args) == 2
        invalid = json.loads(verdict_path.read_text(encoding="utf-8"))
        assert invalid["verdict"] == "INVALID"
        assert invalid["reasons"], "tamper must produce a written, non-vacuous invalid verdict"


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    failures = 0
    for test in tests:
        try:
            test()
        except Exception as error:  # noqa: BLE001 - this runner must report every gate arm
            failures += 1
            print(f"FAIL {test.__name__}: {error}", flush=True)
        else:
            print(f"  ok {test.__name__}", flush=True)
    if failures:
        print(f"PROMOTION GATE: {failures}/{len(tests)} test(s) FAILED", flush=True)
        return 1
    print(f"PROMOTION GATE: {len(tests)} fail-closed arms pinned", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
