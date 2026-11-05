//! Ports of `packages/client-runtime/src/state/orchestrationV2Projection.test.ts`.
//!
//! Upstream cases that assert object identity (`toBe`) assert equality here, and the two
//! performance cases that count getter reads are covered by `upsert_cost` below instead.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use t3_contracts::{DomainEvent, ThreadProjection};
use t3_state::projection::{ApplyOptions, Outcome, apply_event};

const NOW: &str = "2026-06-20T00:00:00.000Z";
const THREAD: &str = "thread-reducer";
const RUN: &str = "run-reducer";

fn run(id: &str, status: &str, ordinal: u64) -> Value {
    json!({
        "id": id, "threadId": THREAD, "ordinal": ordinal, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
        "providerThreadId": null, "userMessageId": "message-reducer", "rootNodeId": null,
        "activeAttemptId": null, "status": status, "requestedAt": NOW, "startedAt": NOW,
        "completedAt": NOW, "checkpointId": null, "contextHandoffId": null
    })
}

fn command_item(id: &str, output: &str, ordinal: u64) -> Value {
    json!({
        "id": id, "threadId": THREAD, "runId": RUN, "nodeId": null, "providerThreadId": null,
        "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
        "ordinal": ordinal, "status": "completed", "title": null, "startedAt": NOW,
        "completedAt": NOW, "updatedAt": NOW, "type": "command_execution", "input": "pwd",
        "output": output, "exitCode": 0
    })
}

fn with(mut value: Value, fields: Value) -> Value {
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    value
}

fn row(position: u64, visibility: &str, source_thread: &str, item: &Value) -> Value {
    json!({"position": position, "visibility": visibility, "sourceThreadId": source_thread,
           "sourceItemId": item["id"], "item": item})
}

fn local_rows(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| row(i as u64, "local", THREAD, item))
        .collect()
}

fn empty_projection() -> Value {
    json!({
        "thread": {
            "id": THREAD, "projectId": "project-reducer", "title": "Reducer",
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
            "worktreePath": null, "activeProviderThreadId": null,
            "lineage": {"rootThreadId": THREAD, "parentThreadId": null, "relationshipToParent": null},
            "forkedFrom": null, "createdBy": "user", "creationSource": "web", "createdAt": NOW,
            "updatedAt": NOW, "archivedAt": null, "settledOverride": null, "settledAt": null,
            "lastVisitedAt": null, "deletedAt": null
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": NOW
    })
}

fn projection(fields: Value) -> ThreadProjection {
    serde_json::from_value(with(empty_projection(), fields)).unwrap()
}

fn event(event_type: &str, payload: Value) -> DomainEvent {
    serde_json::from_value(
        json!({"id": format!("event-{event_type}"), "type": event_type,
        "threadId": THREAD, "runId": RUN, "occurredAt": NOW, "payload": payload}),
    )
    .unwrap()
}

fn apply(projection: &mut ThreadProjection, event: &DomainEvent) -> Outcome {
    apply_event(projection, event, ApplyOptions::default()).unwrap()
}

fn visible_ids(projection: &ThreadProjection) -> Vec<String> {
    projection
        .visible_turn_items
        .iter()
        .map(|row| row.item.id.to_string())
        .collect()
}

fn visible_ids_and_positions(projection: &ThreadProjection) -> Vec<(String, u64)> {
    projection
        .visible_turn_items
        .iter()
        .map(|row| (row.item.id.to_string(), row.position))
        .collect()
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn applies_a_compact_pr_sync_without_replacing_other_links_or_thread_metadata() {
    let link = |number: u64| {
        json!({"host": "github.com", "repository": "pingdotgg/t3code", "number": number,
            "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
            "source": "manual", "linkedAt": NOW, "snapshot": null, "stack": null})
    };
    let mut thread = empty_projection()["thread"].clone();
    thread["pullRequests"] = json!([link(1), link(2)]);
    let mut state = projection(json!({"thread": thread.clone()}));
    let updated = with(link(2), json!({"source": "agent"}));
    apply(
        &mut state,
        &event("thread.pull-request-link-updated", updated.clone()),
    );
    thread["pullRequests"] = json!([link(1), updated]);
    assert_eq!(state.thread, thread);
}

#[test]
fn pr_sync_matches_keys_case_insensitively_and_ignores_threads_without_links() {
    let mut thread = empty_projection()["thread"].clone();
    thread["pullRequests"] = json!([{"host": "GitHub.com", "repository": "A/B", "number": 7}]);
    let mut state = projection(json!({"thread": thread}));
    let updated =
        json!({"host": "github.com", "repository": "a/b", "number": 7, "source": "agent"});
    apply(
        &mut state,
        &event("thread.pull-request-link-updated", updated.clone()),
    );
    assert_eq!(state.thread["pullRequests"], json!([updated]));

    let mut bare = projection(json!({}));
    let before = bare.thread.clone();
    apply(
        &mut bare,
        &event(
            "thread.pull-request-link-updated",
            json!({"host": "h",
        "repository": "r", "number": 1}),
        ),
    );
    assert_eq!(bare.thread, before);
}

#[test]
fn keeps_live_token_usage_when_the_terminal_provider_turn_omits_it() {
    let running = json!({
        "id": "provider-turn-reducer", "providerThreadId": "provider-thread-reducer",
        "nodeId": "provider-node-reducer", "runAttemptId": null, "nativeTurnRef": null,
        "ordinal": 1, "status": "running", "startedAt": NOW, "completedAt": null,
        "tokenUsage": {"usedTokens": 50_000, "maxTokens": 200_000,
            "updatedAt": "2026-08-29T00:00:00.000Z"}
    });
    let mut state = projection(json!({"providerTurns": [running.clone()]}));
    let mut terminal = with(
        running.clone(),
        json!({"status": "completed", "completedAt": NOW}),
    );
    terminal.as_object_mut().unwrap().remove("tokenUsage");
    apply(&mut state, &event("provider-turn.updated", terminal));
    assert_eq!(state.provider_turns[0].extra["status"], json!("completed"));
    assert_eq!(
        state.provider_turns[0].token_usage,
        Some(running["tokenUsage"].clone())
    );
}

#[test]
fn applies_thread_lifecycle_payloads_instead_of_leaving_stale_metadata() {
    let archived_at = "2026-06-20T01:00:00.000Z";
    let mut state = projection(json!({}));
    let payload = with(
        empty_projection()["thread"].clone(),
        json!({"archivedAt": archived_at, "updatedAt": archived_at}),
    );
    let event: DomainEvent = serde_json::from_value(json!({"id": "event-archive",
        "type": "thread.archived", "threadId": THREAD, "occurredAt": archived_at,
        "payload": payload}))
    .unwrap();
    apply(&mut state, &event);
    assert_eq!(state.thread["archivedAt"], json!(archived_at));
    assert_eq!(state.updated_at, archived_at);
}

#[test]
fn quiet_thread_events_replace_the_thread_without_bumping_updated_at() {
    for event_type in [
        "thread.visited",
        "thread.tags-updated",
        "thread.marked-unread",
    ] {
        let mut state = projection(json!({"updatedAt": "earlier"}));
        let payload = with(
            empty_projection()["thread"].clone(),
            json!({"lastVisitedAt": NOW}),
        );
        assert_eq!(
            apply(&mut state, &event(event_type, payload)),
            Outcome::Applied
        );
        assert_eq!(state.thread["lastVisitedAt"], json!(NOW), "{event_type}");
        assert_eq!(state.updated_at, "earlier", "{event_type}");
    }
}

#[test]
fn preserves_visible_rows_when_run_updates_do_not_change_membership() {
    let item = command_item("item-stable", "done", 1);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "turnItems": [item.clone()],
        "visibleTurnItems": local_rows(std::slice::from_ref(&item)),
    }));
    let before = state.visible_turn_items.clone();
    apply(&mut state, &event("run.updated", run(RUN, "completed", 1)));
    assert_eq!(state.visible_turn_items, before);
}

#[test]
fn replaces_only_the_updated_visible_item_and_retains_its_deferred_work_cursor() {
    let deferred = json!({"cursor": "remaining-work", "itemCount": 100, "lastOrdinal": 101});
    let first = with(
        command_item("item-first", "first", 1),
        json!({"deferredWork": deferred}),
    );
    let second = command_item("item-second", "second", 1);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "turnItems": [first.clone(), second.clone()],
        "visibleTurnItems": local_rows(&[first, second]),
    }));
    let second_row = state.visible_turn_items[1].clone();
    let updated = command_item("item-first", "streamed output", 1);
    apply(&mut state, &event("turn-item.updated", updated.clone()));
    assert_eq!(
        to_value(&state.visible_turn_items[0].item),
        with(updated, json!({"deferredWork": deferred}))
    );
    assert_eq!(state.visible_turn_items[0].position, 0);
    assert_eq!(state.visible_turn_items[1], second_row);
}

#[test]
fn inserts_live_turn_items_by_authoritative_ordinal() {
    let queued_future = command_item("item-queued-future", "queued", 300);
    let active_assistant = command_item("item-active-assistant", "done", 201);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "turnItems": [queued_future.clone()],
        "visibleTurnItems": local_rows(std::slice::from_ref(&queued_future)),
    }));
    apply(&mut state, &event("turn-item.updated", active_assistant));
    assert_eq!(
        visible_ids_and_positions(&state),
        vec![
            ("item-active-assistant".into(), 0),
            ("item-queued-future".into(), 1)
        ]
    );
}

#[test]
fn moves_an_updated_item_to_its_new_ordinal_and_renumbers_the_rows_it_passes() {
    let inherited = with(
        command_item("item-inherited", "parent", 0),
        json!({"threadId": "parent"}),
    );
    let items = [
        command_item("item-a", "a", 1),
        command_item("item-b", "b", 2),
        command_item("item-c", "c", 2),
    ];
    let mut rows = vec![row(0, "inherited", "parent", &inherited)];
    rows.extend(
        items
            .iter()
            .enumerate()
            .map(|(i, item)| row(i as u64 + 1, "local", THREAD, item)),
    );
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)], "turnItems": items, "visibleTurnItems": rows,
    }));
    let inherited_row = state.visible_turn_items[0].clone();

    let moved = command_item("item-a", "moved", 2);
    apply(&mut state, &event("turn-item.updated", moved.clone()));
    // Equal ordinals order by id, so item-a stays ahead of item-b.
    assert_eq!(
        visible_ids_and_positions(&state),
        vec![
            ("item-inherited".into(), 0),
            ("item-a".into(), 1),
            ("item-b".into(), 2),
            ("item-c".into(), 3)
        ]
    );
    assert_eq!(state.visible_turn_items[0], inherited_row);
    assert_eq!(to_value(&state.visible_turn_items[1].item), moved);
    let turn_item_ids: Vec<_> = state
        .turn_items
        .iter()
        .map(|item| item.id.to_string())
        .collect();
    assert_eq!(turn_item_ids, vec!["item-a", "item-b", "item-c"]);

    let later = command_item("item-a", "later", 3);
    apply(&mut state, &event("turn-item.updated", later.clone()));
    assert_eq!(
        visible_ids_and_positions(&state),
        vec![
            ("item-inherited".into(), 0),
            ("item-b".into(), 1),
            ("item-c".into(), 2),
            ("item-a".into(), 3)
        ]
    );
    assert_eq!(to_value(&state.turn_items[0]), later);
}

mod partial_timeline {
    use super::*;
    use pretty_assertions::assert_eq;

    const QUEUED: &str = "run-queued";
    const STEER: &str = "run-steer";

    fn run_item(id: &str, run_id: &str, ordinal: u64) -> Value {
        with(command_item(id, id, ordinal), json!({"runId": run_id}))
    }

    fn window_of(runs: Vec<Value>, items: &[Value]) -> ThreadProjection {
        projection(json!({"runs": runs, "turnItems": items, "visibleTurnItems": local_rows(items)}))
    }

    fn partial(latest: u64) -> ApplyOptions {
        ApplyOptions {
            partial_timeline: true,
            latest_local_turn_ordinal: Some(latest),
        }
    }

    #[test]
    fn shows_a_queued_run_that_starts_after_a_newer_steer_raised_the_watermark() {
        let steer_items = [
            run_item("item-steer-user", STEER, 3_000_001),
            run_item("item-steer-reply", STEER, 3_000_002),
        ];
        let mut state = window_of(
            vec![
                run(RUN, "completed", 1),
                run(QUEUED, "queued", 2),
                run(STEER, "completed", 3),
            ],
            &steer_items,
        );
        let mut latest = 3_000_002;
        let queued_user = run_item("item-queued-user", QUEUED, 2_000_001);
        let queued_reply = run_item("item-queued-reply", QUEUED, 2_000_002);
        for event in [
            event("turn-item.updated", queued_user),
            event("run.updated", run(QUEUED, "running", 2)),
            event("turn-item.updated", queued_reply),
            event("run.updated", run(QUEUED, "completed", 2)),
        ] {
            apply_event(&mut state, &event, partial(latest)).unwrap();
            if event.event_type == "turn-item.updated" {
                latest = latest.max(event.payload["ordinal"].as_u64().unwrap());
            }
        }
        assert_eq!(
            visible_ids(&state),
            vec![
                "item-queued-user",
                "item-queued-reply",
                "item-steer-user",
                "item-steer-reply"
            ]
        );
        assert!(
            state
                .turn_items
                .iter()
                .any(|item| item.id.as_str() == "item-queued-reply")
        );
    }

    #[test]
    fn keeps_a_running_runs_item_when_the_window_starts_after_its_earlier_items() {
        let steer_items = [
            run_item("item-steer-user", STEER, 3_000_001),
            run_item("item-steer-reply", STEER, 3_000_002),
        ];
        let mut state = window_of(
            vec![
                run(RUN, "completed", 1),
                run(QUEUED, "running", 2),
                run(STEER, "completed", 3),
            ],
            &steer_items,
        );
        let reply = run_item("item-running-reply", QUEUED, 2_000_003);
        apply_event(
            &mut state,
            &event("turn-item.updated", reply),
            partial(3_000_002),
        )
        .unwrap();
        assert_eq!(
            visible_ids(&state),
            vec!["item-running-reply", "item-steer-user", "item-steer-reply"]
        );
    }

    #[test]
    fn keeps_an_item_of_a_run_already_in_the_window_that_arrives_below_the_watermark() {
        let first = run_item("item-first", STEER, 3_000_001);
        let later = run_item("item-later", STEER, 3_000_003);
        let late = run_item("item-late", STEER, 3_000_002);
        let mut state = window_of(
            vec![run(RUN, "completed", 1), run(STEER, "completed", 3)],
            &[first, later],
        );
        let outcome = apply_event(
            &mut state,
            &event("turn-item.updated", late),
            partial(3_000_003),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(
            visible_ids(&state),
            vec!["item-first", "item-late", "item-later"]
        );
    }

    #[test]
    fn drops_an_item_of_a_finished_run_outside_the_window_that_arrives_below_the_watermark() {
        let first = run_item("item-first", STEER, 3_000_001);
        let later = run_item("item-later", STEER, 3_000_003);
        let late = run_item("item-late", RUN, 3_000_002);
        let mut state = window_of(
            vec![run(RUN, "completed", 1), run(STEER, "completed", 3)],
            &[first, later],
        );
        let before = state.clone();
        let outcome = apply_event(
            &mut state,
            &event("turn-item.updated", late),
            partial(3_000_003),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Ignored);
        assert_eq!(state, before);
    }

    #[test]
    fn drops_run_less_items_below_the_oldest_loaded_row() {
        let first = run_item("item-first", STEER, 100);
        let mut state = window_of(vec![run(STEER, "completed", 3)], &[first]);
        let orphan = with(command_item("orphan", "", 50), json!({"runId": null}));
        let outcome = apply_event(
            &mut state,
            &event("turn-item.updated", orphan),
            ApplyOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: None,
            },
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Ignored);

        let fresh = with(command_item("fresh", "", 150), json!({"runId": null}));
        let outcome = apply_event(
            &mut state,
            &event("turn-item.updated", fresh),
            ApplyOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: None,
            },
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(visible_ids(&state), vec!["item-first", "fresh"]);
    }
}

#[test]
fn removes_only_hidden_local_items_while_preserving_inherited_rows() {
    let inherited = command_item("item-inherited", "done", 1);
    let local = command_item("item-local", "done", 1);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "turnItems": [local.clone()],
        "visibleTurnItems": [row(0, "inherited", "thread-source", &inherited),
                             row(1, "local", THREAD, &local)],
    }));
    let inherited_row = state.visible_turn_items[0].clone();
    apply(
        &mut state,
        &event("run.updated", run(RUN, "rolled_back", 1)),
    );
    assert_eq!(state.visible_turn_items, vec![inherited_row]);
}

#[test]
fn streamed_replacement_of_the_last_row_keeps_every_other_row() {
    let items: Vec<Value> = (0..1000)
        .map(|i| command_item(&format!("item-{i}"), "before", i))
        .collect();
    let runs: Vec<Value> = (0..100)
        .map(|i| run(&format!("run-{i}"), "completed", i + 1))
        .collect();
    let mut state = projection(
        json!({"runs": runs, "turnItems": items, "visibleTurnItems": local_rows(&items)}),
    );
    let first_row = state.visible_turn_items[0].clone();
    let payload = command_item("item-999", "after", 999);
    apply(&mut state, &event("turn-item.updated", payload.clone()));
    assert_eq!(
        to_value(&state.visible_turn_items.last().unwrap().item),
        payload
    );
    assert_eq!(state.visible_turn_items.last().unwrap().position, 999);
    assert_eq!(state.visible_turn_items[0], first_row);
    assert_eq!(state.visible_turn_items.len(), 1000);
}

#[test]
fn rechecks_visibility_when_an_existing_item_changes_runs_without_changing_ordinal() {
    let first = command_item("item-first", "first", 1);
    let second = command_item("item-second", "second", 2);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1), run("rolled-back", "rolled_back", 2)],
        "turnItems": [first.clone(), second.clone()],
        "visibleTurnItems": local_rows(&[first.clone(), second]),
    }));
    apply(
        &mut state,
        &event(
            "turn-item.updated",
            with(first.clone(), json!({"runId": "rolled-back"})),
        ),
    );
    assert_eq!(
        visible_ids_and_positions(&state),
        vec![("item-second".into(), 0)]
    );
    apply(&mut state, &event("turn-item.updated", first));
    assert_eq!(
        visible_ids_and_positions(&state),
        vec![("item-first".into(), 0), ("item-second".into(), 1)]
    );
}

#[test]
fn restores_deduped_bodies_from_the_records_it_already_holds_also_in_a_partial_timeline() {
    let reply = with(
        command_item("item-reply", "", 400),
        json!({"type": "assistant_message", "messageId": "message-deduped",
               "text": "the full reply", "streaming": false}),
    );
    let mut reply = reply;
    for key in ["input", "output", "exitCode"] {
        reply.as_object_mut().unwrap().remove(key);
    }
    let subagent = json!({
        "id": "node-deduped", "threadId": THREAD, "runId": RUN, "parentNodeId": "node-root",
        "origin": "app_owned", "createdBy": "agent", "driver": "codex",
        "providerInstanceId": "codex", "providerThreadId": null, "childThreadId": null,
        "nativeTaskRef": null, "prompt": "the task brief", "title": null, "model": null,
        "status": "running", "result": null, "startedAt": NOW, "completedAt": null,
        "updatedAt": NOW
    });
    let base = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "messages": [{"id": "message-deduped", "threadId": THREAD, "runId": RUN,
            "nodeId": null, "role": "assistant", "text": "the full reply", "attachments": [],
            "streaming": false, "createdBy": "agent", "creationSource": "provider",
            "createdAt": NOW, "updatedAt": NOW}],
        "subagents": [subagent.clone()],
    }));

    for options in [
        ApplyOptions::default(),
        ApplyOptions {
            partial_timeline: true,
            latest_local_turn_ordinal: Some(300),
        },
    ] {
        let mut state = base.clone();
        let deduped = with(reply.clone(), json!({"text": "", "textOmitted": true}));
        apply_event(&mut state, &event("turn-item.updated", deduped), options).unwrap();
        assert_eq!(to_value(&state.turn_items), json!([reply]));
        let visible: Vec<Value> = state
            .visible_turn_items
            .iter()
            .map(|row| to_value(&row.item))
            .collect();
        assert_eq!(visible, vec![reply.clone()]);
    }

    let mut state = base;
    apply(
        &mut state,
        &event(
            "subagent.updated",
            with(
                subagent.clone(),
                json!({"prompt": "", "promptOmitted": true, "progress": "step 2"}),
            ),
        ),
    );
    assert_eq!(
        to_value(&state.subagents),
        json!([with(subagent, json!({"progress": "step 2"}))])
    );
}

// Beyond upstream: every remaining branch of the reducer.

#[test]
fn events_for_other_threads_and_unknown_types_are_ignored() {
    let mut state = projection(json!({}));
    let before = state.clone();
    let mut foreign = event("run.updated", run(RUN, "running", 1));
    foreign.thread_id = "other".into();
    assert_eq!(apply(&mut state, &foreign), Outcome::Ignored);
    assert_eq!(
        apply(&mut state, &event("thread.teleported", json!({}))),
        Outcome::Ignored
    );
    assert_eq!(state, before);
}

#[test]
fn malformed_payloads_are_errors() {
    let mut state = projection(json!({}));
    for event_type in [
        "run.updated",
        "turn-item.updated",
        "run-attempt.created",
        "message.updated",
        "runtime-request.updated",
        "subagent.updated",
        "provider-turn.updated",
        "checkpoint.captured",
    ] {
        let error = apply_event(
            &mut state,
            &event(event_type, json!({"bogus": 1})),
            ApplyOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.event_type, event_type);
    }
}

#[test]
fn upserts_every_entity_collection() {
    let mut state = projection(json!({"updatedAt": "before"}));
    let cases: &[(&str, Value)] = &[
        ("node.updated", json!({"id": "n", "status": "running"})),
        (
            "provider-session.attached",
            json!({"id": "s", "status": "ready"}),
        ),
        ("provider-thread.updated", json!({"id": "pt"})),
        (
            "runtime-request.updated",
            json!({"id": "q", "kind": "command", "status": "pending",
            "responseCapability": {"type": "message"}}),
        ),
        ("message.updated", json!({"id": "m", "text": "hello"})),
        ("plan.updated", json!({"id": "plan"})),
        ("checkpoint-scope.created", json!({"id": "scope"})),
        ("checkpoint.captured", json!({"id": "cp", "files": []})),
        ("context-handoff.updated", json!({"id": "h"})),
        ("context-transfer.created", json!({"id": "x"})),
        (
            "run-attempt.created",
            json!({"id": "a", "runId": RUN, "rootNodeId": "n", "status": "running"}),
        ),
    ];
    for (event_type, payload) in cases {
        assert_eq!(
            apply(&mut state, &event(event_type, payload.clone())),
            Outcome::Applied
        );
        // Twice: the second is an in-place replacement, not a duplicate.
        apply(&mut state, &event(event_type, payload.clone()));
    }
    assert_eq!(state.nodes.len(), 1);
    assert_eq!(state.provider_sessions.len(), 1);
    assert_eq!(state.provider_threads.len(), 1);
    assert_eq!(state.runtime_requests.len(), 1);
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.plans.len(), 1);
    assert_eq!(state.checkpoint_scopes.len(), 1);
    assert_eq!(state.checkpoints.len(), 1);
    assert_eq!(state.context_handoffs.len(), 1);
    assert_eq!(state.context_transfers.len(), 1);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.updated_at, NOW);

    apply(
        &mut state,
        &event("message.updated", json!({"id": "m", "text": "edited"})),
    );
    assert_eq!(state.messages[0].text, "edited");
    apply(
        &mut state,
        &event(
            "context-transfer.updated",
            json!({"id": "x", "state": "done"}),
        ),
    );
    assert_eq!(state.context_transfers[0].extra["state"], json!("done"));
}

#[test]
fn provider_session_detach_removes_only_that_session() {
    let mut state = projection(json!({"providerSessions": [{"id": "a"}, {"id": "b"}]}));
    apply(
        &mut state,
        &event(
            "provider-session.detached",
            json!({"providerSessionId": "a"}),
        ),
    );
    assert_eq!(state.provider_sessions.len(), 1);
    assert_eq!(state.provider_sessions[0].id, json!("b"));
    apply(&mut state, &event("provider-session.detached", json!({})));
    assert_eq!(state.provider_sessions.len(), 1);
}

#[test]
fn background_work_cancellation_marks_the_run() {
    let mut state =
        projection(json!({"runs": [run(RUN, "running", 1), run("other", "running", 2)]}));
    apply(
        &mut state,
        &event(
            "run.background-work-cancelled",
            json!({"runId": RUN, "restartCancelledBackgroundWork": true}),
        ),
    );
    assert_eq!(
        state.runs[0].restart_cancelled_background_work,
        Some(json!(true))
    );
    assert_eq!(state.runs[1].restart_cancelled_background_work, None);
}

#[test]
fn rollback_request_only_bumps_updated_at() {
    let mut state = projection(json!({"updatedAt": "before"}));
    let before = state.clone();
    apply(
        &mut state,
        &event(
            "checkpoint.rollback-requested",
            json!({"checkpointId": "c"}),
        ),
    );
    assert_eq!(state.updated_at, NOW);
    state.updated_at = before.updated_at.clone();
    assert_eq!(state, before);
}

#[test]
fn hidden_items_are_recorded_but_not_shown() {
    let mut state = projection(json!({"runs": [run(RUN, "rolled_back", 1)]}));
    apply(
        &mut state,
        &event("turn-item.updated", command_item("hidden", "x", 1)),
    );
    assert_eq!(state.turn_items.len(), 1);
    assert!(state.visible_turn_items.is_empty());
}

#[test]
fn interrupt_requests_recheck_superseded_results() {
    let result = with(
        command_item("result", "", 5),
        json!({"type": "run_interrupt_result", "message": "stopped", "nodeId": "root"}),
    );
    let mut state = projection(json!({
        "runs": [run(RUN, "running", 1)],
        "attempts": [{"id": "att", "runId": RUN, "rootNodeId": "root", "status": "superseded"}],
        "turnItems": [result.clone()],
        "visibleTurnItems": local_rows(std::slice::from_ref(&result)),
    }));
    // A plain run update re-derives visibility: the superseded result has no request, so it hides.
    apply(&mut state, &event("run.updated", run(RUN, "running", 1)));
    assert!(state.visible_turn_items.is_empty());
    // An interrupt request pairs with it, and the request itself is shown.
    let request = with(
        command_item("request", "", 4),
        json!({"type": "run_interrupt_request", "message": "stop please"}),
    );
    apply(&mut state, &event("turn-item.updated", request));
    assert_eq!(visible_ids(&state), vec!["request"]);
    apply(&mut state, &event("turn-item.updated", result));
    assert_eq!(visible_ids(&state), vec!["request", "result"]);
}

#[test]
fn identical_item_update_is_a_no_op_for_rows() {
    let item = command_item("same", "x", 1);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "turnItems": [item.clone()],
        "visibleTurnItems": local_rows(std::slice::from_ref(&item)),
    }));
    let rows = state.visible_turn_items.clone();
    apply(&mut state, &event("turn-item.updated", item));
    assert_eq!(state.visible_turn_items, rows);
}

#[test]
fn an_inherited_row_becomes_local_when_its_item_streams_here() {
    let item = command_item("shared", "x", 1);
    let mut state = projection(json!({
        "runs": [run(RUN, "completed", 1)],
        "visibleTurnItems": [row(0, "inherited", "parent", &item)],
    }));
    apply(
        &mut state,
        &event("turn-item.updated", command_item("shared", "y", 1)),
    );
    assert_eq!(state.visible_turn_items.len(), 1);
    assert_eq!(
        state.visible_turn_items[0].visibility,
        t3_contracts::Visibility::Local
    );
    assert_eq!(
        state.visible_turn_items[0].source_thread_id.as_str(),
        THREAD
    );
}
