//! What the UI asks the server to do, built as data so it can be tested without a window.

use t3_contracts::{
    ApprovalDecision, Command, CommandId, CreationSource, InteractionMode, LaunchMessage,
    LaunchThreadInput, MessageId, ProjectId, RuntimeMode, RuntimeRequestId, ServerConfig, ThreadId,
    ThreadProjection, WorkspaceStrategy,
};
use t3_state::title::derive_thread_title_seed;

/// What sending the composer's text does.
#[derive(Debug, Clone, PartialEq)]
pub enum Submission {
    /// Post to the open thread.
    Dispatch(Command),
    /// Start a thread in a project with this as its first message.
    Launch(LaunchThreadInput),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    Empty,
    NoTarget,
    NoProvider,
}

impl std::error::Error for SubmitError {}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "Type a message first.",
            Self::NoTarget => "Pick a project or thread first.",
            Self::NoProvider => {
                "No provider is ready on this environment. Set one up in T3 Code first."
            }
        })
    }
}

/// Where the composer sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerTarget {
    Thread(ThreadId),
    NewThread(ProjectId),
    Nothing,
}

/// Builds the request for the composer's text.
pub fn submission(
    target: &ComposerTarget,
    text: &str,
    config: Option<&ServerConfig>,
) -> Result<Submission, SubmitError> {
    if text.trim().is_empty() {
        return Err(SubmitError::Empty);
    }
    match target {
        ComposerTarget::Nothing => Err(SubmitError::NoTarget),
        ComposerTarget::Thread(thread_id) => Ok(Submission::Dispatch(Command::send_message(
            thread_id.clone(),
            text,
        ))),
        ComposerTarget::NewThread(project_id) => {
            let model_selection = config
                .and_then(ServerConfig::default_model_selection)
                .ok_or(SubmitError::NoProvider)?;
            Ok(Submission::Launch(LaunchThreadInput {
                command_id: CommandId::random(),
                creation_source: CreationSource::Web,
                thread_id: ThreadId::random(),
                project_id: project_id.clone(),
                title: derive_thread_title_seed(text),
                generate_title: true,
                model_selection,
                runtime_mode: RuntimeMode::default(),
                interaction_mode: InteractionMode::default(),
                workspace_strategy: WorkspaceStrategy::Root { branch: None },
                initial_message: LaunchMessage {
                    message_id: MessageId::random(),
                    text: text.to_owned(),
                    attachments: Vec::new(),
                },
            }))
        }
    }
}

/// Stops the open thread, as the web client's stop button does.
pub fn stop(
    thread_id: &ThreadId,
    config: Option<&ServerConfig>,
    projection: Option<&ThreadProjection>,
) -> Option<Command> {
    let supports_thread_stop = config.is_some_and(|config| config.has_capability("threadStop"));
    t3_state::stop::stop_command(thread_id, supports_thread_stop, projection)
}

/// Answers an approval request.
pub fn respond(
    thread_id: &ThreadId,
    request_id: &RuntimeRequestId,
    decision: ApprovalDecision,
) -> Command {
    Command::RuntimeRequestRespond {
        command_id: CommandId::random(),
        thread_id: thread_id.clone(),
        request_id: request_id.clone(),
        decision,
    }
}

pub fn archive(thread_id: &ThreadId) -> Command {
    Command::ThreadArchive {
        command_id: CommandId::random(),
        thread_id: thread_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn config(capabilities: serde_json::Value, providers: serde_json::Value) -> ServerConfig {
        serde_json::from_value(json!({
            "environment": {"label": "Box", "capabilities": capabilities},
            "cwd": "/", "providers": providers
        }))
        .unwrap()
    }

    fn codex() -> serde_json::Value {
        json!([{"instanceId": "codex", "driver": "codex", "displayName": "Codex",
            "enabled": true, "installed": true, "status": "ready", "availability": "available",
            "models": [{"slug": "gpt-5", "name": "GPT-5", "isDefault": true}]}])
    }

    #[test]
    fn blank_text_is_rejected_first() {
        let target = ComposerTarget::Thread(ThreadId::new("t"));
        assert_eq!(submission(&target, "  \n", None), Err(SubmitError::Empty));
        assert_eq!(
            submission(&ComposerTarget::Nothing, "", None),
            Err(SubmitError::Empty)
        );
        assert_eq!(
            submission(&ComposerTarget::Nothing, "hi", None),
            Err(SubmitError::NoTarget)
        );
    }

    #[test]
    fn messages_to_an_open_thread_dispatch() {
        let Ok(Submission::Dispatch(command)) = submission(
            &ComposerTarget::Thread(ThreadId::new("t-1")),
            "fix the tests",
            None,
        ) else {
            panic!("expected a dispatch");
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["type"], "message.dispatch");
        assert_eq!(value["threadId"], "t-1");
        assert_eq!(value["text"], "fix the tests");
        assert_eq!(value["deliveryIntent"], "auto");
        assert_eq!(value["dispatchMode"], json!({"type": "start_immediately"}));
    }

    #[test]
    fn new_threads_launch_with_the_default_model_and_a_title_seed() {
        let config = config(json!({}), codex());
        let Ok(Submission::Launch(input)) = submission(
            &ComposerTarget::NewThread(ProjectId::new("p-1")),
            "  Refactor the parser\nso it streams  ",
            Some(&config),
        ) else {
            panic!("expected a launch");
        };
        assert_eq!(input.project_id.as_str(), "p-1");
        assert_eq!(input.title, "Refactor the parser so it streams");
        assert!(input.generate_title);
        assert_eq!(input.model_selection.instance_id, "codex");
        assert_eq!(input.model_selection.model, "gpt-5");
        assert_eq!(
            input.initial_message.text,
            "  Refactor the parser\nso it streams  "
        );
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value["workspaceStrategy"], json!({"type": "root"}));
        assert_eq!(value["runtimeMode"], "full-access");
        assert_eq!(value["interactionMode"], "default");
        assert_eq!(value["creationSource"], "web");
    }

    #[test]
    fn new_threads_need_a_ready_provider() {
        let target = ComposerTarget::NewThread(ProjectId::new("p"));
        assert_eq!(
            submission(&target, "hi", None),
            Err(SubmitError::NoProvider)
        );
        assert_eq!(
            submission(&target, "hi", Some(&config(json!({}), json!([])))),
            Err(SubmitError::NoProvider)
        );
        assert_eq!(
            SubmitError::NoProvider.to_string(),
            "No provider is ready on this environment. Set one up in T3 Code first."
        );
    }

    #[test]
    fn stop_prefers_thread_stop_when_supported() {
        let thread = ThreadId::new("t");
        let supported = config(json!({"threadStop": true}), json!([]));
        let command = stop(&thread, Some(&supported), None).unwrap();
        assert_eq!(
            serde_json::to_value(&command).unwrap()["type"],
            "thread.stop"
        );
        // Without the capability, only a live run can be interrupted.
        let unsupported = config(json!({}), json!([]));
        assert_eq!(stop(&thread, Some(&unsupported), None), None);
        let projection: ThreadProjection = serde_json::from_value({
            let mut projection = t3_contracts::fixtures::projection_json("t");
            projection["runs"] = json!([{"id": "run-1", "status": "running"}]);
            projection
        })
        .unwrap();
        let command = stop(&thread, Some(&unsupported), Some(&projection)).unwrap();
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(
            (value["type"].as_str(), value["runId"].as_str()),
            (Some("run.interrupt"), Some("run-1"))
        );
    }

    #[test]
    fn approvals_and_archive_encode() {
        let thread = ThreadId::new("t");
        let value = serde_json::to_value(respond(
            &thread,
            &RuntimeRequestId::new("r"),
            ApprovalDecision::AcceptForSession,
        ))
        .unwrap();
        assert_eq!(value["type"], "runtime-request.respond");
        assert_eq!(value["requestId"], "r");
        assert_eq!(value["decision"], "acceptForSession");
        assert_eq!(
            serde_json::to_value(archive(&thread)).unwrap()["type"],
            "thread.archive"
        );
    }
}
