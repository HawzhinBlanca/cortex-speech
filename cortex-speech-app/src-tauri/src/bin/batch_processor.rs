//! Retired legacy writer.
//!
//! This binary previously opened the production database and rewrote drafts with a non-champion model
//! while stamping incorrect fixed provenance. Keeping the executable as a fail-closed tombstone
//! gives old scheduled jobs a clear error without leaving any reachable database or ASR code.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("HARD STOP: batch_processor is retired because it cannot produce champion-grade, exact-provenance drafts. Use the app's batch_transcribe path (pinned OmniASR-7B champion) instead.".into())
}
