use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::ids::{MessageId, NodeId, RunAttemptId, RunId, RuntimeRequestId, ThreadId, TurnItemId};
use crate::model::{RunStatus, UserMessageInputIntent};

/// Any record that only needs to be found by `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2Run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: RunId,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_cancelled_background_work: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Run {
    pub fn run_status(&self) -> Option<RunStatus> {
        RunStatus::parse(&self.status)
    }
}

/// `OrchestrationV2RunAttempt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: RunAttemptId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub status: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2ConversationMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: MessageId,
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2Checkpoint`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: Value,
    #[serde(default)]
    pub files: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2Subagent`. Its `prompt` and `result` bodies may arrive deduped
/// (`promptOmitted`/`resultOmitted`), so they stay in `extra` where the restore logic edits them
/// the same way it edits subagent turn items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subagent {
    pub id: NodeId,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2ProviderTurn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `OrchestrationV2RuntimeRequest`: an approval or question the agent is waiting on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub response_capability: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

impl RuntimeRequest {
    pub fn is_pending(&self) -> bool {
        self.status == "pending"
    }

    /// Whether an answer can reach the provider (`live` or `message` capability).
    pub fn is_answerable(&self) -> bool {
        matches!(
            self.response_capability.get("type").and_then(Value::as_str),
            Some("live" | "message")
        )
    }
}

/// `OrchestrationV2TurnItem`: one transcript entry of any type.
///
/// Item types are forward compatible upstream, so the type stays a string and the type-specific
/// fields stay in `extra`, read through accessors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    pub id: TurnItemId,
    #[serde(rename = "type")]
    pub item_type: String,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub ordinal: u64,
    #[serde(default)]
    pub status: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

impl TurnItem {
    fn str_field(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }

    fn bool_field(&self, key: &str) -> Option<bool> {
        self.extra.get(key).and_then(Value::as_bool)
    }

    pub fn text(&self) -> Option<&str> {
        self.str_field("text")
    }

    pub fn title(&self) -> Option<&str> {
        self.str_field("title")
    }

    pub fn status_str(&self) -> Option<&str> {
        self.status.as_str()
    }

    pub fn is_streaming(&self) -> bool {
        self.bool_field("streaming") == Some(true)
    }

    pub fn message_id(&self) -> Option<MessageId> {
        self.str_field("messageId").map(MessageId::new)
    }

    pub fn input_intent(&self) -> Option<UserMessageInputIntent> {
        self.str_field("inputIntent")
            .and_then(UserMessageInputIntent::parse)
    }

    pub fn text_omitted(&self) -> bool {
        self.bool_field("textOmitted") == Some(true)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.str_field(key)
    }

    pub fn field(&self, key: &str) -> Option<&Value> {
        self.extra.get(key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Local,
    Inherited,
    Synthetic,
}

/// `OrchestrationV2ProjectedTurnItem`: one row of the visible transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: Visibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: TurnItem,
}

/// `OrchestrationV2ThreadProjection`: everything the thread view renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: Value,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<Entity>,
    pub subagents: Vec<Subagent>,
    pub provider_sessions: Vec<Entity>,
    pub provider_threads: Vec<Entity>,
    pub provider_turns: Vec<ProviderTurn>,
    pub runtime_requests: Vec<RuntimeRequest>,
    pub messages: Vec<ConversationMessage>,
    pub plans: Vec<Entity>,
    pub turn_items: Vec<TurnItem>,
    pub checkpoint_scopes: Vec<Entity>,
    pub checkpoints: Vec<Checkpoint>,
    pub context_handoffs: Vec<Entity>,
    pub context_transfers: Vec<Entity>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ThreadProjection {
    pub fn thread_id(&self) -> Option<ThreadId> {
        self.thread
            .get("id")
            .and_then(Value::as_str)
            .map(ThreadId::new)
    }

    pub fn title(&self) -> Option<&str> {
        self.thread.get("title").and_then(Value::as_str)
    }

    pub fn pending_runtime_requests(&self) -> impl Iterator<Item = &RuntimeRequest> {
        self.runtime_requests
            .iter()
            .filter(|request| request.is_pending())
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    use serde_json::{Value, json};

    pub fn turn_item_json(id: &str, item_type: &str, ordinal: u64, run: Option<&str>) -> Value {
        json!({
            "id": id,
            "type": item_type,
            "threadId": "thread-1",
            "runId": run,
            "nodeId": run.map(|run| format!("node-{run}")),
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": ordinal,
            "status": "completed",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": "2026-11-01T10:00:00.000Z"
        })
    }

    pub fn projection_json(thread_id: &str) -> Value {
        json!({
            "thread": {"id": thread_id, "title": "A thread"},
            "runs": [],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": "2026-11-01T10:00:00.000Z"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn decodes_an_assistant_message_item() {
        let mut value = turn_item_json("i1", "assistant_message", 3, Some("r1"));
        value["messageId"] = json!("m1");
        value["text"] = json!("Hello");
        value["streaming"] = json!(true);
        let item: TurnItem = serde_json::from_value(value).unwrap();
        assert_eq!(item.item_type, "assistant_message");
        assert_eq!(item.text(), Some("Hello"));
        assert!(item.is_streaming());
        assert_eq!(item.message_id(), Some(MessageId::new("m1")));
        assert_eq!(item.run_id, Some(RunId::new("r1")));
        assert_eq!(item.node_id, Some(NodeId::new("node-r1")));
        assert_eq!(item.status_str(), Some("completed"));
    }

    #[test]
    fn decodes_unknown_item_types() {
        let mut value = turn_item_json("i1", "hologram", 1, None);
        value["beam"] = json!({"power": 9});
        let item: TurnItem = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(item.item_type, "hologram");
        assert_eq!(item.field("beam"), Some(&json!({"power": 9})));
        // Round trip keeps every field.
        assert_eq!(serde_json::to_value(&item).unwrap(), value);
    }

    #[test]
    fn user_message_intent() {
        let mut value = turn_item_json("i1", "user_message", 1, Some("r"));
        value["inputIntent"] = json!("queued_turn");
        value["textOmitted"] = json!(true);
        let item: TurnItem = serde_json::from_value(value).unwrap();
        assert_eq!(
            item.input_intent(),
            Some(UserMessageInputIntent::QueuedTurn)
        );
        assert!(item.text_omitted());
    }

    #[test]
    fn decodes_projection() {
        let mut value = projection_json("thread-1");
        value["runs"] = json!([{"id": "r1", "status": "running", "threadId": "thread-1"}]);
        value["visibleTurnItems"] = json!([{
            "position": 0,
            "visibility": "local",
            "sourceThreadId": "thread-1",
            "sourceItemId": "i1",
            "item": turn_item_json("i1", "reasoning", 1, Some("r1"))
        }]);
        value["runtimeRequests"] = json!([
            {"id": "q1", "kind": "command", "status": "pending",
             "responseCapability": {"type": "live", "providerSessionId": "s"}},
            {"id": "q2", "kind": "command", "status": "resolved",
             "responseCapability": {"type": "not_resumable", "reason": "gone"}}
        ]);
        let projection: ThreadProjection = serde_json::from_value(value).unwrap();
        assert_eq!(projection.thread_id(), Some(ThreadId::new("thread-1")));
        assert_eq!(projection.title(), Some("A thread"));
        assert_eq!(projection.runs[0].run_status(), Some(RunStatus::Running));
        assert_eq!(
            projection.visible_turn_items[0].visibility,
            Visibility::Local
        );
        let pending: Vec<_> = projection.pending_runtime_requests().collect();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].is_answerable());
        assert!(!projection.runtime_requests[1].is_answerable());
    }

    #[test]
    fn projection_without_required_arrays_is_rejected() {
        let mut value = projection_json("t");
        value.as_object_mut().unwrap().remove("visibleTurnItems");
        assert!(serde_json::from_value::<ThreadProjection>(value).is_err());
    }
}
