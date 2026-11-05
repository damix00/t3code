//! Turns transcript entries into the rows the chat shows: messages, one-line summaries of
//! consecutive tool work ("Ran 2 commands and changed 1 file"), and "Worked for 2m 5s" folds over
//! a finished run. Ports of upstream's `summarizeToolGroup` and the run fold in `MessagesTimeline`.

use std::collections::HashSet;

use t3_state::transcript::{EntryKind, RunSummary, TranscriptEntry};

use crate::sidebar::{format_duration, parse_ms};

/// What a work entry did, for the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Action {
    Command,
    Edit,
    Read,
    Search,
    CodeSearch,
    Browser,
    Other,
}

impl Action {
    fn priority(self) -> u8 {
        match self {
            Self::Command | Self::Edit => 0,
            Self::Other => 2,
            _ => 1,
        }
    }

    fn label(self, count: usize) -> String {
        let plural = |one: &str, many: &str| if count == 1 { one } else { many }.to_owned();
        match self {
            Self::Command => format!("Ran {count} {}", plural("command", "commands")),
            Self::Edit => format!("Changed {count} {}", plural("file", "files")),
            Self::Read => format!("Read {count} {}", plural("file", "files")),
            Self::Search => format!("Searched the web {count} {}", plural("time", "times")),
            Self::CodeSearch => format!("Searched code {count} {}", plural("time", "times")),
            Self::Browser => format!("Used browser {count} {}", plural("time", "times")),
            Self::Other => format!("Used {count} {}", plural("tool", "tools")),
        }
    }
}

/// The icon at the start of a work row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkIcon {
    Terminal,
    Edit,
    Thought,
    Search,
    Tool,
}

/// Whether an entry is agent work that folds into a summary row.
pub fn is_work(kind: &EntryKind) -> bool {
    matches!(
        kind,
        EntryKind::Reasoning { .. }
            | EntryKind::Command { .. }
            | EntryKind::FileChange { .. }
            | EntryKind::Search { .. }
            | EntryKind::Tool { .. }
            | EntryKind::Other { .. }
    )
}

/// `mcp__plugin_playwright_playwright__browser_navigate` → (`Playwright`, `browser_navigate`).
/// T3 Code's own tools are not an integration.
fn mcp_parts(name: &str) -> Option<(Option<String>, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server == "t3-code" || server == "t3" {
        return Some((None, tool));
    }
    let base = server.rsplit('_').next().unwrap_or(server);
    let mut label = String::new();
    for (index, word) in base.split('-').filter(|w| !w.is_empty()).enumerate() {
        if index > 0 {
            label.push(' ');
        }
        let mut glyphs = word.chars();
        if let Some(first) = glyphs.next() {
            label.extend(first.to_uppercase());
            label.push_str(glyphs.as_str());
        }
    }
    Some((Some(label).filter(|l| !l.is_empty()), tool))
}

fn tool_action(tool: &str) -> Action {
    let tool = tool.to_ascii_lowercase();
    if tool.starts_with("preview_") || tool.starts_with("browser_") {
        Action::Browser
    } else if tool.contains("web_search") || tool.contains("websearch") {
        Action::Search
    } else if matches!(tool.as_str(), "grep" | "glob" | "search") {
        Action::CodeSearch
    } else if tool == "read" {
        Action::Read
    } else {
        Action::Other
    }
}

/// `summarizeToolGroup`: one line for consecutive work entries.
pub fn summarize_work(entries: &[&TranscriptEntry]) -> String {
    let thoughts = entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::Reasoning { .. }))
        .count();
    if thoughts == entries.len() {
        return if thoughts > 1 {
            format!("Thought (×{thoughts})")
        } else {
            "Thought".into()
        };
    }

    let mut integrations: Vec<String> = Vec::new();
    // (action, first index, count); edits count distinct files.
    let mut groups: Vec<(Action, usize, usize)> = Vec::new();
    let mut edited: HashSet<&str> = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let action = match &entry.kind {
            EntryKind::Reasoning { .. } => continue,
            EntryKind::Command { .. } => Action::Command,
            EntryKind::FileChange { path, .. } => {
                if !edited.insert(path) {
                    continue;
                }
                Action::Edit
            }
            EntryKind::Search { .. } => Action::Search,
            EntryKind::Tool { name, .. } => match mcp_parts(name) {
                Some((integration, tool)) => {
                    if let Some(integration) = integration
                        && !integrations.contains(&integration)
                    {
                        integrations.push(integration);
                    }
                    tool_action(tool)
                }
                None => tool_action(name),
            },
            _ => Action::Other,
        };
        match groups.iter_mut().find(|(seen, ..)| *seen == action) {
            Some((_, _, count)) => *count += 1,
            None => groups.push((action, index, 1)),
        }
    }

    let mut ranked = groups.clone();
    ranked.sort_by_key(|(action, index, _)| (action.priority(), *index));
    let shown: Vec<_> = ranked.iter().take(2).copied().collect();
    let hidden: usize = ranked.iter().skip(2).map(|(_, _, count)| count).sum();
    let mut shown = shown;
    shown.sort_by_key(|(_, index, _)| *index);

    let mut labels: Vec<String> = Vec::new();
    if !integrations.is_empty() {
        let noun = if integrations.len() == 1 {
            "integration"
        } else {
            "integrations"
        };
        labels.push(format!("Used {} {noun}", join(&integrations)));
    }
    labels.extend(shown.iter().map(|(action, _, count)| action.label(*count)));
    if hidden > 0 {
        let noun = if hidden == 1 { "action" } else { "actions" };
        labels.push(format!("Performed {hidden} other {noun}"));
    }
    for label in labels.iter_mut().skip(1) {
        lowercase_first(label);
    }
    join(&labels)
}

fn lowercase_first(text: &mut String) {
    if let Some(first) = text.chars().next() {
        let lower: String = first.to_lowercase().collect();
        text.replace_range(..first.len_utf8(), &lower);
    }
}

/// `a`, `a and b`, `a, b, and c`.
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

pub fn work_icon(entries: &[&TranscriptEntry]) -> WorkIcon {
    let kinds: Vec<&EntryKind> = entries
        .iter()
        .map(|entry| &entry.kind)
        .filter(|kind| !matches!(kind, EntryKind::Reasoning { .. }))
        .collect();
    if kinds.is_empty() {
        WorkIcon::Thought
    } else if kinds.iter().all(|k| matches!(k, EntryKind::Command { .. })) {
        WorkIcon::Terminal
    } else if kinds
        .iter()
        .all(|k| matches!(k, EntryKind::FileChange { .. }))
    {
        WorkIcon::Edit
    } else if kinds.iter().all(|k| matches!(k, EntryKind::Search { .. })) {
        WorkIcon::Search
    } else {
        WorkIcon::Tool
    }
}

/// One row of the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A message or request drawn on its own; the index into the entries.
    Entry(usize),
    /// Consecutive work in one line.
    Work {
        key: String,
        entries: Vec<usize>,
        summary: String,
        icon: WorkIcon,
        running: bool,
    },
    /// A finished run's work before its answer, collapsed.
    Fold {
        key: String,
        label: String,
        rows: Vec<Row>,
    },
}

impl Row {
    pub fn key(&self, entries: &[TranscriptEntry]) -> String {
        match self {
            Self::Entry(index) => entries[*index].key.clone(),
            Self::Work { key, .. } | Self::Fold { key, .. } => key.clone(),
        }
    }
}

fn running(kind: &EntryKind) -> bool {
    match kind {
        EntryKind::Command { running, .. } | EntryKind::Tool { running, .. } => *running,
        EntryKind::Reasoning { streaming, .. } => *streaming,
        _ => false,
    }
}

/// Groups consecutive work entries in `indices` into summary rows.
fn group_work(entries: &[TranscriptEntry], indices: &[usize]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    let flush = |pending: &mut Vec<usize>, rows: &mut Vec<Row>| {
        if pending.is_empty() {
            return;
        }
        let group: Vec<&TranscriptEntry> = pending.iter().map(|&i| &entries[i]).collect();
        rows.push(Row::Work {
            key: format!("work:{}", group[0].key),
            summary: summarize_work(&group),
            icon: work_icon(&group),
            running: group.iter().any(|entry| running(&entry.kind)),
            entries: std::mem::take(pending),
        });
    };
    for &index in indices {
        if is_work(&entries[index].kind) {
            pending.push(index);
        } else {
            flush(&mut pending, &mut rows);
            rows.push(Row::Entry(index));
        }
    }
    flush(&mut pending, &mut rows);
    rows
}

/// Entries that stay visible even inside a finished run.
fn stays_out_of_fold(kind: &EntryKind) -> bool {
    matches!(
        kind,
        EntryKind::User { .. }
            | EntryKind::Approval { pending: true, .. }
            | EntryKind::Question { pending: true, .. }
            | EntryKind::Error { .. }
    )
}

fn fold_label(run: &RunSummary, latest: bool) -> String {
    let duration = match (
        run.started_at.as_deref().and_then(parse_ms),
        run.completed_at.as_deref().and_then(parse_ms),
    ) {
        (Some(start), Some(end)) => Some(format_duration(end - start)),
        _ => None,
    };
    let stopped = latest && run.status == "interrupted";
    match (stopped, duration) {
        (true, Some(duration)) => format!("You stopped after {duration}"),
        (true, None) => "You stopped".into(),
        (false, Some(duration)) => format!("Worked for {duration}"),
        (false, None) => "Worked".into(),
    }
}

/// Builds the chat rows. A finished run folds everything before its last answer into one
/// "Worked for" row; a live run shows its work as it happens.
pub fn build_rows(entries: &[TranscriptEntry], runs: &[RunSummary]) -> Vec<Row> {
    let latest_run = runs.last().map(|run| run.id.as_str());
    let mut rows = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        let run_id = entries[index].meta.run_id.clone();
        let mut end = index + 1;
        while end < entries.len() && entries[end].meta.run_id == run_id {
            end += 1;
        }
        let indices: Vec<usize> = (index..end).collect();
        index = end;

        let run = run_id
            .as_deref()
            .and_then(|id| runs.iter().find(|run| run.id == id));
        let Some(run) = run.filter(|run| !run.is_active()) else {
            rows.extend(group_work(entries, &indices));
            continue;
        };
        let answer = indices
            .iter()
            .rposition(|&i| matches!(entries[i].kind, EntryKind::Assistant { .. }));
        let Some(answer) = answer.filter(|&position| {
            indices[..position]
                .iter()
                .any(|&i| !stays_out_of_fold(&entries[i].kind))
        }) else {
            rows.extend(group_work(entries, &indices));
            continue;
        };

        let (before, after) = indices.split_at(answer);
        let (kept, folded): (Vec<usize>, Vec<usize>) = before
            .iter()
            .partition(|&&i| stays_out_of_fold(&entries[i].kind));
        rows.extend(kept.into_iter().map(Row::Entry));
        rows.push(Row::Fold {
            key: format!("fold:{}", run.id),
            label: fold_label(run, latest_run == Some(run.id.as_str())),
            rows: group_work(entries, &folded),
        });
        rows.extend(group_work(entries, after));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use t3_state::transcript::EntryMeta;

    fn entry(key: &str, run: Option<&str>, kind: EntryKind) -> TranscriptEntry {
        TranscriptEntry {
            key: key.into(),
            kind,
            meta: EntryMeta {
                run_id: run.map(str::to_owned),
                ..Default::default()
            },
        }
    }

    fn command(key: &str) -> TranscriptEntry {
        entry(
            key,
            None,
            EntryKind::Command {
                command: "ls".into(),
                output: None,
                exit_code: Some(0),
                running: false,
                failed: false,
            },
        )
    }

    fn tool(key: &str, name: &str) -> TranscriptEntry {
        entry(
            key,
            None,
            EntryKind::Tool {
                name: name.into(),
                running: false,
                failed: false,
            },
        )
    }

    fn edit(key: &str, path: &str) -> TranscriptEntry {
        entry(
            key,
            None,
            EntryKind::FileChange {
                path: path.into(),
                additions: None,
                deletions: None,
            },
        )
    }

    fn thought(key: &str) -> TranscriptEntry {
        entry(
            key,
            None,
            EntryKind::Reasoning {
                text: "hmm".into(),
                streaming: false,
            },
        )
    }

    fn summary(entries: &[TranscriptEntry]) -> String {
        summarize_work(&entries.iter().collect::<Vec<_>>())
    }

    #[test]
    fn thoughts_alone_say_thought() {
        assert_eq!(summary(&[thought("a")]), "Thought");
        assert_eq!(summary(&[thought("a"), thought("b")]), "Thought (×2)");
    }

    #[test]
    fn summaries_count_by_action() {
        assert_eq!(summary(&[command("a")]), "Ran 1 command");
        assert_eq!(
            summary(&[command("a"), thought("t"), command("b")]),
            "Ran 2 commands"
        );
        assert_eq!(
            summary(&[
                edit("a", "x.rs"),
                edit("b", "x.rs"),
                edit("c", "y.rs"),
                command("d")
            ]),
            "Changed 2 files and ran 1 command"
        );
    }

    #[test]
    fn integrations_lead_the_summary() {
        assert_eq!(
            summary(&[
                tool("a", "mcp__plugin_playwright_playwright__evaluate"),
                command("c"),
            ]),
            "Used Playwright integration, used 1 tool, and ran 1 command"
        );
        assert_eq!(
            summary(&[tool("a", "mcp__t3-code__preview_snapshot")]),
            "Used browser 1 time"
        );
    }

    #[test]
    fn low_priority_actions_collapse_into_other_actions() {
        assert_eq!(
            summary(&[
                tool("a", "WebSearch"),
                tool("b", "Grep"),
                command("c"),
                edit("d", "x.rs"),
            ]),
            "Ran 1 command, changed 1 file, and performed 2 other actions"
        );
    }

    #[test]
    fn icons_follow_the_group() {
        let refs = |entries: &[TranscriptEntry]| work_icon(&entries.iter().collect::<Vec<_>>());
        assert_eq!(refs(&[command("a"), thought("b")]), WorkIcon::Terminal);
        assert_eq!(refs(&[edit("a", "x")]), WorkIcon::Edit);
        assert_eq!(refs(&[thought("a")]), WorkIcon::Thought);
        assert_eq!(refs(&[edit("a", "x"), command("b")]), WorkIcon::Tool);
    }

    fn in_run(mut entry: TranscriptEntry, run: &str) -> TranscriptEntry {
        entry.meta.run_id = Some(run.into());
        entry
    }

    fn user(key: &str) -> TranscriptEntry {
        entry(key, None, EntryKind::User { text: "hi".into() })
    }

    fn answer(key: &str) -> TranscriptEntry {
        entry(
            key,
            None,
            EntryKind::Assistant {
                text: "done".into(),
                streaming: false,
            },
        )
    }

    fn run(id: &str, status: &str) -> RunSummary {
        RunSummary {
            id: id.into(),
            status: status.into(),
            started_at: Some("2026-11-05T10:00:00Z".into()),
            completed_at: Some("2026-11-05T10:02:05Z".into()),
        }
    }

    #[test]
    fn finished_runs_fold_their_work() {
        let entries = vec![
            in_run(user("u"), "r1"),
            in_run(thought("t"), "r1"),
            in_run(command("c"), "r1"),
            in_run(answer("mid"), "r1"),
            in_run(command("c2"), "r1"),
            in_run(answer("final"), "r1"),
        ];
        let rows = build_rows(&entries, &[run("r1", "completed")]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Row::Entry(0));
        let Row::Fold {
            label, rows: inner, ..
        } = &rows[1]
        else {
            panic!("expected a fold, got {:?}", rows[1]);
        };
        assert_eq!(label, "Worked for 2m 5s");
        assert_eq!(inner.len(), 3);
        assert!(matches!(&inner[0], Row::Work { summary, .. } if summary == "Ran 1 command"));
        assert_eq!(inner[1], Row::Entry(3));
        assert_eq!(rows[2], Row::Entry(5));
    }

    #[test]
    fn interrupted_latest_runs_say_you_stopped() {
        let entries = vec![
            in_run(user("u"), "r1"),
            in_run(command("c"), "r1"),
            in_run(answer("a"), "r1"),
        ];
        let rows = build_rows(&entries, &[run("r1", "interrupted")]);
        assert!(matches!(&rows[1], Row::Fold { label, .. } if label == "You stopped after 2m 5s"));
    }

    #[test]
    fn live_runs_and_answer_only_runs_do_not_fold() {
        let entries = vec![
            in_run(user("u"), "r1"),
            in_run(command("c"), "r1"),
            in_run(command("c2"), "r1"),
        ];
        let rows = build_rows(&entries, &[run("r1", "running")]);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[1], Row::Work { entries, .. } if entries == &[1, 2]));

        let entries = vec![in_run(user("u"), "r1"), in_run(answer("a"), "r1")];
        let rows = build_rows(&entries, &[run("r1", "completed")]);
        assert_eq!(rows, vec![Row::Entry(0), Row::Entry(1)]);
    }

    #[test]
    fn duration_is_unknown_without_timestamps() {
        let mut summary = run("r", "completed");
        summary.completed_at = None;
        assert_eq!(fold_label(&summary, true), "Worked");
    }
}
