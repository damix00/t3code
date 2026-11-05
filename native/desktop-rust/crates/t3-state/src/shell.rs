//! Port of `packages/client-runtime/src/state/shellReducer.ts`: the sidebar's projects and
//! threads, kept current from the shell stream.

use std::collections::{HashMap, HashSet};

use t3_contracts::{
    ProjectShell, ShellSnapshot, ShellStreamItem, ThreadShell, ThreadShellLocation,
};

/// `retainRepositoryIdentity`: enrichment is async, so a project update can arrive without the
/// identity an earlier snapshot resolved. Keep it while the workspace root is unchanged.
fn retain_repository_identity(
    previous: Option<&ProjectShell>,
    mut next: ProjectShell,
) -> ProjectShell {
    if next.repository_identity.is_none()
        && let Some(previous) = previous
        && previous.repository_identity.is_some()
        && previous.workspace_root == next.workspace_root
    {
        next.repository_identity
            .clone_from(&previous.repository_identity);
    }
    next
}

fn upsert_by_id<T, K: PartialEq>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &K) {
    match items
        .iter()
        .position(|candidate| id(candidate) == id(&item))
    {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

/// `reuseUnchangedThreadShells`: a snapshot row without pull request links keeps the links it
/// had, since deferred snapshots arrive before links are filled in.
pub fn reuse_unchanged_thread_shells(
    previous: Option<&ShellSnapshot>,
    mut next: ShellSnapshot,
) -> ShellSnapshot {
    let Some(previous) = previous else {
        return next;
    };
    if previous.threads.is_empty() {
        return next;
    }
    let by_id: HashMap<_, _> = previous
        .threads
        .iter()
        .map(|thread| (&thread.id, thread))
        .collect();
    for thread in &mut next.threads {
        if let Some(prior) = by_id.get(&thread.id)
            && thread.pull_requests.is_none()
            && prior.pull_requests.is_some()
        {
            thread.pull_requests.clone_from(&prior.pull_requests);
        }
    }
    next
}

/// `mergeShellSnapshotProjects`.
///
/// `resolved_roots` is `Some` for metadata-only enrichment refreshes: the structure and sequence
/// stay, and only repository identities of listed roots are taken as-is (including null).
pub fn merge_shell_snapshot_projects(
    previous: Option<&ShellSnapshot>,
    mut next: ShellSnapshot,
    resolved_roots: Option<&[String]>,
) -> ShellSnapshot {
    let Some(previous) = previous else {
        return next;
    };

    if let Some(resolved_roots) = resolved_roots {
        let resolved: HashSet<&str> = resolved_roots.iter().map(String::as_str).collect();
        let next_by_id: HashMap<_, _> = next
            .projects
            .iter()
            .map(|project| (&project.id, project))
            .collect();
        let mut merged = previous.clone();
        for project in &mut merged.projects {
            let Some(candidate) = next_by_id.get(&project.id) else {
                continue;
            };
            if candidate.workspace_root != project.workspace_root {
                continue;
            }
            // A resolved root takes the server's answer, even null; otherwise only fill a gap.
            let take = resolved.contains(project.workspace_root.as_str())
                || (project.repository_identity.is_none()
                    && candidate.repository_identity.is_some());
            if take {
                project
                    .repository_identity
                    .clone_from(&candidate.repository_identity);
            }
        }
        return merged;
    }

    let previous_by_id: HashMap<_, _> = previous
        .projects
        .iter()
        .map(|project| (&project.id, project))
        .collect();
    next.projects = next
        .projects
        .into_iter()
        .map(|project| {
            let prior = previous_by_id.get(&project.id).copied();
            retain_repository_identity(prior, project)
        })
        .collect();
    next
}

/// `applyShellStreamEvent` for one delta. Deltas at or below the snapshot's sequence are stale.
/// Returns whether the snapshot changed.
pub fn apply_shell_delta(snapshot: &mut ShellSnapshot, item: &ShellStreamItem) -> bool {
    let Some(sequence) = item.sequence() else {
        return false;
    };
    if sequence <= snapshot.snapshot_sequence {
        return false;
    }
    match item {
        ShellStreamItem::ProjectUpdated { project, .. } => {
            let previous = snapshot
                .projects
                .iter()
                .find(|candidate| candidate.id == project.id);
            let project = retain_repository_identity(previous, (**project).clone());
            upsert_by_id(&mut snapshot.projects, project, |project| &project.id);
        }
        ShellStreamItem::ProjectRemoved { project_id, .. } => {
            snapshot
                .projects
                .retain(|project| &project.id != project_id);
        }
        ShellStreamItem::ThreadUpdated {
            location, thread, ..
        } => {
            let unchanged = *location == ThreadShellLocation::Active
                && snapshot
                    .threads
                    .iter()
                    .any(|existing| existing == thread.as_ref());
            if !unchanged {
                match location {
                    ThreadShellLocation::Active => upsert_by_id(
                        &mut snapshot.threads,
                        (**thread).clone(),
                        |thread: &ThreadShell| &thread.id,
                    ),
                    ThreadShellLocation::Archive => {
                        snapshot.threads.retain(|existing| existing.id != thread.id);
                    }
                }
                // The archive has its own query. Older servers still send archive-located deltas
                // here; drop them from the shell instead of growing it.
                snapshot
                    .archived_threads
                    .retain(|existing| existing.id != thread.id);
            }
        }
        ShellStreamItem::ThreadRemoved { thread_id, .. } => {
            snapshot.threads.retain(|thread| &thread.id != thread_id);
            snapshot
                .archived_threads
                .retain(|thread| &thread.id != thread_id);
        }
        ShellStreamItem::Synchronized
        | ShellStreamItem::Snapshot { .. }
        | ShellStreamItem::Unknown => {
            return false;
        }
    }
    snapshot.snapshot_sequence = sequence;
    true
}

/// Sync status of one environment's shell, as in `EnvironmentShellStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellStatus {
    #[default]
    Empty,
    Synchronizing,
    Live,
}

/// The sidebar's shell state for one connection.
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    snapshot: Option<ShellSnapshot>,
    status: ShellStatus,
}

impl ShellState {
    pub fn snapshot(&self) -> Option<&ShellSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn status(&self) -> ShellStatus {
        self.status
    }

    /// Where a resubscription should resume.
    pub fn resume_sequence(&self) -> Option<u64> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot_sequence)
    }

    /// Marks the state as catching up after a reconnect. Cached rows stay readable.
    pub fn begin_resync(&mut self) {
        if self.snapshot.is_some() {
            self.status = ShellStatus::Synchronizing;
        }
    }

    /// Applies one stream item. Returns whether anything visible changed.
    pub fn apply(&mut self, item: &ShellStreamItem) -> bool {
        match item {
            ShellStreamItem::Snapshot {
                snapshot,
                resolved_repository_identity_roots,
            } => {
                let next = merge_shell_snapshot_projects(
                    self.snapshot.as_ref(),
                    (**snapshot).clone(),
                    resolved_repository_identity_roots.as_deref(),
                );
                self.snapshot = Some(next);
                self.status = ShellStatus::Live;
                true
            }
            ShellStreamItem::Synchronized => {
                let changed = self.status != ShellStatus::Live && self.snapshot.is_some();
                if self.snapshot.is_some() {
                    self.status = ShellStatus::Live;
                }
                changed
            }
            ShellStreamItem::Unknown => false,
            delta => match self.snapshot.as_mut() {
                Some(snapshot) => apply_shell_delta(snapshot, delta),
                None => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    fn thread(id: &str, project: &str) -> ThreadShell {
        serde_json::from_value(json!({
            "id": id, "projectId": project, "title": id, "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "m"},
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
            "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "latestRunId": null, "activeRunId": null, "status": "idle",
            "pendingRuntimeRequest": null, "latestVisibleMessage": null,
            "createdAt": "2026-11-01T00:00:00Z", "updatedAt": "2026-11-01T00:00:00Z",
            "archivedAt": null, "settledOverride": null, "settledAt": null, "deletedAt": null
        }))
        .unwrap()
    }

    fn project(id: &str, root: &str, identity: Option<Value>) -> ProjectShell {
        let mut value = json!({"id": id, "title": id, "workspaceRoot": root});
        if let Some(identity) = identity {
            value["repositoryIdentity"] = identity;
        }
        serde_json::from_value(value).unwrap()
    }

    fn snapshot(
        sequence: u64,
        projects: Vec<ProjectShell>,
        threads: Vec<ThreadShell>,
    ) -> ShellSnapshot {
        ShellSnapshot {
            schema_version: 1,
            snapshot_sequence: sequence,
            threads,
            archived_threads: Vec::new(),
            projects,
            older_settled: None,
            extra: Default::default(),
        }
    }

    fn thread_updated(
        sequence: u64,
        location: ThreadShellLocation,
        thread: ThreadShell,
    ) -> ShellStreamItem {
        ShellStreamItem::ThreadUpdated {
            sequence,
            location,
            thread: Box::new(thread),
        }
    }

    #[test]
    fn stale_deltas_are_ignored() {
        let mut shell = snapshot(5, vec![], vec![]);
        assert!(!apply_shell_delta(
            &mut shell,
            &thread_updated(5, ThreadShellLocation::Active, thread("t", "p"))
        ));
        assert!(!apply_shell_delta(
            &mut shell,
            &thread_updated(3, ThreadShellLocation::Active, thread("t", "p"))
        ));
        assert!(shell.threads.is_empty());
        assert_eq!(shell.snapshot_sequence, 5);
    }

    #[test]
    fn thread_updates_upsert_in_place_and_append_new() {
        let mut shell = snapshot(1, vec![], vec![thread("a", "p"), thread("b", "p")]);
        let mut renamed = thread("a", "p");
        renamed.title = "renamed".into();
        assert!(apply_shell_delta(
            &mut shell,
            &thread_updated(2, ThreadShellLocation::Active, renamed)
        ));
        assert!(apply_shell_delta(
            &mut shell,
            &thread_updated(3, ThreadShellLocation::Active, thread("c", "p"))
        ));
        let titles: Vec<_> = shell.threads.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["renamed", "b", "c"]);
        assert_eq!(shell.snapshot_sequence, 3);
    }

    #[test]
    fn identical_thread_update_only_advances_sequence() {
        let mut shell = snapshot(1, vec![], vec![thread("a", "p")]);
        shell.archived_threads.push(thread("a", "p"));
        assert!(apply_shell_delta(
            &mut shell,
            &thread_updated(2, ThreadShellLocation::Active, thread("a", "p"))
        ));
        assert_eq!(shell.snapshot_sequence, 2);
        // Unchanged rows short-circuit before the archive cleanup, like upstream.
        assert_eq!(shell.archived_threads.len(), 1);
    }

    #[test]
    fn archive_located_updates_remove_rows_from_both_lists() {
        let mut shell = snapshot(1, vec![], vec![thread("a", "p"), thread("b", "p")]);
        shell.archived_threads.push(thread("a", "p"));
        assert!(apply_shell_delta(
            &mut shell,
            &thread_updated(2, ThreadShellLocation::Archive, thread("a", "p"))
        ));
        assert_eq!(
            shell
                .threads
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"]
        );
        assert!(shell.archived_threads.is_empty());
    }

    #[test]
    fn thread_removed_clears_both_lists() {
        let mut shell = snapshot(1, vec![], vec![thread("a", "p")]);
        shell.archived_threads.push(thread("z", "p"));
        for (sequence, id) in [(2, "a"), (3, "z")] {
            assert!(apply_shell_delta(
                &mut shell,
                &ShellStreamItem::ThreadRemoved {
                    sequence,
                    location: ThreadShellLocation::Active,
                    thread_id: id.into(),
                }
            ));
        }
        assert!(shell.threads.is_empty() && shell.archived_threads.is_empty());
    }

    #[test]
    fn project_updates_keep_resolved_identity_for_the_same_root() {
        let identity = json!({"canonicalKey": "github.com/a/b"});
        let mut shell = snapshot(1, vec![project("p", "/r", Some(identity.clone()))], vec![]);
        apply_shell_delta(
            &mut shell,
            &ShellStreamItem::ProjectUpdated {
                sequence: 2,
                project: Box::new(project("p", "/r", None)),
            },
        );
        assert_eq!(shell.projects[0].repository_identity, Some(identity));

        apply_shell_delta(
            &mut shell,
            &ShellStreamItem::ProjectUpdated {
                sequence: 3,
                project: Box::new(project("p", "/moved", None)),
            },
        );
        assert_eq!(shell.projects[0].repository_identity, None);
        assert_eq!(shell.projects.len(), 1);

        apply_shell_delta(
            &mut shell,
            &ShellStreamItem::ProjectRemoved {
                sequence: 4,
                project_id: "p".into(),
            },
        );
        assert!(shell.projects.is_empty());
    }

    #[test]
    fn authoritative_snapshots_replace_structure_but_keep_identities() {
        let identity = json!({"canonicalKey": "k"});
        let previous = snapshot(
            4,
            vec![
                project("p", "/r", Some(identity.clone())),
                project("gone", "/g", None),
            ],
            vec![thread("old", "p")],
        );
        let next = snapshot(
            9,
            vec![project("p", "/r", None), project("new", "/n", None)],
            vec![],
        );
        let merged = merge_shell_snapshot_projects(Some(&previous), next, None);
        assert_eq!(merged.snapshot_sequence, 9);
        assert!(merged.threads.is_empty());
        let ids: Vec<_> = merged.projects.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["p", "new"]);
        assert_eq!(merged.projects[0].repository_identity, Some(identity));
    }

    #[test]
    fn enrichment_snapshots_only_touch_identities() {
        let previous = snapshot(
            4,
            vec![
                project("resolved", "/a", Some(json!("old"))),
                project("filled", "/b", None),
                project("kept", "/c", Some(json!("mine"))),
                project("moved", "/d", None),
            ],
            vec![thread("t", "resolved")],
        );
        let next = snapshot(
            99,
            vec![
                project("resolved", "/a", None),
                project("filled", "/b", Some(json!("new"))),
                project("kept", "/c", Some(json!("theirs"))),
                project("moved", "/elsewhere", Some(json!("x"))),
                project("extra", "/e", None),
            ],
            vec![],
        );
        let merged = merge_shell_snapshot_projects(Some(&previous), next, Some(&["/a".to_owned()]));
        assert_eq!(merged.snapshot_sequence, 4);
        assert_eq!(merged.threads.len(), 1);
        let identities: Vec<_> = merged
            .projects
            .iter()
            .map(|p| (p.id.as_str(), p.repository_identity.clone()))
            .collect();
        assert_eq!(
            identities,
            vec![
                ("resolved", None),
                ("filled", Some(json!("new"))),
                ("kept", Some(json!("mine"))),
                ("moved", None),
            ]
        );
    }

    #[test]
    fn first_snapshot_is_taken_as_is() {
        let next = snapshot(1, vec![project("p", "/r", None)], vec![]);
        assert_eq!(
            merge_shell_snapshot_projects(None, next.clone(), None),
            next
        );
        assert_eq!(
            merge_shell_snapshot_projects(None, next.clone(), Some(&[])),
            next
        );
    }

    #[test]
    fn reused_rows_keep_their_pull_request_links() {
        let mut with_links = thread("a", "p");
        with_links.pull_requests = Some(json!([{"number": 1}]));
        let previous = snapshot(1, vec![], vec![with_links.clone()]);
        let next = snapshot(2, vec![], vec![thread("a", "p"), thread("b", "p")]);
        let reused = reuse_unchanged_thread_shells(Some(&previous), next);
        assert_eq!(reused.threads[0].pull_requests, with_links.pull_requests);
        assert_eq!(reused.threads[1].pull_requests, None);
    }

    #[test]
    fn shell_state_lifecycle() {
        let mut state = ShellState::default();
        assert_eq!(state.status(), ShellStatus::Empty);
        // Deltas and markers before a snapshot do nothing.
        assert!(!state.apply(&thread_updated(
            1,
            ThreadShellLocation::Active,
            thread("a", "p")
        )));
        assert!(!state.apply(&ShellStreamItem::Synchronized));
        assert_eq!(state.resume_sequence(), None);

        let first = ShellStreamItem::Snapshot {
            snapshot: Box::new(snapshot(3, vec![], vec![])),
            resolved_repository_identity_roots: None,
        };
        assert!(state.apply(&first));
        assert_eq!(state.status(), ShellStatus::Live);
        assert!(state.apply(&thread_updated(
            4,
            ThreadShellLocation::Active,
            thread("a", "p")
        )));
        assert_eq!(state.resume_sequence(), Some(4));

        state.begin_resync();
        assert_eq!(state.status(), ShellStatus::Synchronizing);
        assert_eq!(state.snapshot().unwrap().threads.len(), 1);
        assert!(state.apply(&ShellStreamItem::Synchronized));
        assert_eq!(state.status(), ShellStatus::Live);
        assert!(!state.apply(&ShellStreamItem::Synchronized));
        assert!(!state.apply(&ShellStreamItem::Unknown));
    }

    #[test]
    fn begin_resync_without_cache_stays_empty() {
        let mut state = ShellState::default();
        state.begin_resync();
        assert_eq!(state.status(), ShellStatus::Empty);
    }
}
