//! Text and state the window shows, derived from sync state without touching GPUI.

use std::time::Duration;

use t3_backend::BackendStatus;
use t3_client::ConnectionStatus;
use t3_contracts::{ProjectId, ShellSnapshot, ThreadId, ThreadShell};
use t3_state::transcript::TranscriptEntry;

use crate::actions::ComposerTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
    Error,
}

/// A strip above the transcript explaining why the app is not live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub tone: Tone,
    pub text: String,
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 1.0 {
        format!("{}ms", duration.as_millis())
    } else if seconds < 60.0 {
        format!("{}s", seconds.round() as u64)
    } else {
        format!("{}m {}s", duration.as_secs() / 60, duration.as_secs() % 60)
    }
}

/// The banner for the local server (if this environment has one) and the connection.
pub fn connection_banner(
    connection: &ConnectionStatus,
    backend: Option<&BackendStatus>,
) -> Option<Banner> {
    match backend {
        Some(BackendStatus::Starting { attempt: 0 }) => {
            return Some(Banner {
                tone: Tone::Info,
                text: "Starting the local T3 Code server…".into(),
            });
        }
        Some(BackendStatus::Starting { attempt }) => {
            return Some(Banner {
                tone: Tone::Warning,
                text: format!(
                    "Starting the local T3 Code server (attempt {})…",
                    attempt + 1
                ),
            });
        }
        Some(BackendStatus::Restarting {
            retry_in, reason, ..
        }) => {
            return Some(Banner {
                tone: Tone::Error,
                text: format!(
                    "The local server stopped: {reason}. Restarting in {}.",
                    format_duration(*retry_in)
                ),
            });
        }
        Some(BackendStatus::Stopped) => {
            return Some(Banner {
                tone: Tone::Error,
                text: "The local server is stopped.".into(),
            });
        }
        Some(BackendStatus::Failed { reason }) => {
            return Some(Banner {
                tone: Tone::Error,
                text: format!("The local server cannot start: {reason}."),
            });
        }
        Some(BackendStatus::Ready) | None => {}
    }
    match connection {
        ConnectionStatus::Connected => None,
        ConnectionStatus::Connecting { attempt: 1 } => Some(Banner {
            tone: Tone::Info,
            text: "Connecting…".into(),
        }),
        ConnectionStatus::Connecting { attempt } => Some(Banner {
            tone: Tone::Warning,
            text: format!("Reconnecting (attempt {attempt})…"),
        }),
        ConnectionStatus::Waiting {
            retry_in, error, ..
        } => Some(Banner {
            tone: Tone::Error,
            text: format!(
                "Disconnected: {error}. Retrying in {}.",
                format_duration(*retry_in)
            ),
        }),
        ConnectionStatus::Stopped => Some(Banner {
            tone: Tone::Error,
            text: "Disconnected.".into(),
        }),
    }
}

/// An open thread wins; otherwise a picked project starts a new thread.
pub fn composer_target(
    open_thread: Option<&ThreadId>,
    selected_project: Option<&ProjectId>,
) -> ComposerTarget {
    match (open_thread, selected_project) {
        (Some(thread), _) => ComposerTarget::Thread(thread.clone()),
        (None, Some(project)) => ComposerTarget::NewThread(project.clone()),
        (None, None) => ComposerTarget::Nothing,
    }
}

pub fn composer_placeholder(target: &ComposerTarget) -> String {
    match target {
        ComposerTarget::Thread(_) | ComposerTarget::NewThread(_) => "Ask anything".into(),
        ComposerTarget::Nothing => "Add a project to start a thread".into(),
    }
}

/// `claude-opus-5-5` → `Opus 5.5`, `gpt-5.1-codex` → `GPT-5.1 Codex`, with the reasoning effort
/// (`Medium`) when the selection sets one.
pub fn model_label(selection: &t3_contracts::ModelSelection) -> (String, Option<String>) {
    let model = selection.model.trim();
    let name = if let Some(rest) = model.strip_prefix("claude-") {
        let mut words = Vec::new();
        let mut version = Vec::new();
        for part in rest.split('-') {
            if part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty() {
                // Dated snapshots (`20250929`) are not part of the name.
                if part.len() < 6 {
                    version.push(part);
                }
            } else if version.is_empty() {
                words.push(capitalize(part));
            }
        }
        let mut name = words.join(" ");
        if !version.is_empty() {
            name.push(' ');
            name.push_str(&version.join("."));
        }
        name
    } else if let Some(rest) = model.strip_prefix("gpt-") {
        let mut parts = rest.split('-');
        let mut name = format!("GPT-{}", parts.next().unwrap_or_default());
        for part in parts {
            name.push(' ');
            name.push_str(&capitalize(part));
        }
        name
    } else {
        model.to_owned()
    };
    // Options are `[{id, value}]`; older records use a plain object.
    const EFFORT_KEYS: [&str; 3] = ["effort", "reasoningEffort", "reasoning"];
    let effort = selection.options.as_ref().and_then(|options| {
        let value = match options.as_array() {
            Some(list) => list.iter().find_map(|option| {
                let id = option.get("id")?.as_str()?;
                EFFORT_KEYS
                    .contains(&id)
                    .then(|| option.get("value")?.as_str())?
            }),
            None => EFFORT_KEYS
                .iter()
                .find_map(|key| options.get(key)?.as_str()),
        };
        value.map(capitalize)
    });
    (name, effort)
}

fn capitalize(word: &str) -> String {
    let mut glyphs = word.chars();
    match glyphs.next() {
        Some(first) => first.to_uppercase().chain(glyphs).collect(),
        None => String::new(),
    }
}

pub fn find_thread<'a>(
    snapshot: Option<&'a ShellSnapshot>,
    thread_id: &ThreadId,
) -> Option<&'a ThreadShell> {
    snapshot?
        .threads
        .iter()
        .find(|thread| &thread.id == thread_id)
}

pub fn project_title<'a>(
    snapshot: Option<&'a ShellSnapshot>,
    project_id: &ProjectId,
) -> Option<&'a str> {
    snapshot?
        .projects
        .iter()
        .find(|project| &project.id == project_id)
        .map(|project| project.title.as_str())
}

/// The first transcript index whose entry differs; the list re-measures from there.
pub fn first_change(old: &[TranscriptEntry], new: &[TranscriptEntry]) -> Option<usize> {
    let shared = old.iter().zip(new).position(|(old, new)| old != new);
    match shared {
        Some(index) => Some(index),
        None if old.len() != new.len() => Some(old.len().min(new.len())),
        None => None,
    }
}

/// A short environment name for the header.
pub fn environment_name(label: Option<&str>, is_local: bool) -> String {
    match (label, is_local) {
        (Some(label), true) => format!("{label} (local)"),
        (Some(label), false) => label.to_owned(),
        (None, true) => "This computer".into(),
        (None, false) => "Remote environment".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use t3_state::transcript::EntryKind;

    fn entry(key: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            key: key.into(),
            kind: EntryKind::Assistant {
                text: text.into(),
                streaming: false,
            },
            meta: Default::default(),
        }
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(1_400)), "1s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(150)), "2m 30s");
    }

    #[test]
    fn backend_problems_take_precedence() {
        let connected = ConnectionStatus::Connected;
        assert_eq!(connection_banner(&connected, None), None);
        assert_eq!(
            connection_banner(&connected, Some(&BackendStatus::Ready)),
            None
        );
        assert_eq!(
            connection_banner(&connected, Some(&BackendStatus::Starting { attempt: 0 }))
                .map(|b| b.tone),
            Some(Tone::Info)
        );
        assert_eq!(
            connection_banner(&connected, Some(&BackendStatus::Starting { attempt: 2 }))
                .unwrap()
                .text,
            "Starting the local T3 Code server (attempt 3)…"
        );
        let restarting = BackendStatus::Restarting {
            attempt: 1,
            retry_in: Duration::from_secs(2),
            reason: "server exited with exit status: 1".into(),
        };
        assert_eq!(
            connection_banner(&connected, Some(&restarting)).unwrap(),
            Banner {
                tone: Tone::Error,
                text:
                    "The local server stopped: server exited with exit status: 1. Restarting in 2s."
                        .into()
            }
        );
        assert_eq!(
            connection_banner(&connected, Some(&BackendStatus::Stopped))
                .unwrap()
                .tone,
            Tone::Error
        );
        let failed = BackendStatus::Failed {
            reason: "another T3 Code server already uses /home/me/.t3".into(),
        };
        assert_eq!(
            connection_banner(&connected, Some(&failed)).unwrap().text,
            "The local server cannot start: another T3 Code server already uses /home/me/.t3."
        );
    }

    #[test]
    fn connection_states_explain_themselves() {
        assert_eq!(
            connection_banner(&ConnectionStatus::Connecting { attempt: 1 }, None)
                .unwrap()
                .text,
            "Connecting…"
        );
        assert_eq!(
            connection_banner(&ConnectionStatus::Connecting { attempt: 4 }, None)
                .unwrap()
                .text,
            "Reconnecting (attempt 4)…"
        );
        let waiting = ConnectionStatus::Waiting {
            attempt: 3,
            retry_in: Duration::from_secs(8),
            error: "connection closed".into(),
        };
        assert_eq!(
            connection_banner(&waiting, Some(&BackendStatus::Ready))
                .unwrap()
                .text,
            "Disconnected: connection closed. Retrying in 8s."
        );
        assert_eq!(
            connection_banner(&ConnectionStatus::Stopped, None)
                .unwrap()
                .text,
            "Disconnected."
        );
    }

    #[test]
    fn composer_targets_and_placeholders() {
        let thread = ThreadId::new("t");
        let project = ProjectId::new("p");
        assert_eq!(
            composer_target(Some(&thread), Some(&project)),
            ComposerTarget::Thread(thread.clone())
        );
        assert_eq!(
            composer_target(None, Some(&project)),
            ComposerTarget::NewThread(project.clone())
        );
        assert_eq!(composer_target(None, None), ComposerTarget::Nothing);
        assert_eq!(
            composer_placeholder(&ComposerTarget::NewThread(project)),
            "Ask anything"
        );
        assert_eq!(
            composer_placeholder(&ComposerTarget::Thread(thread)),
            "Ask anything"
        );
        assert_eq!(
            composer_placeholder(&ComposerTarget::Nothing),
            "Add a project to start a thread"
        );
    }

    #[test]
    fn model_labels_read_like_the_picker() {
        let label = |model: &str, options: Option<serde_json::Value>| {
            let mut selection = t3_contracts::ModelSelection::new("x", model);
            selection.options = options;
            model_label(&selection)
        };
        assert_eq!(
            label(
                "claude-opus-5-5",
                Some(serde_json::json!({"effort": "medium"}))
            ),
            ("Opus 5.5".into(), Some("Medium".into()))
        );
        assert_eq!(
            label("claude-sonnet-4-5-20250929", None),
            ("Sonnet 4.5".into(), None)
        );
        assert_eq!(label("gpt-5.1-codex", None), ("GPT-5.1 Codex".into(), None));
        assert_eq!(label("gpt-5", None), ("GPT-5".into(), None));
        assert_eq!(
            label(
                "claude-opus-5-5",
                Some(serde_json::json!([{"id": "fastMode", "value": false},
                    {"id": "reasoningEffort", "value": "high"}]))
            ),
            ("Opus 5.5".into(), Some("High".into()))
        );
        assert_eq!(
            label(
                "claude-opus-5-5",
                Some(serde_json::json!([{"id": "fastMode", "value": false}]))
            ),
            ("Opus 5.5".into(), None)
        );
        assert_eq!(label("llama3", None), ("llama3".into(), None));
    }

    #[test]
    fn first_change_finds_where_the_list_must_remeasure() {
        let a = entry("a", "1");
        let b = entry("b", "2");
        let b2 = entry("b", "2 more");
        let c = entry("c", "3");
        assert_eq!(first_change(&[], &[]), None);
        assert_eq!(
            first_change(&[a.clone(), b.clone()], &[a.clone(), b.clone()]),
            None
        );
        assert_eq!(
            first_change(&[a.clone(), b.clone()], &[a.clone(), b2.clone()]),
            Some(1)
        );
        assert_eq!(
            first_change(std::slice::from_ref(&a), &[a.clone(), c.clone()]),
            Some(1)
        );
        assert_eq!(
            first_change(&[a.clone(), c], std::slice::from_ref(&a)),
            Some(1)
        );
        assert_eq!(first_change(std::slice::from_ref(&a), &[]), Some(0));
        assert_eq!(first_change(&[b], &[a]), Some(0));
    }

    #[test]
    fn shell_lookups() {
        let snapshot: ShellSnapshot = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1, "snapshotSequence": 1, "archivedThreads": [],
            "threads": [t3_contracts::fixtures::thread_shell_json("t-1", "p-1")],
            "projects": [t3_contracts::fixtures::project_shell_json("p-1")]
        }))
        .unwrap();
        assert!(find_thread(Some(&snapshot), &ThreadId::new("t-1")).is_some());
        assert!(find_thread(Some(&snapshot), &ThreadId::new("nope")).is_none());
        assert!(find_thread(None, &ThreadId::new("t-1")).is_none());
        assert!(project_title(Some(&snapshot), &ProjectId::new("p-1")).is_some());
        assert_eq!(project_title(Some(&snapshot), &ProjectId::new("x")), None);
    }

    #[test]
    fn environment_names() {
        assert_eq!(environment_name(Some("Mac"), true), "Mac (local)");
        assert_eq!(environment_name(Some("vps"), false), "vps");
        assert_eq!(environment_name(None, true), "This computer");
        assert_eq!(environment_name(None, false), "Remote environment");
    }
}
