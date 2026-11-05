//! Ports of `packages/client-runtime/src/state/shellReducer.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use t3_contracts::{ProjectShell, ShellSnapshot, ShellStreamItem, ThreadShell};
use t3_state::shell::{
    apply_shell_delta, merge_shell_snapshot_projects, reuse_unchanged_thread_shells,
};

fn identity(name: &str) -> Value {
    json!({"canonicalKey": format!("github.com/example/{name}"),
        "locator": {"source": "git-remote", "remoteName": "origin",
            "remoteUrl": format!("https://github.com/example/{name}.git")}})
}

fn project(fields: Value) -> ProjectShell {
    let mut value = json!({"id": "project-1", "title": "Project", "workspaceRoot": "/workspace/project",
        "defaultModelSelection": null, "scripts": [], "createdAt": "2026-01-01T00:00:00.000Z",
        "updatedAt": "2026-01-01T00:00:00.000Z"});
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

fn thread(fields: Value) -> ThreadShell {
    let mut value = json!({
        "id": "thread-1", "projectId": "project-1", "title": "Thread",
        "providerInstanceId": "codex", "modelSelection": {"instanceId": "codex", "model": "m"},
        "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
        "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1"},
        "latestRunId": null, "activeRunId": null, "status": "idle",
        "pendingRuntimeRequest": null, "latestVisibleMessage": null,
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
        "archivedAt": null, "settledOverride": null, "settledAt": null, "deletedAt": null
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

fn shell(sequence: u64, projects: Vec<ProjectShell>, threads: Vec<ThreadShell>) -> ShellSnapshot {
    ShellSnapshot {
        schema_version: 1,
        snapshot_sequence: sequence,
        threads,
        archived_threads: vec![],
        projects,
        older_settled: None,
        extra: Default::default(),
    }
}

fn update(sequence: u64, thread: &ThreadShell, location: &str) -> ShellStreamItem {
    serde_json::from_value(json!({"kind": "thread.updated", "sequence": sequence,
        "location": location, "thread": thread}))
    .unwrap()
}

fn remove(sequence: u64, id: &str) -> ShellStreamItem {
    serde_json::from_value(json!({"kind": "thread.removed", "sequence": sequence,
        "location": "active", "threadId": id}))
    .unwrap()
}

fn project_updated(sequence: u64, project: &ProjectShell) -> ShellStreamItem {
    serde_json::from_value(
        json!({"kind": "project.updated", "sequence": sequence, "project": project}),
    )
    .unwrap()
}

fn apply_all(mut snapshot: ShellSnapshot, events: &[ShellStreamItem]) -> ShellSnapshot {
    for event in events {
        apply_shell_delta(&mut snapshot, event);
    }
    snapshot
}

fn ids(threads: &[ThreadShell]) -> Vec<&str> {
    threads.iter().map(|thread| thread.id.as_str()).collect()
}

#[test]
fn ignores_stale_project_updates_without_mutating_the_snapshot() {
    let mut snapshot = shell(5, vec![project(json!({}))], vec![]);
    let before = snapshot.clone();
    assert!(!apply_shell_delta(
        &mut snapshot,
        &project_updated(4, &project(json!({"title": "Stale"})))
    ));
    assert_eq!(snapshot, before);
}

#[test]
fn authoritative_lower_sequence_reset_replaces_client_ahead_structural_state() {
    let unchanged =
        project(json!({"id": "project-unchanged", "repositoryIdentity": identity("repo")}));
    let previous = shell(
        9,
        vec![
            project(json!({"title": "Client ahead", "repositoryIdentity": identity("repo")})),
            unchanged.clone(),
        ],
        vec![thread(json!({"title": "Local-only thread"}))],
    );
    let next = merge_shell_snapshot_projects(
        Some(&previous),
        shell(
            3,
            vec![
                project(json!({"title": "Server reset", "repositoryIdentity": null})),
                unchanged.clone(),
            ],
            vec![thread(json!({"title": "Server thread"}))],
        ),
        None,
    );
    assert_eq!(next.snapshot_sequence, 3);
    assert_eq!(next.projects[0].title, "Server reset");
    assert_eq!(next.projects[0].repository_identity, Some(identity("repo")));
    assert_eq!(next.projects[1], unchanged);
    assert_eq!(next.threads[0].title, "Server thread");
}

#[test]
fn client_ahead_survives_lower_marked_enrichment_then_resets_on_unmarked_authoritative() {
    let previous = shell(
        20,
        vec![project(
            json!({"title": "Client ahead", "repositoryIdentity": identity("repo")}),
        )],
        vec![thread(json!({"title": "Local-only thread"}))],
    );
    let server = shell(
        3,
        vec![project(
            json!({"title": "Server reset", "repositoryIdentity": null}),
        )],
        vec![thread(json!({"title": "Server thread"}))],
    );
    let enriched = merge_shell_snapshot_projects(
        Some(&previous),
        server.clone(),
        Some(&["/workspace/project".to_owned()]),
    );
    assert_eq!(enriched.snapshot_sequence, 20);
    assert_eq!(enriched.projects[0].title, "Client ahead");
    assert_eq!(enriched.projects[0].repository_identity, None);
    assert_eq!(enriched.threads[0].title, "Local-only thread");

    let authoritative = merge_shell_snapshot_projects(Some(&enriched), server, None);
    assert_eq!(authoritative.snapshot_sequence, 3);
    assert_eq!(authoritative.projects[0].title, "Server reset");
    assert_eq!(authoritative.projects[0].repository_identity, None);
    assert_eq!(authoritative.threads[0].title, "Server thread");
}

#[test]
fn takes_identity_from_a_lower_sequence_enrichment_without_reverting_live_thread_state() {
    let previous = shell(
        5,
        vec![project(json!({"repositoryIdentity": null}))],
        vec![thread(json!({"title": "Newest title"}))],
    );
    let next = merge_shell_snapshot_projects(
        Some(&previous),
        shell(
            4,
            vec![project(json!({"repositoryIdentity": identity("repo")}))],
            vec![thread(json!({"title": "Stale title"}))],
        ),
        Some(&["/workspace/project".to_owned()]),
    );
    assert_eq!(next.snapshot_sequence, 5);
    assert_eq!(next.threads[0].title, "Newest title");
    assert_eq!(next.projects[0].repository_identity, Some(identity("repo")));
}

#[test]
fn lower_sequence_enrichment_cannot_resurrect_empty_structural_state() {
    let previous = shell(7, vec![], vec![]);
    let next = merge_shell_snapshot_projects(
        Some(&previous),
        shell(
            4,
            vec![project(json!({"repositoryIdentity": identity("repo")}))],
            vec![thread(json!({}))],
        ),
        Some(&["/workspace/project".to_owned()]),
    );
    assert_eq!(next.snapshot_sequence, 7);
    assert!(next.projects.is_empty());
    assert!(next.threads.is_empty());
}

#[test]
fn resolved_null_clears_identity_only_for_the_marked_root() {
    let other = project(
        json!({"id": "project-other", "workspaceRoot": "/workspace/other",
        "repositoryIdentity": identity("other")}),
    );
    let previous = shell(
        5,
        vec![
            project(json!({"repositoryIdentity": identity("repo")})),
            other.clone(),
        ],
        vec![],
    );
    let mut other_cleared = other.clone();
    other_cleared.repository_identity = None;
    let next = merge_shell_snapshot_projects(
        Some(&previous),
        shell(
            4,
            vec![project(json!({"repositoryIdentity": null})), other_cleared],
            vec![],
        ),
        Some(&["/workspace/project".to_owned()]),
    );
    assert_eq!(next.snapshot_sequence, 5);
    assert_eq!(next.projects.len(), 2);
    assert_eq!(next.projects[0].repository_identity, None);
    assert_eq!(
        next.projects[1].repository_identity,
        Some(identity("other"))
    );
}

#[test]
fn does_not_replace_a_live_identity_with_one_from_an_unmarked_lower_sequence_enrichment() {
    let next = merge_shell_snapshot_projects(
        Some(&shell(
            5,
            vec![project(json!({"repositoryIdentity": identity("live")}))],
            vec![],
        )),
        shell(
            4,
            vec![project(json!({"repositoryIdentity": identity("stale")}))],
            vec![],
        ),
        Some(&[]),
    );
    assert_eq!(next.snapshot_sequence, 5);
    assert_eq!(next.projects[0].repository_identity, Some(identity("live")));
}

#[test]
fn keeps_archive_deltas_out_of_the_normal_shell_cache() {
    let base = thread(json!({}));
    let archived_row = thread(json!({"archivedAt": "2026-01-01T00:00:00.000Z"}));
    let archived = apply_all(
        shell(1, vec![], vec![]),
        &[update(3, &archived_row, "archive")],
    );
    assert!(archived.threads.is_empty());
    assert!(archived.archived_threads.is_empty());
    let active = apply_all(archived, &[update(4, &base, "active")]);
    assert_eq!(active.threads.len(), 1);
    assert!(active.archived_threads.is_empty());
}

#[test]
fn leaves_unknown_future_events_unchanged() {
    let item: ShellStreamItem =
        serde_json::from_value(json!({"kind": "future.event", "sequence": 99})).unwrap();
    let mut snapshot = shell(1, vec![], vec![thread(json!({}))]);
    let before = snapshot.clone();
    assert!(!apply_shell_delta(&mut snapshot, &item));
    assert_eq!(snapshot, before);
}

#[test]
fn reuse_keeps_deferred_links_and_explicit_removals() {
    let linked =
        thread(json!({"pullRequests": [{"host": "github.com", "repository": "a/b", "number": 1}]}));
    // Unchanged row arriving without its deferred links keeps them.
    let mut without_links = linked.clone();
    without_links.pull_requests = None;
    let next = reuse_unchanged_thread_shells(
        Some(&shell(1, vec![], vec![linked.clone()])),
        shell(2, vec![], vec![without_links.clone()]),
    );
    assert_eq!(next.threads[0], linked);
    // A status change still keeps the deferred links.
    let mut running = without_links;
    running.status = "running".into();
    let next = reuse_unchanged_thread_shells(
        Some(&shell(1, vec![], vec![linked.clone()])),
        shell(2, vec![], vec![running]),
    );
    assert_eq!(next.threads[0].pull_requests, linked.pull_requests);
    assert_eq!(next.threads[0].status, "running");
    // An explicit removal (empty array) stays removed.
    let mut removed = linked.clone();
    removed.title = "Renamed".into();
    removed.pull_requests = Some(json!([]));
    let next = reuse_unchanged_thread_shells(
        Some(&shell(1, vec![], vec![linked])),
        shell(2, vec![], vec![removed.clone()]),
    );
    assert_eq!(next.threads[0], removed);
}

#[test]
fn matches_sequential_mixed_deltas_including_remove_reinsert_order_and_stale_events() {
    let row = |id: &str| thread(json!({"id": id}));
    let (a, b, unchanged, archived, old_archive, added) = (
        row("a"),
        row("b"),
        row("unchanged"),
        row("archived"),
        row("old-archive"),
        row("new"),
    );
    let other_project = project(json!({"id": "other-project"}));
    let mut seed = shell(
        4,
        vec![
            project(json!({"repositoryIdentity": identity("repo")})),
            other_project.clone(),
        ],
        vec![a.clone(), b.clone(), unchanged.clone()],
    );
    seed.archived_threads = vec![archived.clone(), old_archive.clone()];
    let mut renamed_b = b.clone();
    renamed_b.title = "Renamed".into();
    let events = vec![
        update(5, &renamed_b, "active"),
        remove(5, "a"),
        remove(6, "a"),
        update(7, &a, "active"),
        update(8, &b, "archive"),
        update(9, &archived, "active"),
        update(10, &old_archive, "archive"),
        update(11, &added, "active"),
        remove(12, "missing"),
        remove(11, "new"),
        remove(13, "new"),
        update(14, &added, "active"),
        update(15, &archived, "active"),
        project_updated(16, &project(json!({"title": "Updated"}))),
        project_updated(17, &project(json!({"workspaceRoot": "/moved"}))),
        serde_json::from_value(
            json!({"kind": "project.removed", "sequence": 18, "projectId": "project-1"}),
        )
        .unwrap(),
        project_updated(19, &project(json!({}))),
    ];
    let next = apply_all(seed.clone(), &events);
    assert_eq!(
        ids(&next.threads),
        vec!["unchanged", "a", "archived", "new"]
    );
    assert_eq!(next.threads[0], unchanged);
    assert_eq!(next.projects[0], other_project);
    assert!(next.archived_threads.is_empty());
    assert_eq!(next.snapshot_sequence, 19);
    // Identity retention uses the latest project in the batch.
    assert_eq!(
        apply_all(seed.clone(), &events[..14]).projects[0].repository_identity,
        Some(identity("repo"))
    );
    assert_eq!(
        apply_all(seed, &events[..15]).projects[0].repository_identity,
        None
    );
}

#[test]
fn equal_rows_only_advance_the_cursor_and_stale_batches_do_nothing() {
    let (a, b) = (thread(json!({"id": "a"})), thread(json!({"id": "b"})));
    let seed = shell(4, vec![], vec![a.clone(), b.clone()]);
    let next = apply_all(
        seed.clone(),
        &[update(5, &a, "active"), update(6, &b, "active")],
    );
    assert_eq!(next.threads, seed.threads);
    assert_eq!(next.snapshot_sequence, 6);
    assert_eq!(
        apply_all(seed.clone(), &[remove(3, "a"), update(4, &b, "active")]),
        seed
    );
    assert_eq!(apply_all(seed.clone(), &[]), seed);
}
