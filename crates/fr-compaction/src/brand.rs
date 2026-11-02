//! The compaction transaction identity (dsh compaction/src/brand.ts 1:1).

use serde::{Deserialize, Serialize};

/// Stable identity shared by one compact start/summary/checkpoint/end
/// transaction.
///
/// dsh brands an opaque implementation-minted string with no validation; the
/// fr newtype carries the same discipline — construction performs no checks
/// and serialization is transparent, so the wire shape stays the plain
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompactionId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_and_serializes_transparently() {
        let id = CompactionId("tx-1".to_owned());
        assert_eq!(id.0, "tx-1");
        assert_eq!(
            serde_json::to_value(&id).expect("serialize"),
            serde_json::json!("tx-1")
        );
        let round: CompactionId =
            serde_json::from_value(serde_json::json!("tx-1")).expect("deserialize");
        assert_eq!(round, id);
    }
}
