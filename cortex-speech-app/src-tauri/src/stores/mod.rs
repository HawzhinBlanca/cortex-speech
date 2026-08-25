//! Database domain stores.
//!
//! Stores own database interaction for one domain and deliberately have no Tauri or HTTP surface.

mod segment_query;

pub(crate) use segment_query::SegmentQueryStore;
