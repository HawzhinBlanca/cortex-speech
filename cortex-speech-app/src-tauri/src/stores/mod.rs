//! Database domain stores.
//!
//! Stores own database interaction for one domain and deliberately have no Tauri or HTTP surface.

mod review_draft;
mod segment_query;

pub(crate) use review_draft::{ReviewDraftRecord, ReviewDraftStore};
pub(crate) use segment_query::SegmentQueryStore;
