//! Action-neutral desktop review Undo wire authority.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::TechnicalUnusableReasonV1;

/// The one active desktop review action the database can still reverse exactly after a renderer or
/// application restart. Effect ids are table-local, so every authority and outcome carries its
/// closed action kind as well as its immutable payload identity.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DesktopHumanDecisionV1 {
    Accept,
    Edit,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum DesktopReviewFlagKindV1 {
    #[serde(rename = "generic")]
    Generic,
    #[serde(rename = "technicalUnusable")]
    TechnicalUnusable { reason: TechnicalUnusableReasonV1 },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum DesktopReviewUndoTargetV1 {
    #[serde(rename = "decision")]
    Decision {
        #[serde(rename = "effectEventId")]
        effect_event_id: i64,
        #[serde(rename = "segmentId")]
        segment_id: String,
        decision: DesktopHumanDecisionV1,
        #[serde(rename = "sourceOperationId")]
        source_operation_id: String,
        #[serde(rename = "sourcePayloadHash")]
        source_payload_hash: String,
        #[serde(rename = "databaseGeneration")]
        database_generation: u64,
    },
    #[serde(rename = "flag")]
    Flag {
        #[serde(rename = "effectEventId")]
        effect_event_id: i64,
        #[serde(rename = "segmentId")]
        segment_id: String,
        #[serde(rename = "sourceOperationId")]
        source_operation_id: String,
        #[serde(rename = "sourcePayloadHash")]
        source_payload_hash: String,
        #[serde(rename = "priorRevision")]
        prior_revision: i64,
        #[serde(rename = "flagRevision")]
        flag_revision: i64,
        #[serde(rename = "flagKind")]
        flag_kind: DesktopReviewFlagKindV1,
        #[serde(rename = "databaseGeneration")]
        database_generation: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DesktopReviewUndoBlockReasonV1 {
    LegacyHistory,
    LatestDecisionUndone,
    LatestFlagUndone,
    DecisionShadowed,
    FlagShadowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "status")]
pub enum DesktopReviewUndoAvailabilityV1 {
    #[serde(rename = "available")]
    Available { target: DesktopReviewUndoTargetV1 },
    #[serde(rename = "none")]
    None,
    #[serde(rename = "blocked")]
    Blocked { reason: DesktopReviewUndoBlockReasonV1 },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UndoDesktopReviewRequestV1 {
    pub target: DesktopReviewUndoTargetV1,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DesktopReviewUndoEffectKindV1 {
    Decision,
    Flag,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "status")]
pub enum DesktopReviewUndoOutcomeV1 {
    #[serde(rename = "applied")]
    Applied {
        #[serde(rename = "effectKind")]
        effect_kind: DesktopReviewUndoEffectKindV1,
        #[serde(rename = "effectEventId")]
        effect_event_id: i64,
        #[serde(rename = "restoredRevision")]
        restored_revision: i64,
        segment: Box<crate::db::SpeechSegment>,
    },
    #[serde(rename = "alreadyApplied")]
    AlreadyApplied {
        #[serde(rename = "effectKind")]
        effect_kind: DesktopReviewUndoEffectKindV1,
        #[serde(rename = "effectEventId")]
        effect_event_id: i64,
    },
    #[serde(rename = "conflict")]
    Conflict {
        #[serde(rename = "effectKind")]
        effect_kind: DesktopReviewUndoEffectKindV1,
        #[serde(rename = "effectEventId")]
        effect_event_id: i64,
    },
}

impl DesktopReviewUndoOutcomeV1 {
    pub(crate) fn from_decision_database(effect_event_id: i64, value: crate::db::HumanDecisionUndoOutcome) -> Self {
        match value {
            crate::db::HumanDecisionUndoOutcome::Applied { restored_revision, segment } => Self::Applied {
                effect_kind: DesktopReviewUndoEffectKindV1::Decision,
                effect_event_id,
                restored_revision,
                segment: Box::new(segment),
            },
            crate::db::HumanDecisionUndoOutcome::AlreadyApplied { .. } => {
                Self::AlreadyApplied { effect_kind: DesktopReviewUndoEffectKindV1::Decision, effect_event_id }
            }
            crate::db::HumanDecisionUndoOutcome::Conflict { .. } => {
                Self::Conflict { effect_kind: DesktopReviewUndoEffectKindV1::Decision, effect_event_id }
            }
        }
    }

    pub(crate) fn from_flag_database(effect_event_id: i64, value: crate::db::HumanFlagUndoOutcome) -> Self {
        match value {
            crate::db::HumanFlagUndoOutcome::Applied { restored_revision, segment } => Self::Applied {
                effect_kind: DesktopReviewUndoEffectKindV1::Flag,
                effect_event_id,
                restored_revision,
                segment,
            },
            crate::db::HumanFlagUndoOutcome::AlreadyApplied => {
                Self::AlreadyApplied { effect_kind: DesktopReviewUndoEffectKindV1::Flag, effect_event_id }
            }
            crate::db::HumanFlagUndoOutcome::Conflict => {
                Self::Conflict { effect_kind: DesktopReviewUndoEffectKindV1::Flag, effect_event_id }
            }
        }
    }
}
