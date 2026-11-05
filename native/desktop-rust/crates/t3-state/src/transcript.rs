//! Turns the projection's visible turn items into rows the thread view draws.

use serde_json::Value;
use t3_contracts::{RuntimeRequestId, ThreadProjection, TurnItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    Running,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoStep {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    User {
        text: String,
    },
    Assistant {
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    Command {
        command: String,
        output: Option<String>,
        exit_code: Option<i64>,
        running: bool,
        failed: bool,
    },
    FileChange {
        path: String,
        additions: Option<u64>,
        deletions: Option<u64>,
    },
    Search {
        label: String,
    },
    Plan {
        markdown: String,
        streaming: bool,
    },
    Todo {
        steps: Vec<TodoStep>,
    },
    /// The agent asks permission. `pending` approvals can be answered from the transcript.
    Approval {
        request_id: RuntimeRequestId,
        kind: String,
        prompt: Option<String>,
        pending: bool,
    },
    Question {
        request_id: RuntimeRequestId,
        prompt: String,
        pending: bool,
    },
    Error {
        message: String,
    },
    Notice {
        text: String,
    },
    /// A tool the agent called through an integration (MCP), e.g. `mcp__t3-code__preview_open`.
    Tool {
        name: String,
        running: bool,
        failed: bool,
    },
    /// An item type this client does not draw in detail.
    Other {
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Stable across updates, for list keys and scroll anchoring.
    pub key: String,
    pub kind: EntryKind,
    pub meta: EntryMeta,
}

/// Where and when an entry happened, for grouping it under its run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryMeta {
    pub run_id: Option<String>,
    /// ISO timestamps from the item.
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// A run's lifecycle, for "Worked for" folds and the working indicator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl RunSummary {
    /// Still producing output.
    pub fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "preparing" | "queued" | "starting" | "running" | "waiting"
        )
    }
}

/// The thread's runs, in projection order.
pub fn build_runs(projection: &ThreadProjection) -> Vec<RunSummary> {
    projection
        .runs
        .iter()
        .map(|run| RunSummary {
            id: run.id.as_str().to_owned(),
            status: run.status.clone(),
            started_at: run
                .extra
                .get("startedAt")
                .and_then(Value::as_str)
                .map(str::to_owned),
            completed_at: run
                .extra
                .get("completedAt")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect()
}

fn text_of(item: &TurnItem) -> String {
    item.text().unwrap_or_default().to_owned()
}

fn status_is(item: &TurnItem, statuses: &[&str]) -> bool {
    item.status_str()
        .is_some_and(|status| statuses.contains(&status))
}

fn u64_field(item: &TurnItem, key: &str) -> Option<u64> {
    item.field(key).and_then(Value::as_u64)
}

fn humanize(item_type: &str) -> String {
    let mut label = item_type.replace(['_', '-'], " ");
    if let Some(first) = label.get(..1) {
        let upper = first.to_uppercase();
        label.replace_range(..1, &upper);
    }
    label
}

fn request_pending(projection: &ThreadProjection, request_id: &RuntimeRequestId) -> bool {
    projection
        .runtime_requests
        .iter()
        .any(|request| &request.id == request_id && request.is_pending() && request.is_answerable())
}

fn entry_kind(item: &TurnItem, projection: &ThreadProjection) -> Option<EntryKind> {
    let kind = match item.item_type.as_str() {
        "user_message" => EntryKind::User {
            text: text_of(item),
        },
        "assistant_message" => EntryKind::Assistant {
            text: text_of(item),
            streaming: item.is_streaming(),
        },
        "reasoning" => {
            let text = text_of(item);
            // Empty finished reasoning carries nothing to show.
            if text.trim().is_empty() && !item.is_streaming() {
                return None;
            }
            EntryKind::Reasoning {
                text,
                streaming: item.is_streaming(),
            }
        }
        "command_execution" => EntryKind::Command {
            command: item.str("input").unwrap_or_default().to_owned(),
            output: item.str("output").map(str::to_owned),
            exit_code: item.field("exitCode").and_then(Value::as_i64),
            running: status_is(item, &["pending", "running", "waiting"]),
            failed: status_is(item, &["failed"])
                || item
                    .field("outputIndicatesFailure")
                    .and_then(Value::as_bool)
                    == Some(true)
                || item
                    .field("exitCode")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0),
        },
        "file_change" => EntryKind::FileChange {
            path: item.str("fileName").unwrap_or_default().to_owned(),
            additions: u64_field(item, "additions"),
            deletions: u64_field(item, "deletions"),
        },
        "file_search" => EntryKind::Search {
            label: match item.str("pattern") {
                Some(pattern) => format!("Searched files for {pattern}"),
                None => "Searched files".to_owned(),
            },
        },
        "web_search" => {
            let patterns: Vec<&str> = item
                .field("patterns")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            EntryKind::Search {
                label: if patterns.is_empty() {
                    "Searched the web".to_owned()
                } else {
                    format!("Searched the web for {}", patterns.join(", "))
                },
            }
        }
        "proposed_plan" => EntryKind::Plan {
            markdown: item.str("markdown").unwrap_or_default().to_owned(),
            streaming: item.is_streaming(),
        },
        "todo_list" => EntryKind::Todo {
            steps: item
                .field("steps")
                .and_then(Value::as_array)
                .map(|steps| {
                    steps
                        .iter()
                        .filter_map(|step| {
                            let text = step.get("text")?.as_str()?.to_owned();
                            let status = match step.get("status").and_then(Value::as_str) {
                                Some("completed") => TodoStatus::Completed,
                                Some("running") => TodoStatus::Running,
                                _ => TodoStatus::Pending,
                            };
                            Some(TodoStep { text, status })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "approval_request" => {
            let request_id = RuntimeRequestId::new(item.str("requestId").unwrap_or_default());
            EntryKind::Approval {
                pending: request_pending(projection, &request_id),
                kind: item.str("requestKind").unwrap_or("action").to_owned(),
                prompt: item
                    .str("prompt")
                    .or_else(|| item.str("reason"))
                    .map(str::to_owned),
                request_id,
            }
        }
        "user_input_request" => {
            let request_id = RuntimeRequestId::new(item.str("requestId").unwrap_or_default());
            let prompt = item
                .field("questions")
                .and_then(Value::as_array)
                .map(|questions| {
                    questions
                        .iter()
                        .filter_map(|question| question.get("question")?.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            EntryKind::Question {
                pending: request_pending(projection, &request_id),
                prompt,
                request_id,
            }
        }
        "error" => EntryKind::Error {
            message: item
                .field("failure")
                .and_then(|failure| failure.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("The agent hit an error.")
                .to_owned(),
        },
        "system_notice" | "run_interrupt_request" | "run_interrupt_result" => EntryKind::Notice {
            text: item.str("message").unwrap_or_default().to_owned(),
        },
        "notification" => EntryKind::Notice {
            text: item.str("summary").unwrap_or_default().to_owned(),
        },
        "compaction" => EntryKind::Notice {
            text: "Context compacted".to_owned(),
        },
        "dynamic_tool" | "mcp_tool_call" => EntryKind::Tool {
            name: item
                .str("toolName")
                .or_else(|| item.title())
                .unwrap_or("tool")
                .to_owned(),
            running: status_is(item, &["pending", "running", "waiting"]),
            failed: status_is(item, &["failed", "declined"]),
        },
        // Checkpoints are bookkeeping for diffs and restores.
        "checkpoint" => return None,
        other => EntryKind::Other {
            label: item.title().map_or_else(|| humanize(other), str::to_owned),
        },
    };
    Some(kind)
}

/// The rows of a thread view, in transcript order.
pub fn build_transcript(projection: &ThreadProjection) -> Vec<TranscriptEntry> {
    let mut rows: Vec<_> = projection.visible_turn_items.iter().collect();
    rows.sort_by_key(|row| row.position);
    rows.into_iter()
        .filter_map(|row| {
            entry_kind(&row.item, projection).map(|kind| TranscriptEntry {
                key: format!("{}:{}", row.source_thread_id, row.source_item_id),
                kind,
                meta: EntryMeta {
                    run_id: row.item.run_id.as_ref().map(|id| id.as_str().to_owned()),
                    started_at: row.item.str("startedAt").map(str::to_owned),
                    completed_at: row.item.str("completedAt").map(str::to_owned),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn projection(items: Vec<Value>, requests: Value) -> ThreadProjection {
        let rows: Vec<Value> = items
            .into_iter()
            .enumerate()
            .map(|(position, item)| {
                json!({"position": position, "visibility": "local", "sourceThreadId": "t",
                       "sourceItemId": item["id"], "item": item})
            })
            .collect();
        serde_json::from_value(json!({
            "thread": {"id": "t"}, "runs": [], "attempts": [], "nodes": [], "subagents": [],
            "providerSessions": [], "providerThreads": [], "providerTurns": [],
            "runtimeRequests": requests, "messages": [], "plans": [], "turnItems": [],
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
            "contextTransfers": [], "visibleTurnItems": rows, "updatedAt": "0"
        }))
        .unwrap()
    }

    fn item(id: &str, item_type: &str, fields: Value) -> Value {
        let mut value = json!({"id": id, "type": item_type, "threadId": "t", "runId": "r",
            "nodeId": null, "ordinal": 1, "status": "completed", "title": null});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        value
    }

    fn kinds(projection: &ThreadProjection) -> Vec<EntryKind> {
        build_transcript(projection)
            .into_iter()
            .map(|entry| entry.kind)
            .collect()
    }

    #[test]
    fn conversation_rows() {
        let projection = projection(
            vec![
                item("1", "user_message", json!({"text": "hi"})),
                item("2", "reasoning", json!({"text": "", "streaming": false})),
                item("3", "reasoning", json!({"text": "", "streaming": true})),
                item(
                    "4",
                    "assistant_message",
                    json!({"text": "hello", "streaming": true}),
                ),
            ],
            json!([]),
        );
        assert_eq!(
            kinds(&projection),
            vec![
                EntryKind::User { text: "hi".into() },
                EntryKind::Reasoning {
                    text: String::new(),
                    streaming: true
                },
                EntryKind::Assistant {
                    text: "hello".into(),
                    streaming: true
                },
            ]
        );
        assert_eq!(build_transcript(&projection)[0].key, "t:1");
    }

    #[test]
    fn rows_follow_position_not_array_order() {
        let mut projection = projection(
            vec![
                item("a", "user_message", json!({"text": "first"})),
                item("b", "user_message", json!({"text": "second"})),
            ],
            json!([]),
        );
        projection.visible_turn_items.reverse();
        let texts: Vec<_> = kinds(&projection)
            .into_iter()
            .map(|kind| match kind {
                EntryKind::User { text } => text,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn commands_report_running_and_failure() {
        let projection = projection(
            vec![
                item(
                    "1",
                    "command_execution",
                    json!({"input": "ls", "status": "running"}),
                ),
                item(
                    "2",
                    "command_execution",
                    json!({"input": "false", "output": "", "exitCode": 1}),
                ),
                item(
                    "3",
                    "command_execution",
                    json!({"input": "x", "outputIndicatesFailure": true}),
                ),
                item(
                    "4",
                    "command_execution",
                    json!({"input": "ok", "exitCode": 0, "output": "fine"}),
                ),
            ],
            json!([]),
        );
        let flags: Vec<_> = kinds(&projection)
            .into_iter()
            .map(|kind| match kind {
                EntryKind::Command {
                    running, failed, ..
                } => (running, failed),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            flags,
            vec![(true, false), (false, true), (false, true), (false, false)]
        );
    }

    #[test]
    fn approvals_are_pending_only_while_answerable() {
        let projection = projection(
            vec![
                item(
                    "1",
                    "approval_request",
                    json!({"requestId": "q1", "requestKind": "command",
                    "prompt": "run rm?"}),
                ),
                item(
                    "2",
                    "approval_request",
                    json!({"requestId": "q2", "requestKind": "file-change",
                    "reason": "edit"}),
                ),
                item(
                    "3",
                    "approval_request",
                    json!({"requestId": "q3", "requestKind": "command"}),
                ),
            ],
            json!([
                {"id": "q1", "kind": "command", "status": "pending",
                 "responseCapability": {"type": "live", "providerSessionId": "s"}},
                {"id": "q2", "kind": "command", "status": "resolved",
                 "responseCapability": {"type": "live", "providerSessionId": "s"}},
                {"id": "q3", "kind": "command", "status": "pending",
                 "responseCapability": {"type": "not_resumable", "reason": "x"}}
            ]),
        );
        assert_eq!(
            kinds(&projection),
            vec![
                EntryKind::Approval {
                    request_id: "q1".into(),
                    kind: "command".into(),
                    prompt: Some("run rm?".into()),
                    pending: true
                },
                EntryKind::Approval {
                    request_id: "q2".into(),
                    kind: "file-change".into(),
                    prompt: Some("edit".into()),
                    pending: false
                },
                EntryKind::Approval {
                    request_id: "q3".into(),
                    kind: "command".into(),
                    prompt: None,
                    pending: false
                },
            ]
        );
    }

    #[test]
    fn tool_and_status_rows() {
        let projection = projection(
            vec![
                item(
                    "1",
                    "file_change",
                    json!({"fileName": "src/a.rs", "additions": 3, "deletions": 1}),
                ),
                item("2", "file_search", json!({"pattern": "*.rs"})),
                item("3", "web_search", json!({"patterns": ["rust", "gpui"]})),
                item("4", "web_search", json!({})),
                item(
                    "5",
                    "proposed_plan",
                    json!({"markdown": "# Plan", "streaming": false}),
                ),
                item(
                    "6",
                    "todo_list",
                    json!({"steps": [
                        {"id": "a", "text": "one", "status": "completed"},
                        {"id": "b", "text": "two", "status": "running"},
                        {"id": "c", "text": "three", "status": "pending"},
                        {"id": "d"}
                    ]}),
                ),
                item(
                    "7",
                    "error",
                    json!({"failure": {"class": "x", "message": "rate limited",
                    "code": null, "retryable": true}}),
                ),
                item("8", "error", json!({})),
                item("9", "system_notice", json!({"message": "heads up"})),
                item(
                    "10",
                    "notification",
                    json!({"summary": "task done", "source": {}, "outcome": "completed"}),
                ),
                item("11", "compaction", json!({"driver": null})),
                item(
                    "12",
                    "checkpoint",
                    json!({"checkpointId": "c", "files": []}),
                ),
                item("13", "dynamic_tool", json!({"title": "Ran lint"})),
                item("14", "teleport_beam", json!({})),
                item(
                    "15",
                    "user_input_request",
                    json!({"requestId": "q", "questions": [
                    {"id": "1", "header": "h", "question": "Which?", "options": []},
                    {"id": "2", "header": "h", "question": "Why?", "options": []}]}),
                ),
            ],
            json!([]),
        );
        assert_eq!(
            kinds(&projection),
            vec![
                EntryKind::FileChange {
                    path: "src/a.rs".into(),
                    additions: Some(3),
                    deletions: Some(1)
                },
                EntryKind::Search {
                    label: "Searched files for *.rs".into()
                },
                EntryKind::Search {
                    label: "Searched the web for rust, gpui".into()
                },
                EntryKind::Search {
                    label: "Searched the web".into()
                },
                EntryKind::Plan {
                    markdown: "# Plan".into(),
                    streaming: false
                },
                EntryKind::Todo {
                    steps: vec![
                        TodoStep {
                            text: "one".into(),
                            status: TodoStatus::Completed
                        },
                        TodoStep {
                            text: "two".into(),
                            status: TodoStatus::Running
                        },
                        TodoStep {
                            text: "three".into(),
                            status: TodoStatus::Pending
                        },
                    ]
                },
                EntryKind::Error {
                    message: "rate limited".into()
                },
                EntryKind::Error {
                    message: "The agent hit an error.".into()
                },
                EntryKind::Notice {
                    text: "heads up".into()
                },
                EntryKind::Notice {
                    text: "task done".into()
                },
                EntryKind::Notice {
                    text: "Context compacted".into()
                },
                EntryKind::Tool {
                    name: "Ran lint".into(),
                    running: false,
                    failed: false
                },
                EntryKind::Other {
                    label: "Teleport beam".into()
                },
                EntryKind::Question {
                    request_id: "q".into(),
                    prompt: "Which?\nWhy?".into(),
                    pending: false
                },
            ]
        );
    }

    #[test]
    fn integration_tools_keep_their_name_and_entries_their_run() {
        let projection = projection(
            vec![
                item(
                    "a",
                    "dynamic_tool",
                    json!({"toolName": "mcp__t3-code__preview_open", "input": {},
                           "startedAt": "2026-11-05T15:00:00Z", "completedAt": "2026-11-05T15:00:01Z"}),
                ),
                item(
                    "b",
                    "dynamic_tool",
                    json!({"status": "running", "toolName": "mcp__x__y"}),
                ),
            ],
            json!([]),
        );
        let entries = build_transcript(&projection);
        assert_eq!(
            entries[0].kind,
            EntryKind::Tool {
                name: "mcp__t3-code__preview_open".into(),
                running: false,
                failed: false
            }
        );
        assert_eq!(
            entries[0].meta,
            EntryMeta {
                run_id: Some("r".into()),
                started_at: Some("2026-11-05T15:00:00Z".into()),
                completed_at: Some("2026-11-05T15:00:01Z".into()),
            }
        );
        assert!(matches!(
            entries[1].kind,
            EntryKind::Tool { running: true, .. }
        ));
    }

    #[test]
    fn runs_summarize_status_and_timing() {
        let mut projection = projection(vec![], json!([]));
        projection.runs = serde_json::from_value(json!([
            {"id": "r1", "status": "completed", "startedAt": "2026-11-05T15:00:00Z",
             "completedAt": "2026-11-05T15:00:58Z"},
            {"id": "r2", "status": "running", "startedAt": "2026-11-05T15:01:00Z", "completedAt": null}
        ]))
        .unwrap();
        let runs = build_runs(&projection);
        assert_eq!(
            runs[0].completed_at.as_deref(),
            Some("2026-11-05T15:00:58Z")
        );
        assert!(!runs[0].is_active());
        assert!(runs[1].is_active());
        assert_eq!(runs[1].completed_at, None);
    }
}
