use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::ids::{MessageId, ProjectId, RunId, RuntimeRequestId, ThreadId};
use crate::model::{ModelSelection, RunStatus};

/// `OrchestrationProjectShell`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    /// Absent and `null` both mean "not resolved yet".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ProjectShell {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

/// `OrchestrationV2AppThreadLineage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<String>,
    pub root_thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chat: Option<bool>,
}

impl ThreadLineage {
    /// `isListedThreadLineage`: subagents and side chats live inside their parent's view.
    pub fn is_listed(&self) -> bool {
        self.relationship_to_parent.as_deref() != Some("subagent") && self.side_chat != Some(true)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestVisibleMessage {
    pub id: MessageId,
    pub role: String,
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequestSummary {
    pub id: RuntimeRequestId,
    pub kind: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
}

/// `OrchestrationV2ThreadShell`: one sidebar row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub lineage: ThreadLineage,
    pub latest_run_id: Option<RunId>,
    pub active_run_id: Option<RunId>,
    /// `"idle"` or a run status.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_run_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub pending_runtime_request: Option<PendingRuntimeRequestSummary>,
    pub latest_visible_message: Option<LatestVisibleMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_background_tasks: Option<Vec<Value>>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<String>,
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
    pub deleted_at: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ThreadShell {
    pub fn run_status(&self) -> Option<RunStatus> {
        RunStatus::parse(&self.status)
    }

    /// Mirrors `isLiveSubagentShell`: the thread is doing or waiting on work.
    pub fn is_working(&self) -> bool {
        self.activity_run_status.is_some()
            || self.run_status().is_some_and(RunStatus::is_live)
            || self.pending_runtime_request.is_some()
            || self
                .pending_background_tasks
                .as_ref()
                .is_some_and(|tasks| !tasks.is_empty())
    }

    pub fn needs_attention(&self) -> bool {
        self.pending_runtime_request
            .as_ref()
            .is_some_and(|request| request.blocking != Some(false))
    }

    pub fn is_settled(&self) -> bool {
        self.settled_override.as_deref() == Some("settled")
    }
}

/// `OrchestrationV2ShellSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub schema_version: u32,
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
    pub projects: Vec<ProjectShell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_settled: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadShellLocation {
    Active,
    Archive,
}

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    use serde_json::{Value, json};

    pub fn thread_shell_json(id: &str, project: &str) -> Value {
        json!({
            "id": id,
            "projectId": project,
            "title": format!("Thread {id}"),
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "forkedFrom": null,
            "activeProviderThreadId": null,
            "latestRunId": null,
            "activeRunId": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "latestVisibleMessage": null,
            "latestUserMessageAt": null,
            "hasActionableProposedPlan": false,
            "itemCount": 0,
            "visibleItemCount": 0,
            "createdAt": "2026-11-01T10:00:00.000Z",
            "updatedAt": "2026-11-01T10:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null,
            "createdBy": "user",
            "creationSource": "web"
        })
    }

    pub fn project_shell_json(id: &str) -> Value {
        json!({
            "id": id,
            "title": format!("Project {id}"),
            "workspaceRoot": format!("/work/{id}"),
            "defaultModelSelection": null,
            "scripts": [],
            "createdAt": "2026-11-01T10:00:00.000Z",
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
    fn decodes_a_full_thread_shell_and_keeps_unknown_fields() {
        let thread: ThreadShell = serde_json::from_value(thread_shell_json("t1", "p1")).unwrap();
        assert_eq!(thread.id.as_str(), "t1");
        assert_eq!(thread.model_selection.model, "gpt-5");
        assert!(thread.lineage.is_listed());
        assert_eq!(thread.extra["itemCount"], json!(0));
        assert_eq!(thread.extra["creationSource"], json!("web"));
    }

    #[test]
    fn thread_shell_round_trips_known_and_unknown_fields() {
        let source = thread_shell_json("t1", "p1");
        let thread: ThreadShell = serde_json::from_value(source.clone()).unwrap();
        let encoded = serde_json::to_value(&thread).unwrap();
        let decoded: ThreadShell = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, thread);
    }

    #[test]
    fn subagent_and_side_chat_threads_are_not_listed() {
        let mut value = thread_shell_json("child", "p1");
        value["lineage"] = json!({"parentThreadId": "t1", "relationshipToParent": "subagent", "rootThreadId": "t1"});
        let child: ThreadShell = serde_json::from_value(value.clone()).unwrap();
        assert!(!child.lineage.is_listed());

        value["lineage"] = json!({"parentThreadId": "t1", "relationshipToParent": "fork",
            "rootThreadId": "t1", "sideChat": true});
        let side: ThreadShell = serde_json::from_value(value.clone()).unwrap();
        assert!(!side.lineage.is_listed());

        value["lineage"] =
            json!({"parentThreadId": "t1", "relationshipToParent": "fork", "rootThreadId": "t1"});
        let fork: ThreadShell = serde_json::from_value(value).unwrap();
        assert!(fork.lineage.is_listed());
    }

    #[test]
    fn working_state_follows_is_live_subagent_shell() {
        let base = thread_shell_json("t", "p");
        let idle: ThreadShell = serde_json::from_value(base.clone()).unwrap();
        assert!(!idle.is_working());

        for status in ["queued", "preparing", "starting", "running", "waiting"] {
            let mut value = base.clone();
            value["status"] = json!(status);
            let thread: ThreadShell = serde_json::from_value(value).unwrap();
            assert!(thread.is_working(), "{status}");
        }
        for status in [
            "completed",
            "failed",
            "interrupted",
            "cancelled",
            "rolled_back",
        ] {
            let mut value = base.clone();
            value["status"] = json!(status);
            let thread: ThreadShell = serde_json::from_value(value).unwrap();
            assert!(!thread.is_working(), "{status}");
        }

        let mut value = base.clone();
        value["activityRunStatus"] = json!("preparing");
        assert!(
            serde_json::from_value::<ThreadShell>(value)
                .unwrap()
                .is_working()
        );

        let mut value = base.clone();
        value["pendingBackgroundTasks"] = json!([{"id": "x"}]);
        assert!(
            serde_json::from_value::<ThreadShell>(value)
                .unwrap()
                .is_working()
        );

        let mut value = base;
        value["pendingBackgroundTasks"] = json!([]);
        assert!(
            !serde_json::from_value::<ThreadShell>(value)
                .unwrap()
                .is_working()
        );
    }

    #[test]
    fn attention_needs_a_blocking_request() {
        let mut value = thread_shell_json("t", "p");
        value["pendingRuntimeRequest"] =
            json!({"id": "r", "kind": "command", "createdAt": "2026-11-01T10:00:00Z"});
        let thread: ThreadShell = serde_json::from_value(value.clone()).unwrap();
        assert!(thread.needs_attention());
        assert!(thread.is_working());

        value["pendingRuntimeRequest"]["blocking"] = json!(false);
        let thread: ThreadShell = serde_json::from_value(value).unwrap();
        assert!(!thread.needs_attention());
    }

    #[test]
    fn decodes_project_shell() {
        let mut value = project_shell_json("p1");
        value["repositoryIdentity"] = Value::Null;
        let project: ProjectShell = serde_json::from_value(value).unwrap();
        assert_eq!(project.workspace_root, "/work/p1");
        assert_eq!(project.repository_identity, None);
        assert!(!project.is_archived());
        assert_eq!(project.extra["scripts"], json!([]));
    }

    #[test]
    fn decodes_shell_snapshot() {
        let snapshot: ShellSnapshot = serde_json::from_value(json!({
            "schemaVersion": 2,
            "snapshotSequence": 41,
            "threads": [thread_shell_json("t1", "p1")],
            "archivedThreads": [],
            "projects": [project_shell_json("p1")],
            "olderSettled": {"before": "2026-01-01T00:00:00Z", "asOfSequence": 41, "counts": []}
        }))
        .unwrap();
        assert_eq!(snapshot.snapshot_sequence, 41);
        assert_eq!(snapshot.threads.len(), 1);
        assert!(snapshot.older_settled.is_some());
    }

    #[test]
    fn rejects_shells_missing_required_fields() {
        let mut value = thread_shell_json("t", "p");
        value.as_object_mut().unwrap().remove("projectId");
        assert!(serde_json::from_value::<ThreadShell>(value).is_err());
    }
}
