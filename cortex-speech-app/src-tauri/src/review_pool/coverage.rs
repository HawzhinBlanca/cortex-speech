//! Per-voice review coverage report over the live pool (`pool_admin status`, certification).
//! Pure read-side derivation from the same authorities the queue uses; it decides nothing.

use super::{derive_resolution, load, owner_adjudications_on, reviewer_sets, DerivedResolution};
use crate::db::Database;
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCoverage {
    pub voice_name: String,
    pub total_clips: usize,
    pub zero_reviews: usize,
    pub one_review: usize,
    pub two_reviews: usize,
    pub three_or_more_reviews: usize,
    pub resolved: usize,
    pub needs_third_review: usize,
    pub owner_conflicts: usize,
}

pub fn coverage_by_voice(db: &Database) -> Result<Vec<VoiceCoverage>, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let reviewers = reviewer_sets(db)?;
    let adjudications = owner_adjudications_on(db.connection())?;
    let mut by_voice: HashMap<String, VoiceCoverage> = HashMap::new();
    for (segment_id, evidence) in pool.members.iter() {
        let reviews = reviewers.get(segment_id).map_or(0, |value| value.judged.len());
        let entry = by_voice.entry(evidence.voice_name.clone()).or_insert_with(|| VoiceCoverage {
            voice_name: evidence.voice_name.clone(),
            total_clips: 0,
            zero_reviews: 0,
            one_review: 0,
            two_reviews: 0,
            three_or_more_reviews: 0,
            resolved: 0,
            needs_third_review: 0,
            owner_conflicts: 0,
        });
        entry.total_clips += 1;
        match reviews {
            0 => entry.zero_reviews += 1,
            1 => entry.one_review += 1,
            2 => entry.two_reviews += 1,
            _ => entry.three_or_more_reviews += 1,
        }
        let (resolution, _) = derive_resolution(segment_id, reviewers.get(segment_id), adjudications.get(segment_id));
        match resolution {
            DerivedResolution::Resolved { .. } => entry.resolved += 1,
            DerivedResolution::NeedsThird => entry.needs_third_review += 1,
            DerivedResolution::OwnerConflict => entry.owner_conflicts += 1,
            DerivedResolution::Pending => {}
        }
    }
    let mut rows: Vec<VoiceCoverage> = by_voice.into_values().collect();
    rows.sort_unstable_by(|left, right| left.voice_name.cmp(&right.voice_name));
    Ok(rows)
}
