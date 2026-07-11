//! App-owned supervision POLICY for the OmniASR-7B WSL server (audit P0 #4).
//!
//! This is a pure, deterministic state machine that decides WHEN to (re)start the champion engine —
//! bounded exponential backoff between attempts, and a circuit breaker that stops hammering a
//! persistently-failing server and eventually gives up (requiring a manual start). It holds NO I/O:
//! the caller drives it with the real health probe (`pipeline::probe_wsl_7b_server`) and starter
//! (`commands::start_champion_engine`), reporting each outcome via `on_healthy` / `on_failure`. Time
//! is passed in as a monotonic `Duration` since the supervisor started, so every transition is unit-
//! testable without sleeping or touching a clock.
//!
//! Live WSL spawn/restart wiring is owner-machine (a background task calling `decide()` on a tick);
//! this module + its tests are the verifiable-here core.

use std::time::Duration;

/// Circuit-breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breaker {
    /// Normal: (re)starts allowed, subject to backoff.
    Closed,
    /// Too many consecutive failures — stop trying until the cooldown elapses.
    Open,
    /// Cooldown elapsed — allow exactly ONE probe/restart. Success closes; failure re-opens (a trip).
    HalfOpen,
    /// Gave up after `max_trips` breaker openings — only a manual start (`reset`) resumes supervision.
    GaveUp,
}

/// Tunables. Defaults are sized for a ~30 GB WSL model server (slow to warm; don't hammer it).
#[derive(Debug, Clone, Copy)]
pub struct SupervisorPolicy {
    /// Consecutive failures that trip the breaker Closed → Open.
    pub max_consecutive_failures: u32,
    /// First retry delay; doubles each consecutive failure, capped at `max_backoff`.
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// How long the breaker stays Open before allowing a HalfOpen probe.
    pub open_cooldown: Duration,
    /// Breaker openings before giving up entirely (→ GaveUp; manual start required).
    pub max_trips: u32,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 3,
            base_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            open_cooldown: Duration::from_secs(120),
            max_trips: 5,
        }
    }
}

/// What the supervisor wants the caller to do right now (engine is NOT currently healthy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// (Re)start the engine now, then call `on_restart_attempt` followed by on_healthy/on_failure.
    Restart,
    /// Not yet — wait this long (backoff between attempts), then decide again.
    Wait(Duration),
    /// Breaker is Open — cooling down for this long before a probe. Surface "Offline (recovering)".
    Cooldown(Duration),
    /// Gave up — a manual start is required. Surface an actionable "engine failed to start" state.
    GaveUp,
}

pub struct EngineSupervisor {
    policy: SupervisorPolicy,
    breaker: Breaker,
    consecutive_failures: u32,
    trips: u32,
    total_restarts: u32,
    last_attempt_at: Option<Duration>,
    open_until: Option<Duration>,
}

impl EngineSupervisor {
    pub fn new(policy: SupervisorPolicy) -> Self {
        Self {
            policy,
            breaker: Breaker::Closed,
            consecutive_failures: 0,
            trips: 0,
            total_restarts: 0,
            last_attempt_at: None,
            open_until: None,
        }
    }

    pub fn breaker(&self) -> Breaker {
        self.breaker
    }
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
    pub fn total_restarts(&self) -> u32 {
        self.total_restarts
    }
    pub fn trips(&self) -> u32 {
        self.trips
    }

    /// The engine is confirmed healthy (a probe/start succeeded): clear all failure state.
    pub fn on_healthy(&mut self) {
        self.consecutive_failures = 0;
        self.breaker = Breaker::Closed;
        self.open_until = None;
    }

    /// A start/probe FAILED at `now`. Grows backoff; trips the breaker at the failure cap or when a
    /// HalfOpen probe fails; gives up after `max_trips` trips.
    pub fn on_failure(&mut self, now: Duration) {
        if self.breaker == Breaker::GaveUp {
            return;
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_attempt_at = Some(now);
        let should_trip =
            self.breaker == Breaker::HalfOpen || self.consecutive_failures >= self.policy.max_consecutive_failures;
        if should_trip {
            self.trips = self.trips.saturating_add(1);
            if self.trips >= self.policy.max_trips {
                self.breaker = Breaker::GaveUp;
                self.open_until = None;
            } else {
                self.breaker = Breaker::Open;
                self.open_until = Some(now.saturating_add(self.policy.open_cooldown));
            }
        }
    }

    /// Record that a (re)start was just issued at `now`. An Open breaker moves to HalfOpen — this
    /// attempt IS the half-open probe.
    pub fn on_restart_attempt(&mut self, now: Duration) {
        self.total_restarts = self.total_restarts.saturating_add(1);
        self.last_attempt_at = Some(now);
        if self.breaker == Breaker::Open {
            self.breaker = Breaker::HalfOpen;
        }
    }

    /// Manual start / operator reset — resume supervision from a clean slate.
    pub fn reset(&mut self) {
        self.breaker = Breaker::Closed;
        self.consecutive_failures = 0;
        self.trips = 0;
        self.open_until = None;
        self.last_attempt_at = None;
    }

    /// Exponential backoff for the current consecutive-failure count: base * 2^(failures-1), capped.
    pub fn current_backoff(&self) -> Duration {
        if self.consecutive_failures == 0 {
            return Duration::ZERO;
        }
        let shift = self.consecutive_failures.saturating_sub(1).min(32);
        let mult = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let base_ms = self.policy.base_backoff.as_millis() as u64;
        let ms = base_ms.saturating_mul(mult).min(self.policy.max_backoff.as_millis() as u64);
        Duration::from_millis(ms)
    }

    /// Decide what to do at `now`, given the engine is NOT currently healthy.
    pub fn decide(&self, now: Duration) -> Decision {
        match self.breaker {
            Breaker::GaveUp => Decision::GaveUp,
            Breaker::Open => {
                let until = self.open_until.unwrap_or(now);
                if now >= until {
                    Decision::Restart // cooldown elapsed → the next attempt is the HalfOpen probe
                } else {
                    Decision::Cooldown(until.saturating_sub(now))
                }
            }
            // A HalfOpen probe is in flight; hold until its outcome is reported.
            Breaker::HalfOpen => Decision::Wait(self.policy.base_backoff),
            Breaker::Closed => {
                let backoff = self.current_backoff();
                match self.last_attempt_at {
                    Some(t) if now < t.saturating_add(backoff) => {
                        Decision::Wait(t.saturating_add(backoff).saturating_sub(now))
                    }
                    _ => Decision::Restart,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn policy() -> SupervisorPolicy {
        SupervisorPolicy {
            max_consecutive_failures: 3,
            base_backoff: secs(2),
            max_backoff: secs(60),
            open_cooldown: secs(120),
            max_trips: 3,
        }
    }

    #[test]
    fn fresh_supervisor_wants_a_restart_immediately() {
        let s = EngineSupervisor::new(policy());
        assert_eq!(s.decide(secs(0)), Decision::Restart);
        assert_eq!(s.breaker(), Breaker::Closed);
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let mut s = EngineSupervisor::new(policy());
        // 1 failure -> base (2s); 2 -> 4s; capped at max_backoff (60s) eventually.
        s.on_failure(secs(0));
        assert_eq!(s.current_backoff(), secs(2));
        s.on_failure(secs(0));
        assert_eq!(s.current_backoff(), secs(4));
        // Force many failures via reset-free direct calls would trip the breaker; test the cap in
        // isolation with a high failure count on a breaker-immune policy.
        let mut hi = EngineSupervisor::new(SupervisorPolicy { max_consecutive_failures: 1000, ..policy() });
        for _ in 0..20 {
            hi.on_failure(secs(0));
        }
        assert_eq!(hi.current_backoff(), secs(60), "backoff must cap at max_backoff");
    }

    #[test]
    fn decide_waits_during_backoff_then_restarts() {
        let mut s = EngineSupervisor::new(policy());
        s.on_restart_attempt(secs(0));
        s.on_failure(secs(0)); // 1 failure -> 2s backoff from t=0
        assert_eq!(s.decide(secs(1)), Decision::Wait(secs(1))); // 1s left
        assert_eq!(s.decide(secs(2)), Decision::Restart); // backoff elapsed
    }

    #[test]
    fn breaker_opens_after_max_consecutive_failures_and_cools_down() {
        let mut s = EngineSupervisor::new(policy());
        s.on_failure(secs(0));
        s.on_failure(secs(0));
        assert_eq!(s.breaker(), Breaker::Closed);
        s.on_failure(secs(10)); // third consecutive failure -> trip
        assert_eq!(s.breaker(), Breaker::Open);
        assert_eq!(s.trips(), 1);
        // Cooldown = 120s from t=10 -> open_until = 130.
        assert_eq!(s.decide(secs(30)), Decision::Cooldown(secs(100)));
        assert_eq!(s.decide(secs(130)), Decision::Restart); // cooldown elapsed
    }

    #[test]
    fn half_open_success_closes_the_breaker() {
        let mut s = EngineSupervisor::new(policy());
        for _ in 0..3 {
            s.on_failure(secs(0));
        }
        assert_eq!(s.breaker(), Breaker::Open);
        s.on_restart_attempt(secs(120)); // the half-open probe
        assert_eq!(s.breaker(), Breaker::HalfOpen);
        s.on_healthy();
        assert_eq!(s.breaker(), Breaker::Closed);
        assert_eq!(s.consecutive_failures(), 0);
        assert_eq!(s.decide(secs(200)), Decision::Restart); // healthy path handled by caller; if down, retry
    }

    #[test]
    fn half_open_failure_reopens_and_counts_a_trip() {
        let mut s = EngineSupervisor::new(policy());
        for _ in 0..3 {
            s.on_failure(secs(0));
        }
        assert_eq!(s.trips(), 1);
        s.on_restart_attempt(secs(120));
        assert_eq!(s.breaker(), Breaker::HalfOpen);
        s.on_failure(secs(125)); // the half-open probe failed
        assert_eq!(s.breaker(), Breaker::Open);
        assert_eq!(s.trips(), 2);
    }

    #[test]
    fn gives_up_after_max_trips() {
        let mut s = EngineSupervisor::new(policy()); // max_trips = 3
                                                     // Trip 1: 3 consecutive failures.
        for _ in 0..3 {
            s.on_failure(secs(0));
        }
        // Trips 2 and 3: each a half-open probe that fails.
        s.on_restart_attempt(secs(120));
        s.on_failure(secs(121)); // trip 2
        s.on_restart_attempt(secs(300));
        s.on_failure(secs(301)); // trip 3 -> GaveUp
        assert_eq!(s.breaker(), Breaker::GaveUp);
        assert_eq!(s.decide(secs(10_000)), Decision::GaveUp);
        // Further failures are inert once given up.
        s.on_failure(secs(11_000));
        assert_eq!(s.breaker(), Breaker::GaveUp);
    }

    #[test]
    fn reset_resumes_supervision_from_clean_state() {
        let mut s = EngineSupervisor::new(policy());
        for _ in 0..3 {
            s.on_failure(secs(0));
        }
        s.on_restart_attempt(secs(120));
        s.on_failure(secs(121));
        s.on_restart_attempt(secs(300));
        s.on_failure(secs(301)); // GaveUp
        assert_eq!(s.breaker(), Breaker::GaveUp);
        s.reset();
        assert_eq!(s.breaker(), Breaker::Closed);
        assert_eq!(s.trips(), 0);
        assert_eq!(s.consecutive_failures(), 0);
        assert_eq!(s.decide(secs(301)), Decision::Restart);
    }

    #[test]
    fn on_healthy_clears_backoff_so_a_later_drop_retries_immediately() {
        let mut s = EngineSupervisor::new(policy());
        s.on_restart_attempt(secs(0));
        s.on_failure(secs(0));
        s.on_healthy(); // engine came up
                        // Later it drops again — no stale backoff should delay the first retry.
        assert_eq!(s.decide(secs(500)), Decision::Restart);
    }
}
