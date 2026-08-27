//! Database domain stores.
//!
//! Stores own database interaction for one domain and deliberately have no Tauri or HTTP surface.

mod import_write;
mod jobs;
#[cfg(test)]
mod playback;
mod review_draft;
mod review_write;
mod rights;
mod segment_query;
mod segment_write;

pub(crate) use import_write::ImportWriteStore;
pub(crate) use jobs::JobStore;
pub(crate) use review_draft::{ReviewDraftRecord, ReviewDraftStore};
#[cfg(test)]
pub(crate) use review_write::require_listened;
pub(crate) use review_write::{ReviewCommitError, ReviewWriteStore, TechnicalUnusableCommitError};
pub(crate) use rights::RightsStore;
pub(crate) use segment_query::SegmentQueryStore;
pub(crate) use segment_write::{SegmentMetadataChange, SegmentMetadataUpdateError, SegmentWriteStore};
