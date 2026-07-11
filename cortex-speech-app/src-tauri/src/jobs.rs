//! Durable job lifecycle for the persistent Job Supervisor (audit P0 #3).
//!
//! A long operation (import, transcribe, export, backup, eval, ...) gets a row in the `jobs` table
//! (migration v37) so it survives an app crash/restart instead of vanishing with a detached thread.
//! This module owns the PURE part — the state machine — so the transition rules are unit-tested
//! without a database. The DB read/write glue and the actual op wiring come in a later increment.

use std::fmt;

/// The lifecycle of a durable job. String forms match the `jobs.state` CHECK constraint in
/// migration v37 exactly — `as_str`/`parse` are the single source of truth for that mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Persisted but not yet picked up by a worker.
    Queued,
    /// A worker is actively processing it.
    Running,
    /// Finished successfully — terminal.
    Succeeded,
    /// Failed with an `error_code` — terminal.
    Failed,
    /// Cancelled by the user or on shutdown — terminal.
    Cancelled,
}

impl JobState {
    /// The exact token stored in `jobs.state`. Must stay in lockstep with the migration v37 CHECK list.
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }

    /// Parse a `jobs.state` token back into the enum. Returns `None` for anything the CHECK
    /// constraint would have rejected — an unknown value in the DB is a corruption signal, not a
    /// state to silently coerce.
    pub fn parse(s: &str) -> Option<JobState> {
        match s {
            "queued" => Some(JobState::Queued),
            "running" => Some(JobState::Running),
            "succeeded" => Some(JobState::Succeeded),
            "failed" => Some(JobState::Failed),
            "cancelled" => Some(JobState::Cancelled),
            _ => None,
        }
    }

    /// A terminal state never transitions again — success, failure, and cancellation are final.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Succeeded | JobState::Failed | JobState::Cancelled)
    }

    /// Whether `self -> next` is a legal lifecycle move. The only legal edges:
    ///   Queued  -> Running | Cancelled
    ///   Running -> Succeeded | Failed | Cancelled
    /// Terminal states have no outgoing edges (including self-loops), so a double-complete or a
    /// "resurrect a cancelled job" is rejected here rather than corrupting the record.
    pub fn can_transition_to(self, next: JobState) -> bool {
        matches!(
            (self, next),
            (JobState::Queued, JobState::Running)
                | (JobState::Queued, JobState::Cancelled)
                | (JobState::Running, JobState::Succeeded)
                | (JobState::Running, JobState::Failed)
                | (JobState::Running, JobState::Cancelled)
        )
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A durable job as read back from the `jobs` table (the columns the UI/supervisor actually need).
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub state: JobState,
    /// 0.0..=1.0 fraction complete.
    pub progress: f64,
    pub completed: i64,
    pub total: Option<i64>,
    /// Stable machine code on failure (e.g. `MODEL_UNAVAILABLE`), `None` while not failed.
    pub error_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [JobState; 5] =
        [JobState::Queued, JobState::Running, JobState::Succeeded, JobState::Failed, JobState::Cancelled];

    #[test]
    fn str_round_trips_for_every_state() {
        for s in ALL {
            assert_eq!(JobState::parse(s.as_str()), Some(s), "round-trip {s}");
        }
    }

    #[test]
    fn unknown_state_string_is_rejected() {
        // Anything outside the migration v37 CHECK list must not coerce to a valid state.
        for bad in ["", "QUEUED", "done", "pending", "run", "success"] {
            assert_eq!(JobState::parse(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn terminal_states_are_terminal() {
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
    }

    #[test]
    fn only_the_five_legal_edges_are_allowed() {
        let legal = [
            (JobState::Queued, JobState::Running),
            (JobState::Queued, JobState::Cancelled),
            (JobState::Running, JobState::Succeeded),
            (JobState::Running, JobState::Failed),
            (JobState::Running, JobState::Cancelled),
        ];
        for from in ALL {
            for to in ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "edge {from} -> {to} should be {}",
                    if expected { "legal" } else { "illegal" }
                );
            }
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_edges() {
        for from in [JobState::Succeeded, JobState::Failed, JobState::Cancelled] {
            for to in ALL {
                assert!(!from.can_transition_to(to), "terminal {from} must not reach {to}");
            }
        }
    }

    #[test]
    fn no_self_loops() {
        for s in ALL {
            assert!(!s.can_transition_to(s), "self-loop {s} -> {s} must be illegal");
        }
    }
}
