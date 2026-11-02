//! The compaction checkpoint message source (dsh checkpoint.ts 1:1).
//!
//! The correlated constructor and type every backend uses for its
//! replacement user message, plus the predicate that recognizes persisted
//! checkpoints. The dsh module is a pure type/value/predicate outlet; the fr
//! module keeps the same independence — no dougong imports, no session
//! vocabulary beyond the shared `MessageSource`.

use fr_wire::MessageSource;
use serde::{Serialize, Serializer};

use crate::brand::CompactionId;

/// The wire `kind` value identifying a compaction checkpoint source.
pub const COMPACT_CHECKPOINT_KIND: &str = "compact-checkpoint";

/// The checkpoint kind tag; serializes as the bare string
/// `"compact-checkpoint"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckpointTag;

impl Serialize for CheckpointTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(COMPACT_CHECKPOINT_KIND)
    }
}

/// Message source carried by a concrete compaction checkpoint.
///
/// dsh freezes instances (`Object.freeze`); fr instances are immutable by
/// construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionCheckpointSource {
    /// The kind marker; always `compact-checkpoint`.
    pub kind: CheckpointTag,
    /// Owning compaction identity.
    pub compaction_id: CompactionId,
    /// Initiating manual command, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_command_id: Option<String>,
}

/// Create a checkpoint source correlated with one compaction transaction.
#[must_use]
pub const fn compact_checkpoint_source(
    compaction_id: CompactionId,
    source_command_id: Option<String>,
) -> CompactionCheckpointSource {
    CompactionCheckpointSource {
        kind: CheckpointTag,
        compaction_id,
        source_command_id,
    }
}

/// Test whether a persisted message source identifies a compaction
/// checkpoint (dsh `source.kind === 'compact-checkpoint'`).
#[must_use]
pub fn is_compact_checkpoint_source(source: &MessageSource) -> bool {
    match source {
        MessageSource::Other(map) => matches!(
            map.get("kind"),
            Some(serde_json::Value::String(kind)) if kind == COMPACT_CHECKPOINT_KIND
        ),
        MessageSource::Known(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::KnownMessageSource;
    use serde_json::json;

    #[test]
    fn builds_the_correlated_marker() {
        let source = compact_checkpoint_source(CompactionId("c1".to_owned()), None);
        assert_eq!(
            serde_json::to_value(&source).expect("serialize"),
            json!({"kind": "compact-checkpoint", "compactionId": "c1"})
        );

        let commanded =
            compact_checkpoint_source(CompactionId("c2".to_owned()), Some("cmd-1".to_owned()));
        assert_eq!(
            serde_json::to_value(&commanded).expect("serialize"),
            json!({"kind": "compact-checkpoint", "compactionId": "c2", "sourceCommandId": "cmd-1"})
        );
    }

    #[test]
    fn recognizes_only_the_checkpoint_kind() {
        let source = compact_checkpoint_source(CompactionId("c1".to_owned()), None);
        let raw = serde_json::to_value(&source).expect("serialize");
        let source: MessageSource = serde_json::from_value(raw).expect("parse");
        assert!(is_compact_checkpoint_source(&source));

        let user: MessageSource = serde_json::from_value(json!({"kind": "user"})).expect("parse");
        assert!(!is_compact_checkpoint_source(&user));
        assert!(!is_compact_checkpoint_source(&MessageSource::Known(
            KnownMessageSource::User {
                extra: serde_json::Map::new(),
            }
        )));
    }
}
