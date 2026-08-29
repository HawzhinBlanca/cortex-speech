//! Database connection ownership and restore admission.
//!
//! `Database` remains the schema/domain compatibility facade while this runtime owns process-level
//! serialization and bounded auxiliary readers. Commands receive runtime capabilities rather than
//! constructing independent live connections themselves.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, LockResult, Mutex, MutexGuard};
use std::time::Duration;

const DEFAULT_READ_CONNECTIONS: usize = 4;
const DEFAULT_READ_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct DatabaseRuntime {
    writer: Arc<Mutex<Database>>,
    database_path: Arc<str>,
    reads: Arc<ReadConnectionPool>,
    admission: Arc<RestoreAdmission>,
}

impl DatabaseRuntime {
    pub(crate) fn new(database: Database) -> Self {
        Self::with_admission(database, DEFAULT_READ_CONNECTIONS, DEFAULT_READ_WAIT, Arc::clone(&RESTORE_ADMISSION))
    }

    fn with_admission(
        database: Database,
        max_reads: usize,
        read_wait: Duration,
        admission: Arc<RestoreAdmission>,
    ) -> Self {
        let database_path: Arc<str> = Arc::from(database.path());
        Self {
            writer: Arc::new(Mutex::new(database)),
            database_path,
            reads: Arc::new(ReadConnectionPool::new(max_reads, read_wait)),
            admission,
        }
    }

    /// The sole ordinary serialized-write connection entry point.
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, Database>> {
        self.admission.lock(&self.writer)
    }

    /// Acquire the serialized writer after this runtime's mutation authority is already held.
    /// Re-entering `RestoreAdmission::lock` here would hold the admission mutex while waiting for
    /// the database mutex, preventing restore from observing `mutations_active` and refusing.
    pub(crate) fn lock_after_mutation(&self, mutation: &MutationGuard<'_>) -> LockResult<MutexGuard<'_, Database>> {
        assert!(
            std::ptr::eq(self.admission.as_ref(), mutation.admission),
            "database writer mutation authority belongs to another runtime"
        );
        self.writer.lock()
    }

    /// Open one stable, query-only WAL snapshot under a bounded permit. Restore admission spans the
    /// complete reader lifetime so a command cannot observe two database generations.
    pub(crate) fn open_read(&self) -> AppResult<ReadDatabase<'_>> {
        if self.database_path.as_ref() == ":memory:" {
            return Err(AppError::Other("bounded read snapshots require a file-backed database".to_string()));
        }

        // Acquire capacity before restore admission. Waiting for capacity while counted as an active
        // reader would deadlock a restore that has already published `pending` and is draining readers.
        let permit = self.reads.acquire()?;
        let admission = self.admission.begin_capture().map_err(AppError::Other)?;
        let database = Database::open_read_only(self.database_path.as_ref())?;
        Ok(ReadDatabase { database, _admission: admission, _permit: permit })
    }

    /// Execute one restore publication under this runtime's exact reservation and reopen the sole
    /// writer before admission can be released.
    ///
    /// The replacement connection is opened while the old writer is locked but before `operation`
    /// may mutate live pages. An inability to reopen therefore fails before the restore commit
    /// point. On success SQLite observes the page-generation change on the already-open replacement
    /// connection, then dropping the old `Database` clears all pre-restore connection-local state.
    pub(crate) fn with_restore_writer<T>(
        &self,
        reservation: &RestoreReservation<'_>,
        operation: impl FnOnce(&mut Database) -> Result<T, String>,
    ) -> Result<T, String> {
        if !std::ptr::eq(self.admission.as_ref(), reservation.admission) || !reservation.is_active() {
            return Err("restore writer access requires this DatabaseRuntime's active reservation".to_string());
        }
        if self.database_path.as_ref() == ":memory:" {
            return Err("restore publication requires a file-backed DatabaseRuntime".to_string());
        }

        let mut writer = self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !reservation.is_active() {
            return Err("restore reservation lost ownership before writer acquisition".to_string());
        }
        // Open first: after `operation` succeeds there is no fallible connection-creation step that
        // can report failure after the live generation already changed.
        let reopened = Database::open(self.database_path.as_ref())
            .map_err(|error| format!("could not prepare the post-restore database connection: {error}"))?;
        let value = operation(&mut writer)?;
        *writer = reopened;
        Ok(value)
    }
}

pub(crate) struct ReadDatabase<'a> {
    database: Database,
    _admission: SnapshotCaptureReservation<'a>,
    _permit: ReadConnectionPermit,
}

impl Deref for ReadDatabase<'_> {
    type Target = Database;

    fn deref(&self) -> &Self::Target {
        &self.database
    }
}

struct ReadConnectionPool {
    capacity: usize,
    available: Mutex<usize>,
    ready: Condvar,
    wait: Duration,
}

impl ReadConnectionPool {
    fn new(capacity: usize, wait: Duration) -> Self {
        let capacity = capacity.max(1);
        Self { capacity, available: Mutex::new(capacity), ready: Condvar::new(), wait }
    }

    fn acquire(self: &Arc<Self>) -> AppResult<ReadConnectionPermit> {
        let available = self.available.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut available, timeout) = self
            .ready
            .wait_timeout_while(available, self.wait, |remaining| *remaining == 0)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *available == 0 && timeout.timed_out() {
            return Err(AppError::Other(format!(
                "database read capacity exhausted ({} concurrent readers)",
                self.capacity
            )));
        }
        if *available == 0 {
            return Err(AppError::Other("database read capacity unavailable".to_string()));
        }
        *available -= 1;
        Ok(ReadConnectionPermit { pool: Arc::clone(self) })
    }
}

struct ReadConnectionPermit {
    pool: Arc<ReadConnectionPool>,
}

impl Drop for ReadConnectionPermit {
    fn drop(&mut self) {
        let mut available = self.pool.available.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *available = (*available + 1).min(self.pool.capacity);
        self.pool.ready.notify_one();
    }
}

/// One admission boundary for ordinary DB locks, bounded readers, snapshot capture and restore.
pub(crate) struct RestoreAdmission {
    pending: std::sync::atomic::AtomicBool,
    admission: Mutex<RestoreAdmissionState>,
    complete: Condvar,
}

#[derive(Debug, Default)]
struct RestoreAdmissionState {
    captures_active: usize,
    mutations_active: usize,
    generation: u64,
    phase: RestorePhase,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RestorePhase {
    #[default]
    Idle,
    ActiveNew,
    ActiveArmed,
    Parked,
}

impl RestoreAdmission {
    pub(crate) const fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            admission: Mutex::new(RestoreAdmissionState {
                captures_active: 0,
                mutations_active: 0,
                generation: 0,
                phase: RestorePhase::Idle,
            }),
            complete: Condvar::new(),
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn try_reserve(&self) -> Result<RestoreReservation<'_>, String> {
        self.reserve(false)
    }

    pub(crate) fn claim_recovery(&self) -> Result<RestoreReservation<'_>, String> {
        self.reserve(true)
    }

    fn reserve(&self, recovery_required: bool) -> Result<RestoreReservation<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match admission.phase {
            RestorePhase::Idle => {
                self.pending
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .map_err(|_| "Database restore admission is inconsistent; restart before retrying.".to_string())?;
                admission.phase = if recovery_required { RestorePhase::ActiveArmed } else { RestorePhase::ActiveNew };
            }
            RestorePhase::Parked if recovery_required => {
                self.pending.store(true, std::sync::atomic::Ordering::SeqCst);
                admission.phase = RestorePhase::ActiveArmed;
            }
            RestorePhase::Parked => {
                return Err("An interrupted database restore is recovery-required; retry its exact recorded snapshot."
                    .to_string());
            }
            RestorePhase::ActiveNew | RestorePhase::ActiveArmed => {
                return Err("A database restore is already in progress — wait for it to finish.".to_string());
            }
        }
        admission.generation = admission.generation.wrapping_add(1);
        let generation = admission.generation;
        while admission.captures_active > 0 {
            admission = self.complete.wait(admission).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if admission.mutations_active > 0 {
            if admission.phase == RestorePhase::ActiveArmed {
                admission.phase = RestorePhase::Parked;
                self.pending.store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                admission.phase = RestorePhase::Idle;
                self.pending.store(false, std::sync::atomic::Ordering::SeqCst);
                self.complete.notify_all();
            }
            return Err(
                "A database or configuration mutation is already in progress — let it finish before restoring."
                    .to_string(),
            );
        }
        Ok(RestoreReservation { admission: self, generation })
    }

    pub(crate) fn begin_capture(&self) -> Result<SnapshotCaptureReservation<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_pending() {
            return Err("snapshot refused because a database restore is reserved/in progress".to_string());
        }
        admission.captures_active = admission.captures_active.saturating_add(1);
        Ok(SnapshotCaptureReservation { admission: self })
    }

    pub(crate) fn begin_mutation(&self) -> Result<MutationGuard<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.phase != RestorePhase::Idle || self.is_pending() {
            return Err(RESTORE_IN_PROGRESS_MSG.to_string());
        }
        admission.mutations_active = admission.mutations_active.saturating_add(1);
        Ok(MutationGuard { admission: self })
    }

    pub(crate) fn lock<'a, T>(&self, mutex: &'a Mutex<T>) -> LockResult<MutexGuard<'a, T>> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while self.is_pending() {
            admission = self.complete.wait(admission).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let database = mutex.lock();
        drop(admission);
        database
    }
}

pub(crate) static RESTORE_ADMISSION: LazyLock<Arc<RestoreAdmission>> =
    LazyLock::new(|| Arc::new(RestoreAdmission::new()));

pub(crate) fn begin_mutation() -> Result<MutationGuard<'static>, String> {
    RESTORE_ADMISSION.begin_mutation()
}

pub(crate) fn begin_snapshot_capture(primary_data_dir: &Path) -> Result<SnapshotCaptureReservation<'static>, String> {
    let capture = RESTORE_ADMISSION.begin_capture()?;
    let pending = primary_data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    match std::fs::symlink_metadata(&pending) {
        Ok(_) => Err(format!(
            "snapshot refused while interrupted restore barrier {} exists; complete or repair that restore first",
            pending.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(capture),
        Err(error) => Err(format!("snapshot could not inspect restore barrier {}: {error}", pending.display())),
    }
}

pub(crate) struct SnapshotCaptureReservation<'a> {
    admission: &'a RestoreAdmission,
}

pub(crate) struct MutationGuard<'a> {
    admission: &'a RestoreAdmission,
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.mutations_active = state.mutations_active.saturating_sub(1);
        self.admission.complete.notify_all();
    }
}

impl Drop for SnapshotCaptureReservation<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.captures_active = state.captures_active.saturating_sub(1);
        self.admission.complete.notify_all();
    }
}

pub(crate) fn restore_pending() -> bool {
    RESTORE_ADMISSION.is_pending()
}

pub(crate) const RESTORE_IN_PROGRESS_MSG: &str =
    "A database restore is in progress — wait for it to finish before starting this operation.";

pub(crate) struct RestoreReservation<'a> {
    admission: &'a RestoreAdmission,
    generation: u64,
}

impl RestoreReservation<'_> {
    pub(crate) fn is_active(&self) -> bool {
        let state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation == self.generation
            && matches!(state.phase, RestorePhase::ActiveNew | RestorePhase::ActiveArmed)
    }

    pub(crate) fn arm_named_restore(&self) -> Result<(), String> {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation {
            return Err("restore reservation lost ownership before durable transaction arm".to_string());
        }
        match state.phase {
            RestorePhase::ActiveNew => {
                state.phase = RestorePhase::ActiveArmed;
                Ok(())
            }
            RestorePhase::ActiveArmed => Ok(()),
            RestorePhase::Idle | RestorePhase::Parked => {
                Err("restore reservation is not active while arming durable transaction".to_string())
            }
        }
    }

    pub(crate) fn disarm_named_restore_if_safe(&self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation == self.generation && state.phase == RestorePhase::ActiveArmed {
            state.phase = RestorePhase::ActiveNew;
        }
    }

    pub(crate) fn commit_named_restore(&self) -> Result<(), String> {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation || state.phase != RestorePhase::ActiveArmed {
            return Err("restore transaction lost its exclusive admission before commit".to_string());
        }
        state.phase = RestorePhase::Idle;
        self.admission.pending.store(false, std::sync::atomic::Ordering::SeqCst);
        self.admission.complete.notify_all();
        Ok(())
    }
}

impl Drop for RestoreReservation<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation {
            return;
        }
        match state.phase {
            RestorePhase::ActiveNew => {
                state.phase = RestorePhase::Idle;
                self.admission.pending.store(false, std::sync::atomic::Ordering::SeqCst);
                self.admission.complete.notify_all();
            }
            RestorePhase::ActiveArmed => {
                state.phase = RestorePhase::Parked;
                tracing::error!(
                    "named restore remains incomplete; database admission is parked until exact recovery completes"
                );
            }
            RestorePhase::Idle | RestorePhase::Parked => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_runtime(max_reads: usize, wait: Duration) -> (tempfile::TempDir, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let database = Database::open(path.to_string_lossy().as_ref()).unwrap();
        database.initialize().unwrap();
        let runtime = DatabaseRuntime::with_admission(database, max_reads, wait, Arc::new(RestoreAdmission::new()));
        (directory, runtime)
    }

    #[test]
    fn bounded_reads_refuse_exhaustion_and_release_capacity() {
        let (_directory, runtime) = file_runtime(1, Duration::from_millis(20));
        let first = runtime.open_read().unwrap();
        let error = runtime.open_read().err().expect("second reader must be refused");
        assert!(error.to_string().contains("read capacity exhausted"));
        drop(first);
        let reopened = runtime.open_read().unwrap();
        assert_eq!(reopened.segment_count().unwrap(), 0);
    }

    #[test]
    fn writer_lock_after_mutation_keeps_restore_admission_closed() {
        let (_directory, runtime) = file_runtime(1, Duration::from_millis(20));
        let mutation = runtime.admission.begin_mutation().expect("claim runtime mutation authority");
        {
            let database = runtime.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(database.segment_count().unwrap(), 0);
        }

        let error = runtime.admission.try_reserve().err().expect("restore must see the active mutation");
        assert!(error.contains("mutation is already in progress"), "{error}");
        drop(mutation);

        let reservation = runtime.admission.try_reserve().expect("restore may proceed after mutation release");
        drop(reservation);
    }

    #[test]
    fn active_read_snapshot_drains_before_restore_reservation() {
        let (_directory, runtime) = file_runtime(1, Duration::from_millis(20));
        let reader = runtime.open_read().unwrap();
        let admission = Arc::clone(&runtime.admission);
        let waiter = std::thread::spawn(move || {
            let reservation = admission.try_reserve().unwrap();
            drop(reservation);
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(!waiter.is_finished(), "restore must wait for the stable read snapshot");
        drop(reader);
        waiter.join().unwrap();
        assert!(!runtime.admission.is_pending());
    }

    #[test]
    fn bounded_read_snapshot_is_a_valid_online_backup_source() {
        let (directory, runtime) = file_runtime(1, Duration::from_millis(20));
        let destination = directory.path().join("backup.db");
        let reader = runtime.open_read().unwrap();
        reader.backup(&destination).unwrap();
        drop(reader);

        let backup = Database::open_read_only(destination.to_string_lossy().as_ref()).unwrap();
        assert_eq!(backup.segment_count().unwrap(), 0);
        assert_eq!(
            crate::migrations::validate_applied_history(backup.connection()).unwrap(),
            crate::migrations::max_supported_version()
        );
    }

    #[test]
    fn bounded_read_does_not_wait_for_the_serialized_writer_mutex() {
        let (_directory, runtime) = file_runtime(1, Duration::from_millis(10));
        let _writer = runtime.lock().unwrap();

        let reader = runtime.open_read().expect("cached live path must let readers bypass the writer mutex");
        assert_eq!(reader.segment_count().unwrap(), 0);
    }

    #[test]
    fn successful_restore_reopens_the_writer_before_admission_releases() {
        let (_directory, runtime) = file_runtime(1, Duration::from_millis(20));
        {
            let writer = runtime.lock().unwrap();
            writer
                .insert_segment(&crate::db::SpeechSegment {
                    id: "old-generation".into(),
                    audio_path: "old.wav".into(),
                    raw_transcript: "old".into(),
                    duration_ms: 1_000,
                    ..crate::db::SpeechSegment::default()
                })
                .unwrap();
            writer.connection().execute_batch("PRAGMA synchronous=OFF;").unwrap();
        }

        let replacement = Database::open(":memory:").unwrap();
        replacement.initialize().unwrap();
        replacement
            .insert_segment(&crate::db::SpeechSegment {
                id: "restored-generation".into(),
                audio_path: "restored.wav".into(),
                raw_transcript: "restored".into(),
                duration_ms: 1_000,
                ..crate::db::SpeechSegment::default()
            })
            .unwrap();

        let reservation = runtime.admission.try_reserve().unwrap();
        runtime
            .with_restore_writer(&reservation, |writer| {
                writer.commit_staged_restore(&replacement).map_err(|error| error.to_string())
            })
            .unwrap();
        assert!(reservation.is_active(), "runtime must not release the caller-owned restore reservation");
        drop(reservation);

        let writer = runtime.lock().unwrap();
        let synchronous: i64 = writer.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1, "reopened writer must restore Database::open connection pragmas");
        assert!(writer.get_segment_by_id("old-generation").unwrap().is_none());
        assert!(writer.get_segment_by_id("restored-generation").unwrap().is_some());
        drop(writer);

        let reader = runtime.open_read().unwrap();
        assert!(reader.get_segment_by_id("restored-generation").unwrap().is_some());
    }
}
