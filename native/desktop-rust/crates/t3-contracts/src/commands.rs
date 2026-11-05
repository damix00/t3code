use serde::Serialize;

use crate::ids::{CommandId, MessageId, ProjectId, RunId, RuntimeRequestId, ThreadId};
use crate::model::{CreationSource, InteractionMode, ModelSelection, RuntimeMode};

/// How a message reaches a thread that may already be running (`dispatchMode`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DispatchMode {
    StartImmediately,
    QueueAfterActive,
}

/// `ProviderApprovalDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

/// The subset of `OrchestrationV2Command` this client dispatches, encoded exactly as
/// `packages/client-runtime/src/operations/commands.ts` builds them.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // Built once per user action and serialized right away.
pub enum Command {
    #[serde(rename = "message.dispatch", rename_all = "camelCase")]
    MessageDispatch {
        command_id: CommandId,
        created_by: &'static str,
        creation_source: CreationSource,
        thread_id: ThreadId,
        message_id: MessageId,
        text: String,
        attachments: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_selection: Option<ModelSelection>,
        #[serde(skip_serializing_if = "Option::is_none")]
        delivery_intent: Option<&'static str>,
        dispatch_mode: DispatchMode,
    },
    #[serde(rename = "thread.stop", rename_all = "camelCase")]
    ThreadStop {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "run.interrupt", rename_all = "camelCase")]
    RunInterrupt {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    #[serde(rename = "runtime-request.respond", rename_all = "camelCase")]
    RuntimeRequestRespond {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
        decision: ApprovalDecision,
    },
    #[serde(rename = "thread.archive", rename_all = "camelCase")]
    ThreadArchive {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.unarchive", rename_all = "camelCase")]
    ThreadUnarchive {
        command_id: CommandId,
        thread_id: ThreadId,
    },
}

impl Command {
    /// A user message to an existing thread, sent like the web composer's default "auto" mode.
    pub fn send_message(thread_id: ThreadId, text: impl Into<String>) -> Self {
        Self::MessageDispatch {
            command_id: CommandId::random(),
            created_by: "user",
            creation_source: CreationSource::Web,
            thread_id,
            message_id: MessageId::random(),
            text: text.into(),
            attachments: Vec::new(),
            model_selection: None,
            delivery_intent: Some("auto"),
            dispatch_mode: DispatchMode::StartImmediately,
        }
    }

    /// The same message, queued behind the running turn instead of steering it.
    pub fn queue_message(thread_id: ThreadId, text: impl Into<String>) -> Self {
        Self::MessageDispatch {
            command_id: CommandId::random(),
            created_by: "user",
            creation_source: CreationSource::Web,
            thread_id,
            message_id: MessageId::random(),
            text: text.into(),
            attachments: Vec::new(),
            model_selection: None,
            delivery_intent: None,
            dispatch_mode: DispatchMode::QueueAfterActive,
        }
    }

    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::MessageDispatch { thread_id, .. }
            | Self::ThreadStop { thread_id, .. }
            | Self::RunInterrupt { thread_id, .. }
            | Self::RuntimeRequestRespond { thread_id, .. }
            | Self::ThreadArchive { thread_id, .. }
            | Self::ThreadUnarchive { thread_id, .. } => thread_id,
        }
    }
}

/// `OrchestrationV2ThreadLaunchWorkspaceStrategy`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceStrategy {
    Root {
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Worktree {
        base_ref: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<serde_json::Value>,
}

/// `OrchestrationV2ThreadLaunchInput`: creates a thread and sends its first message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchThreadInput {
    pub command_id: CommandId,
    pub creation_source: CreationSource,
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub generate_title: bool,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace_strategy: WorkspaceStrategy,
    pub initial_message: LaunchMessage,
}

/// The `project.create` mutation sent to `projects.mutate`, as `buildProjectCreateCommand`
/// builds it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename = "project.create", rename_all = "camelCase")]
pub struct ProjectCreate {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub create_workspace_root_if_missing: bool,
    pub default_model_selection: Option<ModelSelection>,
}

impl ProjectCreate {
    pub fn for_path(workspace_root: &str) -> Self {
        Self {
            command_id: CommandId::random(),
            project_id: ProjectId::random(),
            title: infer_project_title(workspace_root),
            workspace_root: workspace_root.trim().to_owned(),
            create_workspace_root_if_missing: true,
            default_model_selection: None,
        }
    }
}

/// `inferProjectTitleFromPath`: the last non-empty path segment.
pub fn infer_project_title(path: &str) -> String {
    let trimmed = path.trim();
    trimmed
        .split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn send_message_matches_web_composer_shape() {
        let command = Command::send_message(ThreadId::new("t1"), "hi");
        let Command::MessageDispatch {
            command_id,
            message_id,
            ..
        } = &command
        else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(&command).unwrap(),
            json!({
                "type": "message.dispatch",
                "commandId": command_id.as_str(),
                "createdBy": "user",
                "creationSource": "web",
                "threadId": "t1",
                "messageId": message_id.as_str(),
                "text": "hi",
                "attachments": [],
                "deliveryIntent": "auto",
                "dispatchMode": {"type": "start_immediately"}
            })
        );
    }

    #[test]
    fn queue_message_omits_delivery_intent() {
        let value = serde_json::to_value(Command::queue_message(ThreadId::new("t"), "x")).unwrap();
        assert_eq!(value["dispatchMode"], json!({"type": "queue_after_active"}));
        assert!(value.get("deliveryIntent").is_none());
        assert!(value.get("modelSelection").is_none());
    }

    #[test]
    fn control_commands() {
        let stop = Command::ThreadStop {
            command_id: "c".into(),
            thread_id: "t".into(),
        };
        assert_eq!(
            serde_json::to_value(&stop).unwrap(),
            json!({"type": "thread.stop", "commandId": "c", "threadId": "t"})
        );
        let interrupt = Command::RunInterrupt {
            command_id: "c".into(),
            thread_id: "t".into(),
            run_id: "r".into(),
        };
        assert_eq!(
            serde_json::to_value(&interrupt).unwrap(),
            json!({"type": "run.interrupt", "commandId": "c", "threadId": "t", "runId": "r"})
        );
        let archive = Command::ThreadArchive {
            command_id: "c".into(),
            thread_id: "t".into(),
        };
        assert_eq!(
            serde_json::to_value(&archive).unwrap()["type"],
            json!("thread.archive")
        );
        let unarchive = Command::ThreadUnarchive {
            command_id: "c".into(),
            thread_id: "t".into(),
        };
        assert_eq!(
            serde_json::to_value(&unarchive).unwrap()["type"],
            json!("thread.unarchive")
        );
        assert_eq!(unarchive.thread_id().as_str(), "t");
    }

    #[test]
    fn approval_response() {
        for (decision, wire) in [
            (ApprovalDecision::Accept, "accept"),
            (ApprovalDecision::AcceptForSession, "acceptForSession"),
            (ApprovalDecision::AcceptAlways, "acceptAlways"),
            (ApprovalDecision::Decline, "decline"),
            (ApprovalDecision::Cancel, "cancel"),
        ] {
            let command = Command::RuntimeRequestRespond {
                command_id: "c".into(),
                thread_id: "t".into(),
                request_id: "q".into(),
                decision,
            };
            assert_eq!(
                serde_json::to_value(&command).unwrap(),
                json!({"type": "runtime-request.respond", "commandId": "c", "threadId": "t",
                       "requestId": "q", "decision": wire})
            );
        }
    }

    #[test]
    fn launch_input_shape() {
        let input = LaunchThreadInput {
            command_id: "c".into(),
            creation_source: CreationSource::Web,
            thread_id: "t".into(),
            project_id: "p".into(),
            title: "Fix the bug".into(),
            generate_title: true,
            model_selection: ModelSelection::new("codex", "gpt-5"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace_strategy: WorkspaceStrategy::Root { branch: None },
            initial_message: LaunchMessage {
                message_id: "m".into(),
                text: "Fix the bug".into(),
                attachments: vec![],
            },
        };
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({
                "commandId": "c",
                "creationSource": "web",
                "threadId": "t",
                "projectId": "p",
                "title": "Fix the bug",
                "generateTitle": true,
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "workspaceStrategy": {"type": "root"},
                "initialMessage": {"messageId": "m", "text": "Fix the bug", "attachments": []}
            })
        );
    }

    #[test]
    fn worktree_strategy_shape() {
        let strategy = WorkspaceStrategy::Worktree {
            base_ref: "main".into(),
            branch: Some("t3/x".into()),
        };
        assert_eq!(
            serde_json::to_value(&strategy).unwrap(),
            json!({"type": "worktree", "baseRef": "main", "branch": "t3/x"})
        );
    }

    #[test]
    fn project_titles_come_from_the_last_segment() {
        assert_eq!(infer_project_title("/Users/me/code/t3code"), "t3code");
        assert_eq!(infer_project_title("/Users/me/code/t3code/"), "t3code");
        assert_eq!(infer_project_title("C:\\work\\repo"), "repo");
        assert_eq!(infer_project_title("  relative/dir  "), "dir");
        assert_eq!(infer_project_title("/"), "/");
        assert_eq!(infer_project_title("single"), "single");
    }

    #[test]
    fn project_create_encodes_like_upstream() {
        let mut command = ProjectCreate::for_path("/srv/app/");
        command.command_id = CommandId::new("c-1");
        command.project_id = ProjectId::new("p-1");
        assert_eq!(
            serde_json::to_value(&command).unwrap(),
            json!({"type": "project.create", "commandId": "c-1", "projectId": "p-1",
                "title": "app", "workspaceRoot": "/srv/app/",
                "createWorkspaceRootIfMissing": true, "defaultModelSelection": null})
        );
    }
}
