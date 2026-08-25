//! Database domain stores.
//!
//! Stores own database interaction for one domain and deliberately have no Tauri or HTTP surface.

mod playback;
mod review_draft;
mod review_write;
mod rights;
mod segment_query;

pub(crate) use playback::{PlaybackObservation, PlaybackWriteStore};
pub(crate) use review_draft::{ReviewDraftRecord, ReviewDraftStore};
#[cfg(test)]
pub(crate) use review_write::require_listened;
pub(crate) use review_write::{ReviewCommitError, ReviewWriteStore};
pub(crate) use rights::RightsStore;
pub(crate) use segment_query::SegmentQueryStore;
