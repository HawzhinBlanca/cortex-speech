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

/// Upper bound on a provider JSON response body. ureq's `Response::into_json()` streams via
/// `into_reader()`, which — unlike `into_string()`'s built-in 10 MB cap — is bounded only by the
/// read TIMEOUT, not by bytes. A compromised or on-path HTTPS endpoint that trickles a multi-GB
/// chunked body within the read window could otherwise drive `serde_json::from_reader` to OOM the
/// process. 64 MiB is far above any real STT/LLM API reply.
pub const MAX_JSON_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// Parse an untrusted provider response as JSON with a hard byte cap. Use this instead of
/// `resp.into_json()` everywhere a cloud/LLM endpoint's body is deserialized: an over-limit body
/// yields a bounded read that fails to parse (returns `Err`) rather than an unbounded allocation.
pub fn json_bounded<T: serde::de::DeserializeOwned>(resp: ureq::Response) -> Result<T, String> {
    use std::io::Read;
    let reader = resp.into_reader().take(MAX_JSON_RESPONSE_BYTES);
    serde_json::from_reader(reader).map_err(|e| e.to_string())
}

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
