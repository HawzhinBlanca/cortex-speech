//! Shared `#[cfg(test)]` command harness: a real [`AppState`] managed on a `tauri` MockRuntime app,
//! so unit tests can call `#[tauri::command]` fns through a genuine `State<'_, AppState>` handle
//! (`let state = app.state::<AppState>();`). Backed by the dev-only `tauri/test` feature, which
//! resolver v2 keeps out of the release build.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::settings::AppSettings;
use crate::AppState;

/// The smallest real `AppState` a command test needs: a file-backed database in a throwaway
/// `data_dir`, default settings, and a live pipeline — the same recipe as `lib.rs`'s private
/// `test_app_state`, exposed crate-wide.
pub(crate) fn app_state(data_dir: PathBuf) -> AppState {
    let normalizer = Arc::new(crate::normalizer::SoraniNormalizer::new());
    let cache = Arc::new(crate::cache::TranscriptCache::new(10));
    let fingerprint = Arc::new(crate::fingerprint::AudioFingerprint::new());
    let settings = AppSettings::default();
    let model_manager = crate::models::ModelManager::new(data_dir.join("models"));
    let pipeline = crate::pipeline::ProcessingPipeline::new(
        ":memory:".to_string(),
        Arc::clone(&normalizer),
        Arc::clone(&cache),
        Arc::clone(&fingerprint),
        Arc::new(settings.clone()),
        Arc::new(crate::models::ModelManager::new(data_dir.join("models"))),
    );
    // A SQLite `:memory:` database is private to one connection and DatabaseRuntime::open_read
    // opens its own; a disposable file keeps the multi-connection behavior of production.
    let db = crate::db::Database::open(data_dir.join("app-state.db").to_string_lossy().as_ref()).unwrap();
    db.initialize().unwrap();
    AppState {
        db: crate::database_runtime::DatabaseRuntime::new(db),
        pipeline: Mutex::new(pipeline),
        normalizer,
        cache,
        fingerprint,
        dedup_readiness: crate::DedupReadiness::Ready { rehydrated_recordings: 0 },
        history: Arc::new(Mutex::new(crate::history::HistoryManager::new(10))),
        session: Mutex::new(crate::session::SessionManager::new(data_dir.join("session"))),
        settings: Mutex::new(settings),
        settings_write: Mutex::new(()),
        data_dir: Mutex::new(Some(data_dir)),
        model_manager: Mutex::new(model_manager),
        file_picker_cancel_token: Mutex::new(None),
        import_cancel_token: Mutex::new(None),
        batch_cancel_token: Mutex::new(None),
        import_state: Mutex::new(crate::ImportState::Idle),
        import_run_tracker: Mutex::new(crate::ImportRunTracker::default()),
        batch_state: Mutex::new(crate::BatchState::Idle),
        batch_run_tracker: Mutex::new(crate::BatchRunTracker::default()),
        media_registry: Arc::new(Mutex::new(crate::media::MediaRegistry::default())),
        media_materializer: Arc::new(crate::media::MediaMaterializationCoordinator::default()),
    }
}

/// A mock tauri app with a real [`AppState`] managed on it. Tests hold the returned `App` alive and
/// take `app.state::<AppState>()` — exactly the `State<'_, AppState>` every `#[tauri::command]`
/// receives in production.
pub(crate) fn managed_app_state(data_dir: &Path) -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_app();
    app.manage(app_state(data_dir.to_path_buf()));
    app
}
