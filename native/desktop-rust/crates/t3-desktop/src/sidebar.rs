//! What the thread sidebar shows: which section each thread sits in, in what order, and the
//! labels on its row. Ports of upstream's `Sidebar.logic.ts`, `threadSort.ts`,
//! `threadInbox.ts`, `projectIdentity.ts` and `timestampFormat.ts`.

use chrono::DateTime;
use serde_json::Value;
use t3_contracts::{ShellSnapshot, ThreadId, ThreadShell};

/// Milliseconds since the epoch for an ISO timestamp.
pub fn parse_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn extra_str<'a>(thread: &'a ThreadShell, key: &str) -> Option<&'a str> {
    thread.extra.get(key).and_then(Value::as_str)
}

fn extra_ms(thread: &ThreadShell, key: &str) -> Option<i64> {
    extra_str(thread, key).and_then(parse_ms)
}

fn ms(value: Option<&str>) -> Option<i64> {
    value.and_then(parse_ms)
}

/// `formatDuration` from `orchestrationTiming.ts`.
pub fn format_duration(duration_ms: i64) -> String {
    if duration_ms < 0 {
        return "0ms".into();
    }
    if duration_ms < 1_000 {
        return format!("{}ms", duration_ms.max(1));
    }
    if duration_ms < 10_000 {
        let tenths = (duration_ms as f64 / 100.0).round() / 10.0;
        return format!("{tenths}s");
    }
    let total_seconds = (duration_ms as f64 / 1_000.0).round() as i64;
    if total_seconds < 60 {
        return format!("{total_seconds}s");
    }
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

/// The sidebar's compact relative time: `now`, `5m`, `15h`, `3d`.
pub fn relative_label(iso: &str, now_ms: i64) -> Option<String> {
    let elapsed = (now_ms - parse_ms(iso)?).max(0);
    let seconds = elapsed / 1000;
    Some(if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    })
}

/// `formatElapsedDurationLabel`: how long something has been going.
pub fn elapsed_label(iso: &str, now_ms: i64) -> Option<String> {
    let elapsed = now_ms - parse_ms(iso)?;
    let seconds = elapsed.max(0) / 1000;
    Some(if seconds < 5 {
        "just now".into()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    })
}

/// Tailwind's 400 shades, in `PROJECT_ICON_COLORS` order.
pub const PROJECT_COLORS: [u32; 18] = [
    0x99a1af, // gray
    0xff6467, // red
    0xff8904, // orange
    0xffba00, // amber
    0xfdc700, // yellow
    0x9ae600, // lime
    0x05df72, // green
    0x00d492, // emerald
    0x00d5be, // teal
    0x00d3f2, // cyan
    0x00bcff, // sky
    0x51a2ff, // blue
    0x7c86ff, // indigo
    0xa684ff, // violet
    0xc27aff, // purple
    0xed6aff, // fuchsia
    0xfb64b6, // pink
    0xff637e, // rose
];

fn words(name: &str) -> Vec<&str> {
    name.trim()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Two letters for a project's tile, e.g. `zvec-city` → `ZC`, `t3-code-rust` → `T3`.
pub fn monogram(name: &str) -> String {
    let words = words(name);
    let Some(first_word) = words.first() else {
        return "PR".into();
    };
    let glyphs: Vec<char> = first_word.chars().collect();
    let first = glyphs[0];
    let second = glyphs[1..]
        .iter()
        .copied()
        .find(|glyph| glyph.is_numeric())
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|word| word.chars().next())
            } else {
                glyphs.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

/// The tile color: a stable hash of the lowercased name.
pub fn monogram_color(name: &str) -> u32 {
    let seed = name.trim().to_lowercase();
    let seed = if seed.is_empty() {
        "project".into()
    } else {
        seed
    };
    let mut index: u64 = 0;
    // JavaScript iterates code points; `chars` does the same.
    for glyph in seed.chars() {
        index = (index * 31 + glyph as u64) % PROJECT_COLORS.len() as u64;
    }
    PROJECT_COLORS[index as usize]
}

/// A row's state, most urgent first (`resolveSidebarThreadStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Approval,
    Input,
    Working,
    Failed,
    Limited,
    Ready,
}

const ACTIVE_STATUSES: [&str; 5] = ["preparing", "queued", "starting", "running", "waiting"];

pub fn thread_status(thread: &ThreadShell) -> ThreadStatus {
    if let Some(request) = &thread.pending_runtime_request {
        return if request.kind.contains("input") {
            ThreadStatus::Input
        } else {
            ThreadStatus::Approval
        };
    }
    if ACTIVE_STATUSES.contains(&thread.status.as_str()) {
        return ThreadStatus::Working;
    }
    if thread.status == "failed" {
        return if extra_str(thread, "lastErrorClass") == Some("usage_limit") {
            ThreadStatus::Limited
        } else {
            ThreadStatus::Failed
        };
    }
    ThreadStatus::Ready
}

/// `isThreadWorking`: belongs on the working shelf.
pub fn is_working(thread: &ThreadShell) -> bool {
    thread.pending_runtime_request.is_none() && ACTIVE_STATUSES.contains(&thread.status.as_str())
}

/// Subagents and side chats have no row of their own.
pub fn is_listed(thread: &ThreadShell) -> bool {
    thread.lineage.is_listed()
}

fn is_snoozed(thread: &ThreadShell, now_ms: i64) -> bool {
    ms(thread.snoozed_until.as_deref()).is_some_and(|wake| wake > now_ms)
}

/// Completed since the user last looked.
pub fn is_unread(thread: &ThreadShell) -> bool {
    match (
        extra_ms(thread, "latestRunCompletedAt"),
        extra_ms(thread, "lastVisitedAt"),
    ) {
        (Some(completed), Some(visited)) => completed > visited,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

/// Thread ids per section, each in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sections {
    pub pinned: Vec<ThreadId>,
    pub active: Vec<ThreadId>,
    pub working: Vec<ThreadId>,
    pub snoozed: Vec<ThreadId>,
    pub settled: Vec<ThreadId>,
}

impl Sections {
    pub fn section_of(&self, id: &ThreadId) -> Option<Section> {
        [
            (Section::Pinned, &self.pinned),
            (Section::Active, &self.active),
            (Section::Working, &self.working),
            (Section::Snoozed, &self.snoozed),
            (Section::Settled, &self.settled),
        ]
        .into_iter()
        .find(|(_, ids)| ids.contains(id))
        .map(|(section, _)| section)
    }
}

/// Newest first, then by id, like `sortNewestFirst`.
fn sort_newest_first(threads: &mut [&ThreadShell], key: impl Fn(&ThreadShell) -> i64) {
    threads.sort_by(|left, right| {
        key(right)
            .cmp(&key(left))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
}

fn max_of(values: &[Option<i64>]) -> i64 {
    values.iter().flatten().copied().max().unwrap_or(0)
}

/// When a thread last came back to the inbox (`sortInboxThreadsByReturn`).
fn returned_at(thread: &ThreadShell) -> i64 {
    max_of(&[
        ms(Some(&thread.created_at)),
        extra_ms(thread, "unsettledAt"),
        extra_ms(thread, "latestRunRequestedAt"),
        extra_ms(thread, "latestRunCompletedAt"),
    ])
}

/// When the user last sent to it (`sortWorkingThreadsBySend`).
fn sent_at(thread: &ThreadShell) -> i64 {
    max_of(&[
        ms(Some(&thread.created_at)),
        extra_ms(thread, "latestUserAuthoredMessageAt")
            .or_else(|| extra_ms(thread, "latestRunRequestedAt")),
    ])
}

/// `resolveSettledThreadTimestamp`.
pub fn settled_timestamp(thread: &ThreadShell) -> Option<&str> {
    if let Some(settled) = thread
        .settled_at
        .as_deref()
        .filter(|at| parse_ms(at).is_some())
    {
        return Some(settled);
    }
    [
        "latestUserMessageAt",
        "latestRunRequestedAt",
        "latestRunStartedAt",
        "latestRunCompletedAt",
    ]
    .into_iter()
    .filter_map(|key| extra_str(thread, key))
    .filter_map(|at| parse_ms(at).map(|ms| (ms, at)))
    .max_by_key(|(ms, _)| *ms)
    .map(|(_, at)| at)
    .or_else(|| Some(thread.updated_at.as_str()).filter(|at| parse_ms(at).is_some()))
}

/// The time a row shows: when the user last wrote to it.
pub fn row_timestamp(thread: &ThreadShell) -> &str {
    extra_str(thread, "latestUserMessageAt").unwrap_or(&thread.updated_at)
}

/// When the current run started, for the "Working 11m" label.
pub fn working_since(thread: &ThreadShell) -> Option<&str> {
    extra_str(thread, "activityRunStartedAt").or_else(|| extra_str(thread, "latestRunStartedAt"))
}

/// `partitionSidebarThreads` with the working shelf on.
pub fn sections(snapshot: &ShellSnapshot, now_ms: i64) -> Sections {
    let mut pinned = Vec::new();
    let mut active = Vec::new();
    let mut working = Vec::new();
    let mut snoozed = Vec::new();
    let mut settled = Vec::new();
    for thread in snapshot
        .threads
        .iter()
        .filter(|thread| thread.archived_at.is_none() && thread.deleted_at.is_none())
        .filter(|thread| is_listed(thread))
    {
        if is_snoozed(thread, now_ms) {
            snoozed.push(thread);
        } else if thread.settled_override.as_deref() == Some("settled") {
            settled.push(thread);
        } else if thread.pinned_at.is_some() {
            pinned.push(thread);
        } else if is_working(thread) {
            working.push(thread);
        } else {
            active.push(thread);
        }
    }
    pinned.sort_by(|left, right| {
        let order = |thread: &ThreadShell| extra_str(thread, "pinOrderKey").map(str::to_owned);
        order(left)
            .cmp(&order(right))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    sort_newest_first(&mut active, returned_at);
    sort_newest_first(&mut working, sent_at);
    snoozed.sort_by_key(|thread| ms(thread.snoozed_until.as_deref()).unwrap_or(0));
    sort_newest_first(&mut settled, |thread| {
        settled_timestamp(thread).and_then(parse_ms).unwrap_or(0)
    });
    let ids = |threads: Vec<&ThreadShell>| threads.into_iter().map(|t| t.id.clone()).collect();
    Sections {
        pinned: ids(pinned),
        active: ids(active),
        working: ids(working),
        snoozed: ids(snoozed),
        settled: ids(settled),
    }
}

/// The pull request number linked to a thread, if any.
pub fn pull_request_number(thread: &ThreadShell) -> Option<u64> {
    thread
        .pull_requests
        .as_ref()?
        .as_array()?
        .iter()
        .find_map(|pr| pr.get("number").and_then(Value::as_u64))
}

/// Everything a thread card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    pub id: ThreadId,
    pub project_id: t3_contracts::ProjectId,
    pub project: String,
    pub monogram: String,
    pub color: u32,
    pub title: String,
    /// `15h`, when not working.
    pub time: Option<String>,
    pub branch: Option<String>,
    pub pull_request: Option<u64>,
    pub status: ThreadStatus,
    /// `8m`, while working.
    pub working_for: Option<String>,
    pub unread: bool,
}

/// The sidebar's cards per section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarModel {
    pub pinned: Vec<ThreadRow>,
    pub active: Vec<ThreadRow>,
    pub working: Vec<ThreadRow>,
    pub snoozed: Vec<ThreadRow>,
    pub settled: Vec<ThreadRow>,
}

impl SidebarModel {
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty()
            && self.active.is_empty()
            && self.working.is_empty()
            && self.snoozed.is_empty()
            && self.settled.is_empty()
    }

    pub fn find(&self, id: &ThreadId) -> Option<&ThreadRow> {
        [
            &self.pinned,
            &self.active,
            &self.working,
            &self.snoozed,
            &self.settled,
        ]
        .into_iter()
        .flatten()
        .find(|row| &row.id == id)
    }

    /// Rows whose title or project contains `query`, ignoring case, in section order.
    pub fn search(&self, query: &str) -> Vec<&ThreadRow> {
        let query = query.trim().to_lowercase();
        [
            &self.pinned,
            &self.working,
            &self.active,
            &self.snoozed,
            &self.settled,
        ]
        .into_iter()
        .flatten()
        .filter(|row| {
            row.title.to_lowercase().contains(&query) || row.project.to_lowercase().contains(&query)
        })
        .collect()
    }
}

fn thread_row(thread: &ThreadShell, snapshot: &ShellSnapshot, now_ms: i64) -> ThreadRow {
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
        .map(|project| project.title.clone())
        .unwrap_or_default();
    let status = thread_status(thread);
    let working_for = (status == ThreadStatus::Working)
        .then(|| working_since(thread).and_then(|at| elapsed_label(at, now_ms)))
        .flatten();
    ThreadRow {
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        monogram: monogram(&project),
        color: monogram_color(&project),
        project,
        title: thread.title.clone(),
        time: relative_label(row_timestamp(thread), now_ms),
        branch: thread.branch.clone().filter(|branch| !branch.is_empty()),
        pull_request: pull_request_number(thread),
        status,
        working_for,
        unread: is_unread(thread),
    }
}

/// Builds every section's cards.
pub fn build(snapshot: &ShellSnapshot, now_ms: i64) -> SidebarModel {
    let sections = sections(snapshot, now_ms);
    let rows = |ids: &[ThreadId]| {
        ids.iter()
            .filter_map(|id| snapshot.threads.iter().find(|thread| &thread.id == id))
            .map(|thread| thread_row(thread, snapshot, now_ms))
            .collect()
    };
    SidebarModel {
        pinned: rows(&sections.pinned),
        active: rows(&sections.active),
        working: rows(&sections.working),
        snoozed: rows(&sections.snoozed),
        settled: rows(&sections.settled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    const NOW: &str = "2026-11-05T15:00:00Z";

    fn now() -> i64 {
        parse_ms(NOW).unwrap()
    }

    fn thread(id: &str, fields: Value) -> Value {
        let mut base = t3_contracts::fixtures::thread_shell_json(id, "p");
        base.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        base
    }

    fn snapshot(threads: Vec<Value>) -> ShellSnapshot {
        serde_json::from_value(json!({
            "schemaVersion": 2, "snapshotSequence": 1, "archivedThreads": [],
            "threads": threads, "projects": [t3_contracts::fixtures::project_shell_json("p")]
        }))
        .unwrap()
    }

    #[test]
    fn durations_match_upstream() {
        assert_eq!(format_duration(-5), "0ms");
        assert_eq!(format_duration(0), "1ms");
        assert_eq!(format_duration(420), "420ms");
        assert_eq!(format_duration(1_250), "1.3s");
        assert_eq!(format_duration(9_960), "10s");
        assert_eq!(format_duration(58_000), "58s");
        assert_eq!(format_duration(59_500), "1m");
        assert_eq!(format_duration(125_000), "2m 5s");
        assert_eq!(format_duration(3_600_000 + 60_000), "1h 1m");
    }

    #[test]
    fn relative_and_elapsed_labels() {
        let at = |seconds_ago: i64| {
            DateTime::from_timestamp_millis(now() - seconds_ago * 1000)
                .unwrap()
                .to_rfc3339()
        };
        assert_eq!(relative_label(&at(10), now()).as_deref(), Some("now"));
        assert_eq!(relative_label(&at(9 * 60), now()).as_deref(), Some("9m"));
        assert_eq!(
            relative_label(&at(15 * 3600 + 5), now()).as_deref(),
            Some("15h")
        );
        assert_eq!(
            relative_label(&at(3 * 86_400), now()).as_deref(),
            Some("3d")
        );
        assert_eq!(relative_label("garbage", now()), None);
        assert_eq!(elapsed_label(&at(2), now()).as_deref(), Some("just now"));
        assert_eq!(elapsed_label(&at(42), now()).as_deref(), Some("42s"));
        assert_eq!(elapsed_label(&at(11 * 60), now()).as_deref(), Some("11m"));
        // Clock skew never shows a negative time.
        assert_eq!(elapsed_label(&at(-30), now()).as_deref(), Some("just now"));
    }

    #[test]
    fn monograms_follow_project_identity() {
        assert_eq!(monogram("zvec-city"), "ZC");
        assert_eq!(monogram("t3-code-rust"), "T3");
        assert_eq!(monogram("doorbell-cam"), "DC");
        assert_eq!(monogram("app"), "AP");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("  --  "), "PR");
        assert_eq!(monogram("élan vital"), "ÉV");
    }

    #[test]
    fn monogram_colors_are_stable_hashes() {
        // Measured from the web client: zvec-city is orange-400.
        assert_eq!(monogram_color("zvec-city"), 0xff8904);
        assert_eq!(monogram_color("Zvec-City"), monogram_color("zvec-city"));
        assert_eq!(monogram_color(""), monogram_color("project"));
    }

    #[test]
    fn sections_follow_upstream_precedence_and_order() {
        let snapshot = snapshot(vec![
            thread(
                "settled-old",
                json!({"settledOverride": "settled",
                "settledAt": "2026-11-01T00:00:00Z"}),
            ),
            thread(
                "settled-new",
                json!({"settledOverride": "settled",
                "settledAt": "2026-11-04T00:00:00Z"}),
            ),
            thread(
                "working",
                json!({"status": "running",
                "latestUserAuthoredMessageAt": "2026-11-05T14:00:00Z"}),
            ),
            thread("active-old", json!({"createdAt": "2026-11-01T00:00:00Z"})),
            thread(
                "active-new",
                json!({"createdAt": "2026-11-01T00:00:00Z",
                "latestRunCompletedAt": "2026-11-05T12:00:00Z"}),
            ),
            thread(
                "pinned",
                json!({"pinnedAt": "2026-11-02T00:00:00Z",
                "settledOverride": null}),
            ),
            thread(
                "snoozed",
                json!({"snoozedUntil": "2026-11-06T00:00:00Z",
                "settledOverride": "settled"}),
            ),
            thread("woke", json!({"snoozedUntil": "2026-11-05T14:00:00Z"})),
            thread(
                "subagent",
                json!({"lineage": {"parentThreadId": "working",
                "relationshipToParent": "subagent", "rootThreadId": "working"}}),
            ),
            thread("archived", json!({"archivedAt": "2026-11-02T00:00:00Z"})),
        ]);
        let sections = sections(&snapshot, now());
        let names = |ids: &[ThreadId]| {
            ids.iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&sections.pinned), ["pinned"]);
        assert_eq!(names(&sections.working), ["working"]);
        assert_eq!(names(&sections.snoozed), ["snoozed"]);
        assert_eq!(names(&sections.settled), ["settled-new", "settled-old"]);
        assert_eq!(names(&sections.active)[0], "active-new");
        assert!(names(&sections.active).contains(&"woke".to_owned()));
        assert!(!names(&sections.active).contains(&"subagent".to_owned()));
        assert_eq!(
            sections.section_of(&ThreadId::new("snoozed")),
            Some(Section::Snoozed)
        );
        assert_eq!(sections.section_of(&ThreadId::new("archived")), None);
    }

    #[test]
    fn statuses_rank_requests_over_activity() {
        let parse =
            |fields: Value| -> ThreadShell { serde_json::from_value(thread("t", fields)).unwrap() };
        assert_eq!(
            thread_status(&parse(json!({"status": "running"}))),
            ThreadStatus::Working
        );
        assert_eq!(
            thread_status(&parse(json!({"status": "completed"}))),
            ThreadStatus::Ready
        );
        assert_eq!(
            thread_status(&parse(json!({"status": "failed"}))),
            ThreadStatus::Failed
        );
        assert_eq!(
            thread_status(&parse(
                json!({"status": "failed", "lastErrorClass": "usage_limit"})
            )),
            ThreadStatus::Limited
        );
        let unread = parse(json!({"latestRunCompletedAt": "2026-11-05T12:00:00Z",
            "lastVisitedAt": "2026-11-05T11:00:00Z"}));
        assert!(is_unread(&unread));
        let seen = parse(json!({"latestRunCompletedAt": "2026-11-05T12:00:00Z",
            "lastVisitedAt": "2026-11-05T13:00:00Z"}));
        assert!(!is_unread(&seen));
    }

    #[test]
    fn rows_show_the_latest_user_message_time() {
        let thread: ThreadShell = serde_json::from_value(thread(
            "t",
            json!({"latestUserMessageAt": "2026-11-05T14:00:00Z", "updatedAt": "2026-11-05T14:30:00Z",
                   "activityRunStartedAt": "2026-11-05T14:00:01Z",
                   "pullRequests": [{"number": 29, "state": "merged"}]}),
        ))
        .unwrap();
        assert_eq!(row_timestamp(&thread), "2026-11-05T14:00:00Z");
        assert_eq!(working_since(&thread), Some("2026-11-05T14:00:01Z"));
        assert_eq!(pull_request_number(&thread), Some(29));
    }

    #[test]
    fn cards_carry_project_identity_and_timing() {
        let mut project = t3_contracts::fixtures::project_shell_json("p");
        project["title"] = json!("zvec-city");
        let snapshot: ShellSnapshot = serde_json::from_value(json!({
            "schemaVersion": 2, "snapshotSequence": 1, "archivedThreads": [],
            "projects": [project],
            "threads": [
                thread("idle", json!({"title": "Delete Local Branches", "branch": "feature/x",
                    "latestUserMessageAt": "2026-11-05T00:00:00Z",
                    "pullRequests": [{"number": 29}]})),
                thread("busy", json!({"status": "running",
                    "activityRunStartedAt": "2026-11-05T14:52:00Z"})),
            ]
        }))
        .unwrap();
        let model = build(&snapshot, now());
        let idle = model.find(&ThreadId::new("idle")).unwrap();
        assert_eq!(idle.monogram, "ZC");
        assert_eq!(idle.color, 0xff8904);
        assert_eq!(idle.time.as_deref(), Some("15h"));
        assert_eq!(idle.branch.as_deref(), Some("feature/x"));
        assert_eq!(idle.pull_request, Some(29));
        assert_eq!(idle.working_for, None);
        let busy = &model.working[0];
        assert_eq!(busy.status, ThreadStatus::Working);
        assert_eq!(busy.working_for.as_deref(), Some("8m"));
        assert_eq!(model.search("DELETE").len(), 1);
        assert_eq!(model.search("zvec").len(), 2);
        assert!(model.search("nothing").is_empty());
        assert!(!model.is_empty());
    }
}
