//! Crash-recoverable OmniASR champion promotion.
//!
//! Promotion crosses four independently durable systems: the registry database, the champion pointer,
//! the WSL worker process, and the worker's loaded identity/canary result. A SQLite transaction cannot
//! cover those systems. This module therefore uses a durable, idempotent saga whose completed phase is
//! stored in `jobs.payload_json` after every side effect. A crash before a phase stamp simply repeats an
//! idempotent setter on recovery; a crash after the stamp resumes at the next effect.
//!
//! Important lock contract: every [`PromotionJournal`] call must release its database lock before it
//! returns. Runtime callbacks are invoked only between journal calls. Callers must not wrap this runner
//! in an outer database/AppState mutex guard: a restart can block, and holding that mutex would deadlock
//! status/health work and turn rollback into a second outage.

use crate::error::{AppError, AppResult};
use crate::jobs::{BegunPayloadJob, JobState, PayloadJob};
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MODEL_PROMOTION_JOB_KIND: &str = "model_promotion";
pub const PROMOTION_FAILED: &str = "PROMOTION_FAILED";
pub const ROLLBACK_UNVERIFIED: &str = "ROLLBACK_UNVERIFIED";
pub const PROMOTION_INTERRUPTED: &str = "PROMOTION_INTERRUPTED";
const PROMOTION_PAYLOAD_SCHEMA: u32 = 1;

/// Exact deployable identity. A model id without the raw deployment-manifest SHA is not an identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExactDeploymentIdentity {
    pub model_version_id: String,
    pub deployment_sha256: String,
}

/// Identity of the immutable canary suite and the exact model it must observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanaryIdentity {
    pub suite_id: String,
    pub suite_sha256: String,
    pub expected_model: ExactDeploymentIdentity,
}

/// Immutable evidence and identities admitted for one promotion attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotionContract {
    pub incumbent: ExactDeploymentIdentity,
    pub candidate: ExactDeploymentIdentity,
    pub scorecard_sha256: String,
    pub verdict_sha256: String,
    pub canary: CanaryIdentity,
}

/// Last side effect durably known to have completed. Setters in [`PromotionRuntime`] MUST be
/// idempotent because a process can die after the effect and before this phase advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionPhase {
    Prepared,
    CandidateDbActivated,
    CandidatePointerPublished,
    CandidateRestarted,
    CandidateVerified,
    RollbackPending,
    RollbackDbRestored,
    RollbackPointerRestored,
    RollbackRestarted,
    RollbackVerified,
    Completed,
    RollbackUnverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotionFailure {
    /// Stable callback-provided code, never a raw exception/message which could contain a secret.
    pub runtime_code: String,
    pub operation: String,
    pub rollback_code: Option<String>,
}

/// Entire recovery journal. Immutable admission fields stay beside the phase, so recovery never has
/// to reconstruct model/evidence identity from whichever registry state happens to be current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotionPayload {
    pub schema: u32,
    pub phase: PromotionPhase,
    pub contract: PromotionContract,
    pub failure: Option<PromotionFailure>,
}

impl PromotionPayload {
    pub fn prepared(contract: PromotionContract) -> Result<Self, PromotionSagaError> {
        let payload =
            Self { schema: PROMOTION_PAYLOAD_SCHEMA, phase: PromotionPhase::Prepared, contract, failure: None };
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), PromotionSagaError> {
        if self.schema != PROMOTION_PAYLOAD_SCHEMA {
            return Err(PromotionSagaError::contract(format!("unsupported promotion payload schema {}", self.schema)));
        }
        validate_identity("incumbent", &self.contract.incumbent)?;
        validate_identity("candidate", &self.contract.candidate)?;
        if self.contract.incumbent == self.contract.candidate {
            return Err(PromotionSagaError::contract("candidate is already the exact incumbent"));
        }
        validate_sha("scorecardSha256", &self.contract.scorecard_sha256)?;
        validate_sha("verdictSha256", &self.contract.verdict_sha256)?;
        validate_token("canary.suiteId", &self.contract.canary.suite_id, 160)?;
        validate_sha("canary.suiteSha256", &self.contract.canary.suite_sha256)?;
        validate_identity("canary.expectedModel", &self.contract.canary.expected_model)?;
        if self.contract.canary.expected_model != self.contract.candidate {
            return Err(PromotionSagaError::contract("canary expectedModel must equal the exact candidate identity"));
        }
        if let Some(failure) = &self.failure {
            validate_token("failure.runtimeCode", &failure.runtime_code, 96)?;
            validate_token("failure.operation", &failure.operation, 96)?;
            if let Some(code) = &failure.rollback_code {
                validate_token("failure.rollbackCode", code, 96)?;
            }
        }
        let rollback_phase = matches!(
            self.phase,
            PromotionPhase::RollbackPending
                | PromotionPhase::RollbackDbRestored
                | PromotionPhase::RollbackPointerRestored
                | PromotionPhase::RollbackRestarted
                | PromotionPhase::RollbackVerified
                | PromotionPhase::RollbackUnverified
        );
        if rollback_phase != self.failure.is_some() {
            return Err(PromotionSagaError::contract(
                "rollback phases require failure provenance and candidate phases forbid it",
            ));
        }
        Ok(())
    }
}

fn validate_identity(label: &str, identity: &ExactDeploymentIdentity) -> Result<(), PromotionSagaError> {
    validate_token(&format!("{label}.modelVersionId"), &identity.model_version_id, 160)?;
    validate_sha(&format!("{label}.deploymentSha256"), &identity.deployment_sha256)
}

fn validate_sha(label: &str, value: &str) -> Result<(), PromotionSagaError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(PromotionSagaError::contract(format!(
            "{label} must be exactly 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_token(label: &str, value: &str, max_len: usize) -> Result<(), PromotionSagaError> {
    if !is_safe_token(value, max_len) {
        return Err(PromotionSagaError::contract(format!("{label} must be 1..={max_len} safe ASCII token characters")));
    }
    Ok(())
}

fn is_safe_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn durable_runtime_code(code: &str) -> String {
    if is_safe_token(code, 96) {
        code.to_string()
    } else {
        // A buggy callback must never prevent rollback or persist an exception/secret as evidence.
        "RUNTIME_ERROR_INVALID_CODE".to_string()
    }
}

/// Runtime callbacks deliberately use exact setters, not toggles. Every method must be idempotent.
/// `verify_exact_and_canary` must fail unless both the loaded `{id, sha}` and the fixed suite pass.
pub trait PromotionRuntime {
    fn set_database_champion(&mut self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError>;
    fn publish_champion_pointer(&mut self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError>;
    fn restart_champion(&mut self) -> Result<(), PromotionRuntimeError>;
    fn verify_exact_and_canary(
        &mut self,
        identity: &ExactDeploymentIdentity,
        canary: &CanaryIdentity,
    ) -> Result<(), PromotionRuntimeError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRuntimeError {
    pub code: String,
}

impl PromotionRuntimeError {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Persistence seam used by production SQLite and deterministic in-memory fault tests.
pub trait PromotionJournal {
    fn begin_running(
        &mut self,
        job_id: &str,
        idempotency_key: &str,
        payload_json: &str,
    ) -> Result<BegunPayloadJob, PromotionSagaError>;
    fn load(&mut self, job_id: &str) -> Result<Option<PayloadJob>, PromotionSagaError>;
    fn compare_and_swap_payload(
        &mut self,
        job_id: &str,
        expected_json: &str,
        next_json: &str,
    ) -> Result<(), PromotionSagaError>;
    fn finish(
        &mut self,
        job_id: &str,
        expected_json: &str,
        final_json: &str,
        state: JobState,
        error_code: Option<&str>,
    ) -> Result<(), PromotionSagaError>;
}

impl PromotionJournal for crate::db::Database {
    fn begin_running(
        &mut self,
        job_id: &str,
        idempotency_key: &str,
        payload_json: &str,
    ) -> Result<BegunPayloadJob, PromotionSagaError> {
        self.begin_running_payload_job(job_id, MODEL_PROMOTION_JOB_KIND, idempotency_key, payload_json)
            .map_err(PromotionSagaError::journal)
    }

    fn load(&mut self, job_id: &str) -> Result<Option<PayloadJob>, PromotionSagaError> {
        self.get_payload_job(job_id).map_err(PromotionSagaError::journal)
    }

    fn compare_and_swap_payload(
        &mut self,
        job_id: &str,
        expected_json: &str,
        next_json: &str,
    ) -> Result<(), PromotionSagaError> {
        self.compare_and_swap_running_job_payload(job_id, MODEL_PROMOTION_JOB_KIND, expected_json, next_json)
            .map_err(PromotionSagaError::journal)
    }

    fn finish(
        &mut self,
        job_id: &str,
        expected_json: &str,
        final_json: &str,
        state: JobState,
        error_code: Option<&str>,
    ) -> Result<(), PromotionSagaError> {
        self.finish_running_payload_job(job_id, MODEL_PROMOTION_JOB_KIND, expected_json, final_json, state, error_code)
            .map_err(PromotionSagaError::journal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionOutcome {
    Promoted { identity: ExactDeploymentIdentity },
    RolledBack { incumbent: ExactDeploymentIdentity, runtime_code: String },
    AlreadyTerminal { state: JobState, error_code: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionSagaError {
    pub code: &'static str,
    pub detail: String,
}

impl PromotionSagaError {
    fn contract(detail: impl Into<String>) -> Self {
        Self { code: "PROMOTION_CONTRACT_INVALID", detail: detail.into() }
    }

    fn journal(error: impl fmt::Display) -> Self {
        Self { code: "PROMOTION_JOURNAL_ERROR", detail: error.to_string() }
    }

    #[cfg(test)]
    fn interrupted(phase: PromotionPhase) -> Self {
        Self { code: PROMOTION_INTERRUPTED, detail: format!("interrupted after durable phase {phase:?}") }
    }

    fn rollback_unverified(code: &str) -> Self {
        Self { code: ROLLBACK_UNVERIFIED, detail: format!("incumbent recovery callback failed with {code}") }
    }
}

impl fmt::Display for PromotionSagaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for PromotionSagaError {}

/// Test seam representing sudden process death immediately after a durable phase. Production uses
/// [`NoPromotionFault`]. A returned error MUST NOT trigger rollback because a real crash cannot do so.
pub trait PromotionCheckpoint {
    fn after_persisted_phase(&mut self, phase: PromotionPhase) -> Result<(), PromotionSagaError>;
}

pub struct NoPromotionFault;

impl PromotionCheckpoint for NoPromotionFault {
    fn after_persisted_phase(&mut self, _phase: PromotionPhase) -> Result<(), PromotionSagaError> {
        Ok(())
    }
}

/// Admit and run a new promotion. Idempotency-key retries resume the exact existing contract and are
/// rejected if any model/evidence/canary identity differs.
pub fn start_promotion<J: PromotionJournal, R: PromotionRuntime>(
    journal: &mut J,
    runtime: &mut R,
    job_id: &str,
    idempotency_key: &str,
    contract: PromotionContract,
) -> Result<PromotionOutcome, PromotionSagaError> {
    let mut checkpoint = NoPromotionFault;
    start_promotion_with_checkpoint(journal, runtime, &mut checkpoint, job_id, idempotency_key, contract)
}

pub fn start_promotion_with_checkpoint<J: PromotionJournal, R: PromotionRuntime, C: PromotionCheckpoint>(
    journal: &mut J,
    runtime: &mut R,
    checkpoint: &mut C,
    job_id: &str,
    idempotency_key: &str,
    contract: PromotionContract,
) -> Result<PromotionOutcome, PromotionSagaError> {
    validate_token("jobId", job_id, 160)?;
    validate_token("idempotencyKey", idempotency_key, 200)?;
    let initial = PromotionPayload::prepared(contract.clone())?;
    let initial_json = encode_payload(&initial)?;
    let begun = journal.begin_running(job_id, idempotency_key, &initial_json)?;
    validate_begun_job(&begun.job, job_id, idempotency_key, &contract)?;
    if begun.created {
        checkpoint.after_persisted_phase(PromotionPhase::Prepared)?;
    }
    if begun.job.state.is_terminal() {
        return terminal_outcome(&begun.job);
    }
    drive(journal, runtime, checkpoint, job_id, begun.job.payload_json)
}

/// Resume one startup-discovered running promotion. The job itself carries all immutable identity and
/// evidence pins; recovery takes no renderer/user-supplied candidate fields.
pub fn resume_promotion<J: PromotionJournal, R: PromotionRuntime>(
    journal: &mut J,
    runtime: &mut R,
    job_id: &str,
) -> Result<PromotionOutcome, PromotionSagaError> {
    let mut checkpoint = NoPromotionFault;
    resume_promotion_with_checkpoint(journal, runtime, &mut checkpoint, job_id)
}

pub fn resume_promotion_with_checkpoint<J: PromotionJournal, R: PromotionRuntime, C: PromotionCheckpoint>(
    journal: &mut J,
    runtime: &mut R,
    checkpoint: &mut C,
    job_id: &str,
) -> Result<PromotionOutcome, PromotionSagaError> {
    let job = journal
        .load(job_id)?
        .ok_or_else(|| PromotionSagaError::contract(format!("promotion job {job_id} does not exist")))?;
    if job.kind != MODEL_PROMOTION_JOB_KIND {
        return Err(PromotionSagaError::contract(format!(
            "job {job_id} is kind {}, not {MODEL_PROMOTION_JOB_KIND}",
            job.kind
        )));
    }
    if job.state.is_terminal() {
        return terminal_outcome(&job);
    }
    if job.state != JobState::Running {
        return Err(PromotionSagaError::contract(format!("promotion job {job_id} is {}, not running", job.state)));
    }
    drive(journal, runtime, checkpoint, job_id, job.payload_json)
}

fn terminal_outcome(job: &PayloadJob) -> Result<PromotionOutcome, PromotionSagaError> {
    let payload = decode_payload(&job.payload_json)?;
    let valid = matches!(
        (job.state, job.error_code.as_deref(), payload.phase),
        (JobState::Succeeded, None, PromotionPhase::Completed)
            | (JobState::Failed, Some(PROMOTION_FAILED), PromotionPhase::RollbackVerified)
            | (JobState::Failed, Some(ROLLBACK_UNVERIFIED), PromotionPhase::RollbackUnverified)
    );
    if !valid {
        return Err(PromotionSagaError::contract(format!(
            "terminal job state/error/phase disagree: {}/{:?}/{:?}",
            job.state, job.error_code, payload.phase
        )));
    }
    Ok(PromotionOutcome::AlreadyTerminal { state: job.state, error_code: job.error_code.clone() })
}

fn validate_begun_job(
    job: &PayloadJob,
    job_id: &str,
    idempotency_key: &str,
    contract: &PromotionContract,
) -> Result<(), PromotionSagaError> {
    if job.id != job_id || job.kind != MODEL_PROMOTION_JOB_KIND {
        return Err(PromotionSagaError::contract(
            "job id/idempotency collision belongs to a different durable operation",
        ));
    }
    if job.idempotency_key.as_deref() != Some(idempotency_key) {
        return Err(PromotionSagaError::contract("job id collision has a different idempotency key"));
    }
    let persisted = decode_payload(&job.payload_json)?;
    if persisted.contract != *contract {
        return Err(PromotionSagaError::contract("idempotency key already pins a different promotion contract"));
    }
    Ok(())
}

fn drive<J: PromotionJournal, R: PromotionRuntime, C: PromotionCheckpoint>(
    journal: &mut J,
    runtime: &mut R,
    checkpoint: &mut C,
    job_id: &str,
    mut current_json: String,
) -> Result<PromotionOutcome, PromotionSagaError> {
    loop {
        let mut payload = decode_payload(&current_json)?;
        match payload.phase {
            PromotionPhase::Prepared => match runtime.set_database_champion(&payload.contract.candidate) {
                Ok(()) => advance_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    PromotionPhase::CandidateDbActivated,
                )?,
                Err(error) => begin_rollback(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    "set_candidate_database",
                    error,
                )?,
            },
            PromotionPhase::CandidateDbActivated => {
                match runtime.publish_champion_pointer(&payload.contract.candidate) {
                    Ok(()) => advance_phase(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        PromotionPhase::CandidatePointerPublished,
                    )?,
                    Err(error) => begin_rollback(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        "publish_candidate_pointer",
                        error,
                    )?,
                }
            }
            PromotionPhase::CandidatePointerPublished => match runtime.restart_champion() {
                Ok(()) => advance_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    PromotionPhase::CandidateRestarted,
                )?,
                Err(error) => begin_rollback(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    "restart_candidate",
                    error,
                )?,
            },
            PromotionPhase::CandidateRestarted => {
                match runtime.verify_exact_and_canary(&payload.contract.candidate, &payload.contract.canary) {
                    Ok(()) => advance_phase(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        PromotionPhase::CandidateVerified,
                    )?,
                    Err(error) => begin_rollback(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        "verify_candidate_exact_and_canary",
                        error,
                    )?,
                }
            }
            PromotionPhase::CandidateVerified => {
                // Re-verify immediately before the terminal CAS. If the app died after the first pass,
                // startup cannot convert an old pass into success while a different worker is loaded.
                match runtime.verify_exact_and_canary(&payload.contract.candidate, &payload.contract.canary) {
                    Ok(()) => {
                        let identity = payload.contract.candidate.clone();
                        finish_phase(
                            journal,
                            checkpoint,
                            job_id,
                            &payload,
                            &current_json,
                            TerminalPhase {
                                phase: PromotionPhase::Completed,
                                state: JobState::Succeeded,
                                error_code: None,
                            },
                        )?;
                        return Ok(PromotionOutcome::Promoted { identity });
                    }
                    Err(error) => begin_rollback(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        "final_candidate_reverification",
                        error,
                    )?,
                }
            }
            PromotionPhase::RollbackPending => match runtime.set_database_champion(&payload.contract.incumbent) {
                Ok(()) => advance_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    PromotionPhase::RollbackDbRestored,
                )?,
                Err(error) => {
                    return finish_rollback_unverified(journal, checkpoint, job_id, payload, current_json, error);
                }
            },
            PromotionPhase::RollbackDbRestored => match runtime.publish_champion_pointer(&payload.contract.incumbent) {
                Ok(()) => advance_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    PromotionPhase::RollbackPointerRestored,
                )?,
                Err(error) => {
                    return finish_rollback_unverified(journal, checkpoint, job_id, payload, current_json, error);
                }
            },
            PromotionPhase::RollbackPointerRestored => match runtime.restart_champion() {
                Ok(()) => advance_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &mut payload,
                    &mut current_json,
                    PromotionPhase::RollbackRestarted,
                )?,
                Err(error) => {
                    return finish_rollback_unverified(journal, checkpoint, job_id, payload, current_json, error);
                }
            },
            PromotionPhase::RollbackRestarted => {
                match runtime.verify_exact_and_canary(&payload.contract.incumbent, &rollback_canary(&payload.contract))
                {
                    Ok(()) => advance_phase(
                        journal,
                        checkpoint,
                        job_id,
                        &mut payload,
                        &mut current_json,
                        PromotionPhase::RollbackVerified,
                    )?,
                    Err(error) => {
                        return finish_rollback_unverified(journal, checkpoint, job_id, payload, current_json, error);
                    }
                }
            }
            PromotionPhase::RollbackVerified => {
                // As on promotion, a recovery from this phase rechecks exact live identity before the
                // terminal write. The candidate can never be reported as safely rolled back on stale proof.
                if let Err(error) =
                    runtime.verify_exact_and_canary(&payload.contract.incumbent, &rollback_canary(&payload.contract))
                {
                    return finish_rollback_unverified(journal, checkpoint, job_id, payload, current_json, error);
                }
                let failure = payload.failure.as_ref().ok_or_else(|| {
                    PromotionSagaError::contract("rollback verified without original failure provenance")
                })?;
                let runtime_code = failure.runtime_code.clone();
                let incumbent = payload.contract.incumbent.clone();
                finish_phase(
                    journal,
                    checkpoint,
                    job_id,
                    &payload,
                    &current_json,
                    TerminalPhase {
                        phase: PromotionPhase::RollbackVerified,
                        state: JobState::Failed,
                        error_code: Some(PROMOTION_FAILED),
                    },
                )?;
                return Ok(PromotionOutcome::RolledBack { incumbent, runtime_code });
            }
            PromotionPhase::Completed | PromotionPhase::RollbackUnverified => {
                return Err(PromotionSagaError::contract(format!(
                    "terminal phase {:?} persisted on a running job",
                    payload.phase
                )));
            }
        }
    }
}

fn rollback_canary(contract: &PromotionContract) -> CanaryIdentity {
    // The immutable suite is reused, but exact identity is replaced with the incumbent. Runtime must
    // still execute the fixed suite; it may not treat rollback as a health-only TCP probe.
    CanaryIdentity {
        suite_id: contract.canary.suite_id.clone(),
        suite_sha256: contract.canary.suite_sha256.clone(),
        expected_model: contract.incumbent.clone(),
    }
}

fn begin_rollback<J: PromotionJournal, C: PromotionCheckpoint>(
    journal: &mut J,
    checkpoint: &mut C,
    job_id: &str,
    payload: &mut PromotionPayload,
    current_json: &mut String,
    operation: &str,
    error: PromotionRuntimeError,
) -> Result<(), PromotionSagaError> {
    payload.failure = Some(PromotionFailure {
        runtime_code: durable_runtime_code(&error.code),
        operation: operation.to_string(),
        rollback_code: None,
    });
    advance_phase(journal, checkpoint, job_id, payload, current_json, PromotionPhase::RollbackPending)
}

fn finish_rollback_unverified<J: PromotionJournal, C: PromotionCheckpoint>(
    journal: &mut J,
    checkpoint: &mut C,
    job_id: &str,
    mut payload: PromotionPayload,
    current_json: String,
    error: PromotionRuntimeError,
) -> Result<PromotionOutcome, PromotionSagaError> {
    let code = durable_runtime_code(&error.code);
    let failure = payload
        .failure
        .as_mut()
        .ok_or_else(|| PromotionSagaError::contract("rollback failed without original failure provenance"))?;
    failure.rollback_code = Some(code.clone());
    finish_phase(
        journal,
        checkpoint,
        job_id,
        &payload,
        &current_json,
        TerminalPhase {
            phase: PromotionPhase::RollbackUnverified,
            state: JobState::Failed,
            error_code: Some(ROLLBACK_UNVERIFIED),
        },
    )?;
    Err(PromotionSagaError::rollback_unverified(&code))
}

fn advance_phase<J: PromotionJournal, C: PromotionCheckpoint>(
    journal: &mut J,
    checkpoint: &mut C,
    job_id: &str,
    payload: &mut PromotionPayload,
    current_json: &mut String,
    phase: PromotionPhase,
) -> Result<(), PromotionSagaError> {
    payload.phase = phase;
    payload.validate()?;
    let next_json = encode_payload(payload)?;
    journal.compare_and_swap_payload(job_id, current_json, &next_json)?;
    *current_json = next_json;
    checkpoint.after_persisted_phase(phase)
}

struct TerminalPhase {
    phase: PromotionPhase,
    state: JobState,
    error_code: Option<&'static str>,
}

fn finish_phase<J: PromotionJournal, C: PromotionCheckpoint>(
    journal: &mut J,
    checkpoint: &mut C,
    job_id: &str,
    payload: &PromotionPayload,
    current_json: &str,
    terminal: TerminalPhase,
) -> Result<(), PromotionSagaError> {
    let mut final_payload = payload.clone();
    final_payload.phase = terminal.phase;
    final_payload.validate()?;
    let final_json = encode_payload(&final_payload)?;
    journal.finish(job_id, current_json, &final_json, terminal.state, terminal.error_code)?;
    checkpoint.after_persisted_phase(terminal.phase)
}

fn encode_payload(payload: &PromotionPayload) -> Result<String, PromotionSagaError> {
    serde_json::to_string(payload).map_err(PromotionSagaError::journal)
}

pub fn decode_payload(json: &str) -> Result<PromotionPayload, PromotionSagaError> {
    let payload: PromotionPayload = serde_json::from_str(json).map_err(PromotionSagaError::journal)?;
    payload.validate()?;
    Ok(payload)
}

/// Startup helper intentionally returns payload-bearing rows rather than reaping them as interrupted.
pub fn running_promotions(database: &crate::db::Database) -> AppResult<Vec<PayloadJob>> {
    database.list_running_payload_jobs(MODEL_PROMOTION_JOB_KIND)
}

/// Narrow conversion for callers that use the app's error surface.
pub fn saga_error(error: PromotionSagaError) -> AppError {
    AppError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn identity(id: &str, c: char) -> ExactDeploymentIdentity {
        ExactDeploymentIdentity { model_version_id: id.to_string(), deployment_sha256: sha(c) }
    }

    fn contract() -> PromotionContract {
        let candidate = identity("omniasr-7b-challenger-42", 'b');
        PromotionContract {
            incumbent: identity("omniasr-7b-champion-41", 'a'),
            candidate: candidate.clone(),
            scorecard_sha256: sha('c'),
            verdict_sha256: sha('d'),
            canary: CanaryIdentity {
                suite_id: "fixed-sorani-canary-v1".to_string(),
                suite_sha256: sha('e'),
                expected_model: candidate,
            },
        }
    }

    #[derive(Default)]
    struct FakeJournal {
        jobs: HashMap<String, PayloadJob>,
    }

    impl PromotionJournal for FakeJournal {
        fn begin_running(
            &mut self,
            job_id: &str,
            idempotency_key: &str,
            payload_json: &str,
        ) -> Result<BegunPayloadJob, PromotionSagaError> {
            if let Some(existing) =
                self.jobs.values().find(|job| job.idempotency_key.as_deref() == Some(idempotency_key)).cloned()
            {
                return Ok(BegunPayloadJob { job: existing, created: false });
            }
            let job = PayloadJob {
                id: job_id.to_string(),
                kind: MODEL_PROMOTION_JOB_KIND.to_string(),
                state: JobState::Running,
                idempotency_key: Some(idempotency_key.to_string()),
                error_code: None,
                payload_json: payload_json.to_string(),
            };
            self.jobs.insert(job_id.to_string(), job.clone());
            Ok(BegunPayloadJob { job, created: true })
        }

        fn load(&mut self, job_id: &str) -> Result<Option<PayloadJob>, PromotionSagaError> {
            Ok(self.jobs.get(job_id).cloned())
        }

        fn compare_and_swap_payload(
            &mut self,
            job_id: &str,
            expected_json: &str,
            next_json: &str,
        ) -> Result<(), PromotionSagaError> {
            let job = self.jobs.get_mut(job_id).ok_or_else(|| PromotionSagaError::journal("missing job"))?;
            if job.state != JobState::Running || job.payload_json != expected_json {
                return Err(PromotionSagaError::journal("stale phase CAS"));
            }
            job.payload_json = next_json.to_string();
            Ok(())
        }

        fn finish(
            &mut self,
            job_id: &str,
            expected_json: &str,
            final_json: &str,
            state: JobState,
            error_code: Option<&str>,
        ) -> Result<(), PromotionSagaError> {
            let job = self.jobs.get_mut(job_id).ok_or_else(|| PromotionSagaError::journal("missing job"))?;
            if job.state != JobState::Running || job.payload_json != expected_json {
                return Err(PromotionSagaError::journal("stale terminal CAS"));
            }
            job.payload_json = final_json.to_string();
            job.state = state;
            job.error_code = error_code.map(str::to_string);
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        SetCandidateDb,
        CandidatePointer,
        CandidateRestart,
        CandidateVerify,
        CandidateFinalVerify,
        SetIncumbentDb,
        IncumbentPointer,
        IncumbentRestart,
        IncumbentVerify,
        IncumbentFinalVerify,
    }

    struct FakeRuntime {
        contract: PromotionContract,
        db: ExactDeploymentIdentity,
        pointer: ExactDeploymentIdentity,
        loaded: ExactDeploymentIdentity,
        calls: Vec<Op>,
        fail_once: Option<Op>,
        candidate_verify_count: usize,
        incumbent_verify_count: usize,
    }

    impl FakeRuntime {
        fn new(contract: PromotionContract) -> Self {
            Self {
                db: contract.incumbent.clone(),
                pointer: contract.incumbent.clone(),
                loaded: contract.incumbent.clone(),
                contract,
                calls: Vec::new(),
                fail_once: None,
                candidate_verify_count: 0,
                incumbent_verify_count: 0,
            }
        }

        fn with_failure(contract: PromotionContract, op: Op) -> Self {
            let mut runtime = Self::new(contract);
            runtime.fail_once = Some(op);
            runtime
        }

        fn call(&mut self, op: Op) -> Result<(), PromotionRuntimeError> {
            self.calls.push(op);
            if self.fail_once == Some(op) {
                self.fail_once = None;
                return Err(PromotionRuntimeError::new(format!("FAULT_{op:?}")));
            }
            Ok(())
        }
    }

    impl PromotionRuntime for FakeRuntime {
        fn set_database_champion(&mut self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError> {
            let op = if *identity == self.contract.candidate { Op::SetCandidateDb } else { Op::SetIncumbentDb };
            self.call(op)?;
            self.db = identity.clone();
            Ok(())
        }

        fn publish_champion_pointer(
            &mut self,
            identity: &ExactDeploymentIdentity,
        ) -> Result<(), PromotionRuntimeError> {
            let op = if *identity == self.contract.candidate { Op::CandidatePointer } else { Op::IncumbentPointer };
            self.call(op)?;
            self.pointer = identity.clone();
            Ok(())
        }

        fn restart_champion(&mut self) -> Result<(), PromotionRuntimeError> {
            let candidate = self.pointer == self.contract.candidate;
            self.call(if candidate { Op::CandidateRestart } else { Op::IncumbentRestart })?;
            self.loaded = self.pointer.clone();
            Ok(())
        }

        fn verify_exact_and_canary(
            &mut self,
            identity: &ExactDeploymentIdentity,
            canary: &CanaryIdentity,
        ) -> Result<(), PromotionRuntimeError> {
            let candidate = *identity == self.contract.candidate;
            let op = if candidate {
                self.candidate_verify_count += 1;
                if self.candidate_verify_count == 1 {
                    Op::CandidateVerify
                } else {
                    Op::CandidateFinalVerify
                }
            } else {
                self.incumbent_verify_count += 1;
                if self.incumbent_verify_count == 1 {
                    Op::IncumbentVerify
                } else {
                    Op::IncumbentFinalVerify
                }
            };
            self.call(op)?;
            if self.loaded != *identity || canary.expected_model != *identity {
                return Err(PromotionRuntimeError::new("EXACT_IDENTITY_OR_CANARY_MISMATCH"));
            }
            Ok(())
        }
    }

    struct CrashAfter {
        phase: PromotionPhase,
        fired: bool,
    }

    impl PromotionCheckpoint for CrashAfter {
        fn after_persisted_phase(&mut self, phase: PromotionPhase) -> Result<(), PromotionSagaError> {
            if !self.fired && phase == self.phase {
                self.fired = true;
                return Err(PromotionSagaError::interrupted(phase));
            }
            Ok(())
        }
    }

    #[test]
    fn happy_path_requires_two_exact_canary_checks_before_success() {
        let contract = contract();
        let mut journal = FakeJournal::default();
        let mut runtime = FakeRuntime::new(contract.clone());
        let outcome = start_promotion(&mut journal, &mut runtime, "promotion-1", "key-1", contract.clone()).unwrap();
        assert_eq!(outcome, PromotionOutcome::Promoted { identity: contract.candidate.clone() });
        assert_eq!(runtime.db, contract.candidate);
        assert_eq!(runtime.pointer, runtime.db);
        assert_eq!(runtime.loaded, runtime.db);
        assert_eq!(runtime.candidate_verify_count, 2);
        let job = journal.jobs.get("promotion-1").unwrap();
        assert_eq!(job.state, JobState::Succeeded);
        assert_eq!(decode_payload(&job.payload_json).unwrap().phase, PromotionPhase::Completed);
    }

    #[test]
    fn crash_after_every_candidate_phase_resumes_idempotently_without_false_success() {
        let phases = [
            PromotionPhase::Prepared,
            PromotionPhase::CandidateDbActivated,
            PromotionPhase::CandidatePointerPublished,
            PromotionPhase::CandidateRestarted,
            PromotionPhase::CandidateVerified,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let contract = contract();
            let mut journal = FakeJournal::default();
            let mut runtime = FakeRuntime::new(contract.clone());
            let mut crash = CrashAfter { phase, fired: false };
            let id = format!("candidate-crash-{index}");
            let first = start_promotion_with_checkpoint(
                &mut journal,
                &mut runtime,
                &mut crash,
                &id,
                &format!("candidate-key-{index}"),
                contract.clone(),
            );
            assert_eq!(first.unwrap_err().code, PROMOTION_INTERRUPTED, "phase {phase:?}");
            assert_eq!(journal.jobs.get(&id).unwrap().state, JobState::Running);

            let outcome = resume_promotion(&mut journal, &mut runtime, &id).unwrap();
            assert_eq!(outcome, PromotionOutcome::Promoted { identity: contract.candidate.clone() });
            assert_eq!(runtime.db, contract.candidate);
            assert_eq!(runtime.pointer, runtime.db);
            assert_eq!(runtime.loaded, runtime.db);
        }
    }

    #[test]
    fn crash_after_atomic_success_is_observed_as_terminal_on_recovery() {
        let contract = contract();
        let mut journal = FakeJournal::default();
        let mut runtime = FakeRuntime::new(contract.clone());
        let mut crash = CrashAfter { phase: PromotionPhase::Completed, fired: false };
        let first = start_promotion_with_checkpoint(
            &mut journal,
            &mut runtime,
            &mut crash,
            "terminal-success",
            "terminal-success-key",
            contract,
        );
        assert_eq!(first.unwrap_err().code, PROMOTION_INTERRUPTED);
        assert_eq!(journal.jobs.get("terminal-success").unwrap().state, JobState::Succeeded);
        assert_eq!(
            resume_promotion(&mut journal, &mut runtime, "terminal-success").unwrap(),
            PromotionOutcome::AlreadyTerminal { state: JobState::Succeeded, error_code: None }
        );
    }

    #[test]
    fn candidate_failure_runs_full_verified_rollback_and_never_reports_promoted() {
        for failed_op in [
            Op::SetCandidateDb,
            Op::CandidatePointer,
            Op::CandidateRestart,
            Op::CandidateVerify,
            Op::CandidateFinalVerify,
        ] {
            let contract = contract();
            let mut journal = FakeJournal::default();
            let mut runtime = FakeRuntime::with_failure(contract.clone(), failed_op);
            let outcome = start_promotion(
                &mut journal,
                &mut runtime,
                &format!("failure-{failed_op:?}"),
                &format!("failure-key-{failed_op:?}"),
                contract.clone(),
            )
            .unwrap();
            assert!(matches!(outcome, PromotionOutcome::RolledBack { .. }), "op {failed_op:?}");
            assert_eq!(runtime.db, contract.incumbent, "op {failed_op:?}");
            assert_eq!(runtime.pointer, runtime.db, "op {failed_op:?}");
            assert_eq!(runtime.loaded, runtime.db, "op {failed_op:?}");
            assert_eq!(runtime.incumbent_verify_count, 2, "op {failed_op:?}");
            let job = journal.jobs.values().next().unwrap();
            assert_eq!(job.state, JobState::Failed);
            assert_eq!(job.error_code.as_deref(), Some(PROMOTION_FAILED));
            assert_eq!(decode_payload(&job.payload_json).unwrap().phase, PromotionPhase::RollbackVerified);
        }
    }

    #[test]
    fn crash_after_every_rollback_phase_resumes_the_rollback_idempotently() {
        let phases = [
            PromotionPhase::RollbackPending,
            PromotionPhase::RollbackDbRestored,
            PromotionPhase::RollbackPointerRestored,
            PromotionPhase::RollbackRestarted,
            PromotionPhase::RollbackVerified,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let contract = contract();
            let mut journal = FakeJournal::default();
            let mut runtime = FakeRuntime::with_failure(contract.clone(), Op::CandidateVerify);
            let mut crash = CrashAfter { phase, fired: false };
            let id = format!("rollback-crash-{index}");
            let first = start_promotion_with_checkpoint(
                &mut journal,
                &mut runtime,
                &mut crash,
                &id,
                &format!("rollback-key-{index}"),
                contract.clone(),
            );
            assert_eq!(first.unwrap_err().code, PROMOTION_INTERRUPTED, "phase {phase:?}");
            assert_eq!(journal.jobs.get(&id).unwrap().state, JobState::Running);

            let outcome = resume_promotion(&mut journal, &mut runtime, &id).unwrap();
            assert!(matches!(outcome, PromotionOutcome::RolledBack { .. }), "phase {phase:?}");
            assert_eq!(runtime.db, contract.incumbent);
            assert_eq!(runtime.pointer, runtime.db);
            assert_eq!(runtime.loaded, runtime.db);
        }
    }

    #[test]
    fn any_rollback_callback_failure_is_terminal_rollback_unverified() {
        for failed_op in [
            Op::SetIncumbentDb,
            Op::IncumbentPointer,
            Op::IncumbentRestart,
            Op::IncumbentVerify,
            Op::IncumbentFinalVerify,
        ] {
            let contract = contract();
            let mut journal = FakeJournal::default();
            let mut runtime = FakeRuntime::new(contract.clone());
            // Force candidate verification to fail first, then arm the rollback failure from a small
            // runtime wrapper state change after the candidate has reached its verification callback.
            runtime.fail_once = Some(Op::CandidateVerify);
            let id = format!("unverified-{failed_op:?}");

            // Run until rollback pending, then interrupt there so the rollback-specific fault can be armed.
            let mut crash = CrashAfter { phase: PromotionPhase::RollbackPending, fired: false };
            let interrupted = start_promotion_with_checkpoint(
                &mut journal,
                &mut runtime,
                &mut crash,
                &id,
                &format!("unverified-key-{failed_op:?}"),
                contract,
            );
            assert_eq!(interrupted.unwrap_err().code, PROMOTION_INTERRUPTED);
            runtime.fail_once = Some(failed_op);

            let error = resume_promotion(&mut journal, &mut runtime, &id).unwrap_err();
            assert_eq!(error.code, ROLLBACK_UNVERIFIED, "op {failed_op:?}");
            let job = journal.jobs.get(&id).unwrap();
            assert_eq!(job.state, JobState::Failed);
            assert_eq!(job.error_code.as_deref(), Some(ROLLBACK_UNVERIFIED));
            let payload = decode_payload(&job.payload_json).unwrap();
            assert_eq!(payload.phase, PromotionPhase::RollbackUnverified);
            assert!(payload.failure.unwrap().rollback_code.is_some());
        }
    }

    #[test]
    fn crash_after_atomic_rollback_unverified_keeps_stable_terminal_code() {
        let contract = contract();
        let mut journal = FakeJournal::default();
        let mut runtime = FakeRuntime::with_failure(contract.clone(), Op::CandidateVerify);
        let mut pause = CrashAfter { phase: PromotionPhase::RollbackPending, fired: false };
        let first = start_promotion_with_checkpoint(
            &mut journal,
            &mut runtime,
            &mut pause,
            "terminal-rollback",
            "terminal-rollback-key",
            contract,
        );
        assert_eq!(first.unwrap_err().code, PROMOTION_INTERRUPTED);

        runtime.fail_once = Some(Op::SetIncumbentDb);
        let mut terminal_crash = CrashAfter { phase: PromotionPhase::RollbackUnverified, fired: false };
        let second =
            resume_promotion_with_checkpoint(&mut journal, &mut runtime, &mut terminal_crash, "terminal-rollback");
        assert_eq!(second.unwrap_err().code, PROMOTION_INTERRUPTED);
        let terminal = resume_promotion(&mut journal, &mut runtime, "terminal-rollback").unwrap();
        assert_eq!(
            terminal,
            PromotionOutcome::AlreadyTerminal {
                state: JobState::Failed,
                error_code: Some(ROLLBACK_UNVERIFIED.to_string())
            }
        );
    }

    #[test]
    fn idempotency_key_cannot_be_reused_for_different_evidence_or_candidate() {
        let contract = contract();
        let mut journal = FakeJournal::default();
        let mut runtime = FakeRuntime::new(contract.clone());
        start_promotion(&mut journal, &mut runtime, "idem-1", "same-key", contract.clone()).unwrap();
        let mut different = contract;
        different.verdict_sha256 = sha('f');
        let error = start_promotion(&mut journal, &mut runtime, "idem-2", "same-key", different).unwrap_err();
        assert_eq!(error.code, "PROMOTION_CONTRACT_INVALID");
    }

    #[test]
    fn payload_rejects_unknown_fields_bad_hashes_and_canary_model_drift() {
        let mut value = serde_json::to_value(PromotionPayload::prepared(contract()).unwrap()).unwrap();
        value.as_object_mut().unwrap().insert("surprise".to_string(), serde_json::json!(true));
        assert!(decode_payload(&serde_json::to_string(&value).unwrap()).is_err());

        let mut bad = contract();
        bad.scorecard_sha256 = "ABC".to_string();
        assert_eq!(PromotionPayload::prepared(bad).unwrap_err().code, "PROMOTION_CONTRACT_INVALID");

        let mut drift = contract();
        drift.canary.expected_model = drift.incumbent.clone();
        assert_eq!(PromotionPayload::prepared(drift).unwrap_err().code, "PROMOTION_CONTRACT_INVALID");
        assert_eq!(durable_runtime_code("exception at C:\\secret path"), "RUNTIME_ERROR_INVALID_CODE");
    }

    #[test]
    fn terminal_state_cannot_claim_success_over_a_nonterminal_or_tampered_payload() {
        let contract = contract();
        let payload = PromotionPayload::prepared(contract).unwrap();
        let job = PayloadJob {
            id: "tampered-terminal".to_string(),
            kind: MODEL_PROMOTION_JOB_KIND.to_string(),
            state: JobState::Succeeded,
            idempotency_key: Some("tampered-terminal-key".to_string()),
            error_code: None,
            payload_json: encode_payload(&payload).unwrap(),
        };
        assert_eq!(terminal_outcome(&job).unwrap_err().code, "PROMOTION_CONTRACT_INVALID");
    }

    #[test]
    fn sqlite_startup_inventory_survives_generic_orphan_reaping_and_resumes() {
        let mut database = crate::db::Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        let contract = contract();
        let payload = PromotionPayload::prepared(contract.clone()).unwrap();
        let payload_json = encode_payload(&payload).unwrap();
        let begun = database
            .begin_running_payload_job(
                "sqlite-promotion",
                MODEL_PROMOTION_JOB_KIND,
                "sqlite-promotion-key",
                &payload_json,
            )
            .unwrap();
        assert!(begun.created);

        database.create_or_get_job("ordinary-running", "export", None, None).unwrap();
        database.transition_job("ordinary-running", JobState::Running, None).unwrap();
        assert_eq!(database.mark_orphaned_running_jobs_failed().unwrap(), 1);
        assert_eq!(database.get_job("ordinary-running").unwrap().unwrap().state, JobState::Failed);

        let recoverable = running_promotions(&database).unwrap();
        assert_eq!(recoverable.len(), 1);
        assert_eq!(recoverable[0].id, "sqlite-promotion");
        assert_eq!(recoverable[0].payload_json, payload_json);

        let mut runtime = FakeRuntime::new(contract.clone());
        let outcome = resume_promotion(&mut database, &mut runtime, "sqlite-promotion").unwrap();
        assert_eq!(outcome, PromotionOutcome::Promoted { identity: contract.candidate });
        assert_eq!(database.get_job("sqlite-promotion").unwrap().unwrap().state, JobState::Succeeded);
        assert!(running_promotions(&database).unwrap().is_empty());
    }
}
