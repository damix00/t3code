use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::ids::{ProjectId, ThreadId};
use crate::projection::ThreadProjection;
use crate::shell::{ProjectShell, ShellSnapshot, ThreadShell, ThreadShellLocation};

/// `OrchestrationV2ShellStreamItem`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind")]
pub enum ShellStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot", rename_all = "camelCase")]
    Snapshot {
        snapshot: Box<ShellSnapshot>,
        /// Present on metadata-only enrichment refreshes.
        #[serde(default)]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated {
        sequence: u64,
        project: Box<ProjectShell>,
    },
    #[serde(rename = "project.removed", rename_all = "camelCase")]
    ProjectRemoved {
        sequence: u64,
        project_id: ProjectId,
    },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: u64,
        location: ThreadShellLocation,
        thread: Box<ThreadShell>,
    },
    #[serde(rename = "thread.removed", rename_all = "camelCase")]
    ThreadRemoved {
        sequence: u64,
        location: ThreadShellLocation,
        thread_id: ThreadId,
    },
    #[serde(other)]
    Unknown,
}

impl ShellStreamItem {
    /// The sequence of a delta; `None` for snapshots and markers.
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::ProjectUpdated { sequence, .. }
            | Self::ProjectRemoved { sequence, .. }
            | Self::ThreadUpdated { sequence, .. }
            | Self::ThreadRemoved { sequence, .. } => Some(*sequence),
            _ => None,
        }
    }
}

/// `OrchestrationV2DomainEvent`. The payload stays raw: the reducer decodes it by `type`, and
/// event types newer than this client pass through untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub thread_id: ThreadId,
    pub occurred_at: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2ThreadStreamItem`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind")]
pub enum ThreadStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot", rename_all = "camelCase")]
    Snapshot {
        snapshot_sequence: u64,
        projection: Box<ThreadProjection>,
        #[serde(default)]
        has_more_history: Option<bool>,
    },
    #[serde(rename = "event")]
    Event {
        sequence: u64,
        event: Box<DomainEvent>,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::fixtures::projection_json;
    use crate::shell::fixtures::{project_shell_json, thread_shell_json};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn decodes_every_shell_item_kind() {
        let items: Vec<ShellStreamItem> = serde_json::from_value(json!([
            {"kind": "synchronized"},
            {"kind": "snapshot", "snapshot": {"schemaVersion": 1, "snapshotSequence": 3,
                "threads": [], "archivedThreads": [], "projects": []}},
            {"kind": "snapshot", "resolvedRepositoryIdentityRoots": ["/a"], "snapshot": {
                "schemaVersion": 1, "snapshotSequence": 3, "threads": [], "archivedThreads": [],
                "projects": []}},
            {"kind": "project.updated", "sequence": 4, "project": project_shell_json("p")},
            {"kind": "project.removed", "sequence": 5, "projectId": "p"},
            {"kind": "thread.updated", "sequence": 6, "location": "active",
                "thread": thread_shell_json("t", "p")},
            {"kind": "thread.removed", "sequence": 7, "location": "archive", "threadId": "t"},
            {"kind": "thread.teleported", "sequence": 8}
        ]))
        .unwrap();
        assert_eq!(items[0], ShellStreamItem::Synchronized);
        assert!(matches!(
            &items[1],
            ShellStreamItem::Snapshot {
                resolved_repository_identity_roots: None,
                ..
            }
        ));
        assert!(matches!(
            &items[2],
            ShellStreamItem::Snapshot { resolved_repository_identity_roots: Some(roots), .. }
                if roots == &vec!["/a".to_owned()]
        ));
        let sequences: Vec<_> = items.iter().map(ShellStreamItem::sequence).collect();
        assert_eq!(
            sequences,
            vec![None, None, None, Some(4), Some(5), Some(6), Some(7), None]
        );
        assert_eq!(items[7], ShellStreamItem::Unknown);
        assert!(matches!(
            &items[6],
            ShellStreamItem::ThreadRemoved {
                location: ThreadShellLocation::Archive,
                ..
            }
        ));
    }

    #[test]
    fn decodes_thread_stream_items() {
        let items: Vec<ThreadStreamItem> = serde_json::from_value(json!([
            {"kind": "snapshot", "snapshotSequence": 10, "projection": projection_json("t"),
             "hasMoreHistory": false},
            {"kind": "event", "sequence": 11, "event": {
                "id": "e1", "type": "run.updated", "threadId": "t",
                "occurredAt": "2026-11-01T10:00:01.000Z", "runId": "r",
                "payload": {"id": "r", "status": "running"}}},
            {"kind": "event", "sequence": 12, "event": {
                "id": "e2", "type": "brand.new", "threadId": "t",
                "occurredAt": "2026-11-01T10:00:02.000Z"}},
            {"kind": "synchronized"},
            {"kind": "mystery"}
        ]))
        .unwrap();
        assert!(matches!(
            &items[0],
            ThreadStreamItem::Snapshot {
                snapshot_sequence: 10,
                ..
            }
        ));
        let ThreadStreamItem::Event { sequence, event } = &items[1] else {
            panic!()
        };
        assert_eq!(*sequence, 11);
        assert_eq!(event.event_type, "run.updated");
        assert_eq!(event.extra["runId"], json!("r"));
        let ThreadStreamItem::Event { event, .. } = &items[2] else {
            panic!()
        };
        assert_eq!(event.payload, Value::Null);
        assert_eq!(items[3], ThreadStreamItem::Synchronized);
        assert_eq!(items[4], ThreadStreamItem::Unknown);
    }
}
