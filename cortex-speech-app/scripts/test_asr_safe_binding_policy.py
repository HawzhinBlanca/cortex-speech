#!/usr/bin/env python3
"""Keep native sherpa-onnx decoding inside the supervised worker process."""

from pathlib import Path


def test_asr_uses_supported_result_api() -> None:
    source = (Path(__file__).parents[1] / "src-tauri" / "src" / "asr.rs").read_text(encoding="utf-8")
    start = source.index("fn transcribe_chunk(")
    end = source.index("/// Thread-safe lazy singleton", start)
    body = source[start:end]
    assert "stream.get_result()" in body, "ASR result retrieval must use sherpa-onnx's supported API"
    assert "std::mem::transmute" not in body, "private OfflineStream layout access reintroduces UB"
    assert "SherpaOnnxGetOfflineStreamResultAsJson" not in body, "do not bypass the safe Rust binding"


def test_host_service_owns_only_the_worker_supervisor() -> None:
    source = (Path(__file__).parents[1] / "src-tauri" / "src" / "asr.rs").read_text(encoding="utf-8")
    host_start = source.index("pub struct KurdishAsrService")
    host_end = source.index("fn run_supervisor_self_test", host_start)
    host = source[host_start:host_end]
    assert "worker: Option<AsrWorkerSupervisor>" in host
    assert "OfflineRecognizer" not in host, "the Tauri host must never own a native recognizer"
    assert ".decode(" not in host, "the Tauri host must never execute native inference"

    native_start = source.index("struct NativeKurdishAsrEngine")
    native_end = source.index("#[derive(Debug, serde::Serialize, serde::Deserialize)]", native_start)
    worker_entry = source.index("fn run_worker(")
    assert native_start < worker_entry, "native engine must be reached through the worker entrypoint"
    assert "WorkerEngine::Native(engine) => engine.transcribe" in source[worker_entry:host_start]


if __name__ == "__main__":
    test_asr_uses_supported_result_api()
    test_host_service_owns_only_the_worker_supervisor()
    print("PASS: ASR subprocess-containment policy")
