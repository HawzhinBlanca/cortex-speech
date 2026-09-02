//! Export IPC commands — the first slice extracted from `commands.rs` (Week-4 decomposition).
//!
//! Behaviour and command NAMES are unchanged: `commands.rs` re-exports this module with
//! `pub use export::*;`, so `lib.rs`'s `invoke_handler` still names `commands::export_dataset` and the
//! frontend's `invoke('export_dataset')` is untouched. Nothing here is a rewrite — these are the same
//! functions that lived in `commands.rs`, only relocated.
//!
//! They are all `async` + `run_blocking`: a full-library export (DB scan + serialize + hash + atomic
//! write) would otherwise freeze the UI thread. The DB guard is taken INSIDE the blocking task, never
//! held across an await.

use super::{run_blocking, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::AppState;
use std::path::Path;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn export_dataset(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_dataset")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_dataset"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let fmt = match format.to_lowercase().as_str() {
        "csv" => crate::settings::ExportFormat::Csv,
        "jsonl" => crate::settings::ExportFormat::Jsonl,
        "parquet" => crate::settings::ExportFormat::Parquet,
        _ => crate::settings::ExportFormat::Json,
    };
    // Off the main thread: a full-library export (DB scan + serialize + hash + atomic write) would
    // otherwise freeze the UI. The DB guard is taken INSIDE the blocking task, never across an await.
    // Bracketed as a durable job so a crash mid-export is reaped as INTERRUPTED at the next startup and
    // the outcome shows up in get_jobs — the op's real work is unchanged.
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_dataset(&job_id, Path::new(&validated_path), &fmt).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner dataset export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Dataset, &error)
    })
}

/// Export a plain, human-facing transcript / subtitle file (txt | srt | vtt) from the library —
/// distinct from the ML dataset export. Path is validated the same way; unknown formats fall to txt.
#[tauri::command]
#[specta::specta]
pub async fn export_transcript(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_transcript")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_transcript"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let fmt = crate::transcript_export::TranscriptFormat::from_str_lossy(&format);
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_transcript(&job_id, Path::new(&validated_path), fmt).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner transcript export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Transcript, &error)
    })
}

#[tauri::command]
pub async fn export_dataset_bundle(
    path: String,
    production: bool,
    warning_threshold: Option<usize>,
    state: State<'_, AppState>,
) -> Result<crate::export_bundle::BundleExportResult, String> {
    STRICT_RATE_LIMITER.check("export_dataset_bundle")?;
    let validated_path = validate::validate_output_path(&path)?;
    let warning_threshold = warning_threshold.unwrap_or(0);
    let settings = state.lock_settings().clone();
    let model_manager = state.lock_model_manager().clone(); // ModelManager is Clone (a PathBuf)
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    run_blocking(move || {
        store
            .export_dataset_bundle(
                &job_id,
                &model_manager,
                Path::new(&validated_path),
                &settings,
                production,
                warning_threshold,
            )
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn export_huggingface_dataset(
    path: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_huggingface_dataset")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_huggingface_dataset"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let settings = state.lock_settings().clone();
    // Bracketed as a durable job (same as export_dataset): a crash mid-export is reaped as INTERRUPTED
    // at the next startup and the outcome shows in get_jobs / the activity pill. Work is unchanged.
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_huggingface_dataset(&job_id, Path::new(&validated_path), &settings).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner Hugging Face export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::HuggingFace, &error)
    })
}

#[tauri::command]
#[specta::specta]
pub async fn export_audio(
    segment_ids: Vec<String>,
    options: crate::ipc_contract::AudioExportOptionsV1,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::AudioExportResultV1, crate::ipc_contract::CommandErrorV1> {
    // Decodes + re-encodes one clip per segment to disk — throttle it like every sibling export
    // command (round-22 #5: it was the lone export missing a rate-limiter, a local DoS/disk-fill gap).
    STRICT_RATE_LIMITER
        .check("export_audio")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_audio"))?;
    for id in &segment_ids {
        validate::validate_identifier(id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    }
    let mut options: crate::export_audio::AudioExportOptions = options.into();
    options.output_dir = validate::validate_output_path(&options.output_dir)
        .map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result =
        run_blocking(move || store.export_audio(&job_id, &segment_ids, &options).map_err(|e| e.to_string())).await;
    result.map(crate::ipc_contract::AudioExportResultV1::from).map_err(|error| {
        tracing::warn!("Owner reviewed-audio export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Audio, &error)
    })
}

/// M2.7 / P1.6: export the gold set as a portable eval set (manifest.jsonl + 16 kHz WAV clips) under
/// `out_dir`. Returns the export summary (counts + manifest path).
#[tauri::command]
#[specta::specta]
pub async fn export_gold_eval_set(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::GoldEvalExport, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_gold_eval_set")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_gold_eval_set"))?;
    // Same trust-boundary guard every sibling export (incl. the directory export export_audio) enforces:
    // reject null bytes + Windows UNC syntactically, so a compromised renderer can't hand a
    // `\\attacker\share` out_dir straight to create_dir_all — which would drive the SMB redirector (a
    // forced-auth NTLM-credential leak) and write the gold clips off-machine. A null-byte-only check
    // (what this had) left that open. The dir comes from an OS folder picker in the real flow, so it
    // always exists → validate_output_path's parent-must-exist requirement never rejects a legit run.
    let validated =
        validate::validate_output_path(&out_dir).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result =
        run_blocking(move || store.export_gold_eval_set(&job_id, Path::new(&validated)).map_err(|e| e.to_string()))
            .await;
    result.map_err(|error| {
        tracing::warn!("Owner gold evaluation export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::GoldEval, &error)
    })
}

/// M5.1 / P5.1: export a fine-tune training pack (trainer manifest + 16 kHz clips) from human-verified
/// segments under `out_dir`, EXCLUDING holdout gold (the leak guard). Returns the pack summary.
#[tauri::command]
#[specta::specta]
pub async fn export_finetune_pack(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::FinetunePackResult, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_finetune_pack")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_finetune_pack"))?;
    // Same UNC/null trust-boundary guard as export_gold_eval_set above (and every sibling export): the
    // pack writes 16 kHz clips under out_dir via create_dir_all, so a webview-supplied `\\attacker\share`
    // would leak NTLM creds + write off-machine. Null-byte-only was insufficient.
    let validated =
        validate::validate_output_path(&out_dir).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    // P5.5: every pack export appends its provenance line to the durable corpus ledger.
    let ledger = state.lock_data_dir().clone().map(|d| d.join("corpus_ledger.jsonl"));
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_finetune_pack(&job_id, Path::new(&validated), ledger.as_deref()).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner fine-tune export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Finetune, &error)
    })
}

/// Wave-4 state-boundary coverage: every `#[tauri::command]` above invoked through a genuine managed
/// `State<'_, AppState>` (`crate::test_support::managed_app_state`), so the limiter check, the path
/// validation, the format mapping and the job-store closure all run exactly as production IPC runs
/// them. The export engines themselves are covered in `export_tests.rs`; these tests own the wrappers.
#[cfg(test)]
mod state_command_surface_tests {
    use super::*;
    use crate::test_support::managed_app_state;
    use tauri::Manager;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().expect("build test runtime").block_on(future)
    }

    /// A destination whose PARENT does not exist — the input `validate_output_path` rejects with
    /// zero I/O against the network or the filesystem's UNC machinery.
    fn unusable_destination(root: &std::path::Path, name: &str) -> String {
        root.join("no-such-dir").join(name).to_string_lossy().into_owned()
    }

    fn expect_refusal(error: &crate::ipc_contract::CommandErrorV1, code: &str) {
        assert_eq!(error.code, code);
        assert!(!error.retryable, "{code} is a caller-repairable input, never a retry");
        assert_eq!(error.suggested_action, None, "{code} has no in-app affordance to point at");
    }

    /// `format` is matched lowercased with a `_ => Json` fallback, so the four named tables and the
    /// fallback must each land as their real on-disk encoding — not merely "a file appeared".
    #[test]
    fn export_dataset_writes_each_named_table_and_falls_back_to_json() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let json = tmp.path().join("dataset.json");
        // The refusal goes FIRST. `export_dataset` sits behind STRICT_RATE_LIMITER, a process-global
        // token bucket keyed by command name with burst 5 and refill 10/s. This test issues six
        // command-level calls; with the refusal last, the sixth needed a refilled token and whether
        // one had accrued depended on how fast the five empty-library exports ran -- measured 2026-09-02
        // on a 4-core runner: `RATE_LIMITED` where `INVALID_OUTPUT_PATH` was asserted. Refusing first
        // spends token 1 on a full bucket, and the five format exports then consume exactly the burst,
        // so nothing here depends on elapsed time.
        let refused =
            block_on(export_dataset(unusable_destination(tmp.path(), "out.json"), "json".into(), app.state()))
                .expect_err("a destination whose parent does not exist is refused");
        expect_refusal(&refused, "INVALID_OUTPUT_PATH");
        block_on(export_dataset(json.to_string_lossy().into_owned(), "json".into(), app.state()))
            .expect("json export of an empty library");
        let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).expect("valid JSON");
        assert!(parsed.get("metadata").is_some(), "the JSON table carries dataset metadata");
        assert_eq!(parsed["segments"].as_array().map(Vec::len), Some(0), "an empty library exports zero rows");

        let jsonl = tmp.path().join("dataset.jsonl");
        block_on(export_dataset(jsonl.to_string_lossy().into_owned(), "jsonl".into(), app.state()))
            .expect("jsonl export");
        assert_eq!(std::fs::read(&jsonl).unwrap(), Vec::<u8>::new(), "one row per line means no rows means no bytes");

        let csv = tmp.path().join("dataset.csv");
        block_on(export_dataset(csv.to_string_lossy().into_owned(), "csv".into(), app.state())).expect("csv export");
        let csv_text = std::fs::read_to_string(&csv).unwrap();
        assert!(
            csv_text.starts_with("id,audio_path,raw_transcript,"),
            "the CSV table must still publish its header row: {csv_text:?}"
        );

        let parquet = tmp.path().join("dataset.parquet");
        block_on(export_dataset(parquet.to_string_lossy().into_owned(), "parquet".into(), app.state()))
            .expect("parquet export");
        let parquet_bytes = std::fs::read(&parquet).unwrap();
        assert_eq!(&parquet_bytes[..4], b"PAR1", "a real Parquet file opens with the PAR1 magic");
        assert_eq!(&parquet_bytes[parquet_bytes.len() - 4..], b"PAR1", "and closes with it");

        // Case-insensitive match plus the `_ => Json` arm: an unknown format is not an error, it is JSON.
        let fallback = tmp.path().join("fallback.bin");
        block_on(export_dataset(fallback.to_string_lossy().into_owned(), "SpreadSheet".into(), app.state()))
            .expect("an unknown format falls back rather than failing");
        let fallback_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fallback).unwrap()).expect("the fallback wrote JSON");
        assert!(fallback_value.get("segments").is_some(), "the fallback is the JSON table, not an empty file");
    }

    /// `TranscriptFormat::from_str_lossy` is the human-facing sibling mapping, with txt as the
    /// fallback. VTT is the only one of the three with a mandatory header, which is what separates
    /// "the format was honoured" from "some file was written".
    #[test]
    fn export_transcript_writes_each_subtitle_format_and_falls_back_to_txt() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let vtt = tmp.path().join("transcript.vtt");
        block_on(export_transcript(vtt.to_string_lossy().into_owned(), "vtt".into(), app.state())).expect("vtt export");
        assert_eq!(std::fs::read_to_string(&vtt).unwrap(), "WEBVTT\n\n", "WebVTT is invalid without its header");

        let srt = tmp.path().join("transcript.srt");
        block_on(export_transcript(srt.to_string_lossy().into_owned(), "srt".into(), app.state())).expect("srt export");
        assert_eq!(std::fs::read_to_string(&srt).unwrap(), "", "SRT is cue-only: no cues, no bytes");

        let txt = tmp.path().join("transcript.txt");
        block_on(export_transcript(txt.to_string_lossy().into_owned(), "txt".into(), app.state())).expect("txt export");
        assert_eq!(std::fs::read_to_string(&txt).unwrap(), "");

        let fallback = tmp.path().join("fallback.transcript");
        block_on(export_transcript(fallback.to_string_lossy().into_owned(), "nonsense".into(), app.state()))
            .expect("an unknown transcript format falls back rather than failing");
        assert_eq!(
            std::fs::read_to_string(&fallback).unwrap(),
            "",
            "the fallback is TXT — it must not acquire the WEBVTT header"
        );

        let refused =
            block_on(export_transcript(unusable_destination(tmp.path(), "out.txt"), "txt".into(), app.state()))
                .expect_err("a destination whose parent does not exist is refused");
        expect_refusal(&refused, "INVALID_OUTPUT_PATH");
    }

    /// The three directory-shaped exports publish real, named artifacts. Their empty-library totals
    /// are the honest zero, not an absent file.
    #[test]
    fn directory_exports_publish_their_real_artifacts_with_honest_zero_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        // With ZERO exportable rows the HF export is a documented NO-OP that PRESERVES the previous
        // dataset rather than wiping it (the aligner-missing configuration grades every clip REVIEW,
        // so this is a real, reachable state — it once destroyed a good export and returned Ok).
        let hf = tmp.path().join("hf");
        let previous = hf.join("data").join("train").join("metadata.csv");
        std::fs::create_dir_all(previous.parent().unwrap()).unwrap();
        std::fs::write(&previous, b"file_name,transcription\nprevious-good-export.wav,text\n").unwrap();
        block_on(export_huggingface_dataset(hf.to_string_lossy().into_owned(), app.state()))
            .expect("an empty library is a successful no-op, not an error");
        assert_eq!(
            std::fs::read_to_string(&previous).unwrap(),
            "file_name,transcription\nprevious-good-export.wav,text\n",
            "a zero-row export must never replace a previously good dataset with an empty one"
        );
        assert!(!hf.join(".data-staging").exists(), "the no-op returns before it stages anything");

        let bundle_dir = tmp.path().join("bundle");
        let bundle =
            block_on(export_dataset_bundle(bundle_dir.to_string_lossy().into_owned(), false, None, app.state()))
                .expect("bundle export");
        assert!(!bundle.production, "the command's `production` flag reaches the bundle unchanged");
        assert!(!bundle.validation.blocked);
        assert_eq!(bundle.validation.error_count, 0);
        for required in ["manifest.json", "SHA256SUMS", "dataset.parquet", "dataset_card.md"] {
            assert!(bundle.files.iter().any(|f| f == required), "bundle is missing {required}: {:?}", bundle.files);
        }
        assert!(std::path::Path::new(&bundle.manifest_path).is_file(), "the reported manifest path must exist");

        let gold_dir = tmp.path().join("gold");
        let gold = block_on(export_gold_eval_set(gold_dir.to_string_lossy().into_owned(), app.state()))
            .expect("gold eval export");
        assert_eq!((gold.total_gold, gold.exported, gold.skipped), (0, 0, 0));
        assert!(std::path::Path::new(&gold.manifest_path).is_file());

        let pack_dir = tmp.path().join("pack");
        let pack = block_on(export_finetune_pack(pack_dir.to_string_lossy().into_owned(), app.state()))
            .expect("fine-tune pack export");
        assert_eq!((pack.total_verified, pack.emitted, pack.skipped), (0, 0, 0));
        assert!(pack.newly_sealed);
        // The SHA-256 of no bytes: the manifest is genuinely empty, and the seal is over it.
        assert_eq!(pack.manifest_sha256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(pack.snapshot_id, pack.manifest_sha256);
        assert_eq!(std::fs::metadata(&pack.manifest_path).unwrap().len(), 0);
    }

    /// Every export shares one destination guard; a per-command drift is a real hole, so each is
    /// asserted separately. The bundle is the one that answers on the plain `String` channel.
    #[test]
    fn every_export_refuses_a_destination_whose_parent_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let hf = block_on(export_huggingface_dataset(unusable_destination(tmp.path(), "hf"), app.state()))
            .expect_err("hugging face export must validate its destination");
        expect_refusal(&hf, "INVALID_OUTPUT_PATH");

        let gold = block_on(export_gold_eval_set(unusable_destination(tmp.path(), "gold"), app.state()))
            .expect_err("gold export writes clips under out_dir, so it must validate it");
        expect_refusal(&gold, "INVALID_OUTPUT_PATH");

        let pack = block_on(export_finetune_pack(unusable_destination(tmp.path(), "pack"), app.state()))
            .expect_err("fine-tune pack writes clips under out_dir, so it must validate it");
        expect_refusal(&pack, "INVALID_OUTPUT_PATH");

        let bundle =
            block_on(export_dataset_bundle(unusable_destination(tmp.path(), "bundle"), true, Some(3), app.state()))
                .expect_err("the bundle validates its destination on the String channel");
        assert!(bundle.contains("Invalid output directory"), "{bundle}");
    }

    /// `export_audio` decodes one clip per id into `options.output_dir`, so it validates BOTH the ids
    /// and the directory before the store runs.
    #[test]
    fn export_audio_validates_every_id_and_its_output_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let audio_dir = tmp.path().join("audio-out");
        std::fs::create_dir_all(&audio_dir).unwrap();
        let options = |dir: &std::path::Path| crate::ipc_contract::AudioExportOptionsV1 {
            output_dir: dir.to_string_lossy().into_owned(),
            format: crate::ipc_contract::AudioExportFormatV1::Wav,
            sample_rate: 16_000,
            include_metadata: false,
        };

        let bad_id = block_on(export_audio(vec!["seg-ok".into(), "../evil".into()], options(&audio_dir), app.state()))
            .expect_err("one bad id in the batch refuses the whole request");
        expect_refusal(&bad_id, "INVALID_SEGMENT_ID");

        // Two levels deep: `validate_output_path` canonicalizes the PARENT, so a single missing leaf
        // under an existing directory is a legitimate destination — it is the missing PARENT that fails.
        let bad_dir =
            block_on(export_audio(vec![], options(&tmp.path().join("no-such-dir").join("clips")), app.state()))
                .expect_err("the output directory is validated even with no ids");
        expect_refusal(&bad_dir, "INVALID_OUTPUT_PATH");

        let empty = block_on(export_audio(vec![], options(&audio_dir), app.state()))
            .expect("an empty request is a successful no-op, not an error");
        assert_eq!((empty.total, empty.succeeded, empty.failed), (0, 0, 0));
        assert!(empty.files.is_empty());
        assert!(empty.errors.is_empty());
        // The reported directory is the VALIDATED (canonicalized) path, not the caller's raw string.
        assert!(empty.output_dir.ends_with("audio-out"), "{}", empty.output_dir);
    }
}
