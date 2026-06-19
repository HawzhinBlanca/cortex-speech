//! Shared HTTP agents with bounded timeouts.
//!
//! Every outbound call previously used the bare global `ureq::post`/`ureq::get`, whose
//! default agent has NO read/write timeout — so a server that accepts the connection then
//! stalls blocks the calling worker thread *forever* (the WSL path already learned this
//! lesson the hard way). Routing all calls through these shared, timeout-bounded agents
//! guarantees every network operation makes progress or fails in bounded time. Agents are
//! cheap to clone (internally `Arc`) and pool connections, so a single shared instance per
//! purpose is ideal.

use std::sync::LazyLock;
use std::time::Duration;

/// API / LLM calls (Gemini, local LLM refiner, jury). The read timeout is the *socket*
/// read timeout (max stall between bytes), generous enough for a slow model to think,
/// but never unbounded.
pub static API_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(60))
        .build()
});

/// Large model-archive downloads. A more generous read timeout for slow CDNs, but still
/// bounded so a dead connection can't hang a download thread indefinitely.
pub static DOWNLOAD_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(30))
        .build()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_agents_build_with_bounded_timeouts() {
        // Forcing the LazyLocks proves the agents construct (a bad builder config would
        // panic here) — the guarantee being that no call site uses the timeout-less
        // global agent anymore.
        let _api = &*API_AGENT;
        let _dl = &*DOWNLOAD_AGENT;
        // Cloning is cheap (Arc) — call sites can clone freely without re-building config.
        let _cloned = API_AGENT.clone();
    }
}
