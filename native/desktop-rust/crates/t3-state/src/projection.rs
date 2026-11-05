//! Port of `packages/client-runtime/src/state/orchestrationV2Projection.ts`: applies one thread
//! stream event to the thread projection the transcript renders.

use std::cmp::Ordering;

use serde::de::DeserializeOwned;
use serde_json::Value;
use t3_contracts::{
    Checkpoint, ConversationMessage, DomainEvent, Entity, ProjectedTurnItem, ProviderTurn, Run,
    RunAttempt, RunStatus, RuntimeRequest, Subagent, ThreadProjection, TurnItem, TurnItemId,
    Visibility,
};

use crate::bodies::{restore_subagent_bodies, restore_turn_item_bodies};
use crate::collation::locale_compare;
use crate::visibility::{TurnItemVisibility, is_turn_item_visible};

/// `ApplyOrchestrationV2ProjectionEventOptions`. Desktop subscriptions request full snapshots,
/// so `partial_timeline` stays false outside tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyOptions {
    pub partial_timeline: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// The event belongs to another thread, is of a type this client does not know, or is an
    /// item outside a partial window. The projection is untouched.
    Ignored,
}

#[derive(Debug, thiserror::Error)]
#[error("`{event_type}` payload does not decode: {source}")]
pub struct ApplyError {
    pub event_type: String,
    #[source]
    pub source: serde_json::Error,
}

const THREAD_REPLACING_EVENTS: &[&str] = &[
    "thread.created",
    "thread.archived",
    "thread.unarchived",
    "thread.deleted",
    "thread.settled",
    "thread.unsettled",
    "thread.snoozed",
    "thread.unsnoozed",
    "thread.auto-settle-set",
    "thread.pinned",
    "thread.unpinned",
    "thread.pin-reordered",
    "thread.active-reordered",
    "thread.metadata-updated",
    "thread.pull-request-synced",
    "thread.runtime-mode-updated",
    "thread.interaction-mode-updated",
    "thread.model-selection-updated",
    "thread.provider-switched",
    "thread.guest-share-updated",
];

/// Visits and tags are not activity, so they skip the `updatedAt` bump.
const THREAD_REPLACING_QUIET_EVENTS: &[&str] = &[
    "thread.visited",
    "thread.tags-updated",
    "thread.marked-unread",
];

fn decode<T: DeserializeOwned>(event: &DomainEvent) -> Result<T, ApplyError> {
    serde_json::from_value(event.payload.clone()).map_err(|source| ApplyError {
        event_type: event.event_type.clone(),
        source,
    })
}

trait HasId {
    type Id: PartialEq;
    fn entity_id(&self) -> &Self::Id;
}

macro_rules! has_id {
    ($($ty:ty => $id:ty),* $(,)?) => {$(
        impl HasId for $ty {
            type Id = $id;
            fn entity_id(&self) -> &$id {
                &self.id
            }
        }
    )*};
}

has_id!(
    Entity => Value,
    Run => t3_contracts::RunId,
    RunAttempt => t3_contracts::RunAttemptId,
    Subagent => t3_contracts::NodeId,
    ProviderTurn => Value,
    RuntimeRequest => t3_contracts::RuntimeRequestId,
    ConversationMessage => t3_contracts::MessageId,
    Checkpoint => Value,
);

/// `upsertEntity`: replaces the first record with the same id, else appends.
fn upsert<T: HasId>(items: &mut Vec<T>, item: T) {
    match items
        .iter()
        .position(|candidate| candidate.entity_id() == item.entity_id())
    {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

fn renumber(rows: &mut [ProjectedTurnItem]) {
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position as u64;
    }
}

/// `removeVisibleItem`.
fn remove_visible_item(rows: &mut Vec<ProjectedTurnItem>, source_item_id: &TurnItemId) {
    if let Some(index) = rows
        .iter()
        .position(|row| &row.source_item_id == source_item_id)
    {
        rows.remove(index);
        renumber(rows);
    }
}

/// `activeVisibleTurnItems`: drops local rows that runs or attempts now hide.
fn refresh_visible_turn_items(projection: &mut ThreadProjection) {
    let visibility = TurnItemVisibility::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    if visibility.is_trivial() {
        return;
    }
    let before = projection.visible_turn_items.len();
    projection
        .visible_turn_items
        .retain(|row| row.visibility != Visibility::Local || visibility.is_visible(&row.item));
    if projection.visible_turn_items.len() != before {
        renumber(&mut projection.visible_turn_items);
    }
}

fn oldest_local_turn_ordinal(rows: &[ProjectedTurnItem]) -> Option<u64> {
    rows.iter()
        .filter(|row| row.visibility == Visibility::Local)
        .map(|row| row.item.ordinal)
        .min()
}

fn is_below_partial_window(
    projection: &ThreadProjection,
    item: &TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if latest_local_turn_ordinal.is_some_and(|latest| item.ordinal <= latest) {
        return true;
    }
    oldest_local_turn_ordinal(&projection.visible_turn_items)
        .is_some_and(|oldest| item.ordinal < oldest)
}

/// `shouldDropMissingPartialTurnItem`.
fn should_drop_missing_partial_turn_item(
    projection: &ThreadProjection,
    item: &TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if projection
        .visible_turn_items
        .iter()
        .any(|row| row.source_item_id == item.id)
    {
        return false;
    }
    let Some(run_id) = &item.run_id else {
        return is_below_partial_window(projection, item, latest_local_turn_ordinal);
    };
    // Items of a run that has not finished are new, not unloaded history.
    if projection
        .runs
        .iter()
        .any(|run| &run.id == run_id && run.run_status().is_some_and(RunStatus::is_live))
    {
        return false;
    }
    if projection.visible_turn_items.iter().any(|row| {
        row.visibility == Visibility::Local
            && row.item.run_id.as_ref() == Some(run_id)
            && row.item.ordinal <= item.ordinal
    }) {
        return false;
    }
    is_below_partial_window(projection, item, latest_local_turn_ordinal)
}

/// `upsertVisibleTurnItem`: replaces a row in place when its ordinal is unchanged, otherwise
/// moves it before the first later local row.
fn upsert_visible_turn_item(
    projection: &mut ThreadProjection,
    item: &TurnItem,
    options: ApplyOptions,
) {
    let index = projection
        .visible_turn_items
        .iter()
        .rposition(|row| row.source_item_id == item.id);
    let next = ProjectedTurnItem {
        position: 0,
        visibility: Visibility::Local,
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        item: item.clone(),
    };
    if let Some(index) = index {
        let previous = &projection.visible_turn_items[index];
        if previous.visibility == Visibility::Local
            && previous.source_thread_id == next.source_thread_id
        {
            if &previous.item == item {
                return;
            }
            if previous.item.ordinal == item.ordinal {
                let position = previous.position;
                projection.visible_turn_items[index] = ProjectedTurnItem { position, ..next };
                return;
            }
        }
    } else if options.partial_timeline
        && should_drop_missing_partial_turn_item(
            projection,
            item,
            options.latest_local_turn_ordinal,
        )
    {
        return;
    }

    let rows = &projection.visible_turn_items;
    let insertion = rows
        .iter()
        .enumerate()
        .position(|(candidate, row)| {
            Some(candidate) != index
                && row.visibility == Visibility::Local
                && match row.item.ordinal.cmp(&item.ordinal) {
                    Ordering::Greater => true,
                    Ordering::Equal => {
                        locale_compare(row.item.id.as_str(), item.id.as_str()) == Ordering::Greater
                    }
                    Ordering::Less => false,
                }
        })
        .unwrap_or(rows.len());

    let mut next = Some(next);
    let mut updated = Vec::with_capacity(rows.len() + 1);
    for (source, row) in projection.visible_turn_items.drain(..).enumerate() {
        if source == insertion {
            updated.extend(next.take());
        }
        if Some(source) != index {
            updated.push(row);
        }
    }
    updated.extend(next.take());
    renumber(&mut updated);
    projection.visible_turn_items = updated;
}

fn apply_turn_item(
    projection: &mut ThreadProjection,
    event: &DomainEvent,
    options: ApplyOptions,
) -> Result<Outcome, ApplyError> {
    let mut payload: TurnItem = decode(event)?;
    restore_turn_item_bodies(&mut payload, projection);
    // Streamed updates land on the newest items, so search from the end.
    let item_index = projection
        .turn_items
        .iter()
        .rposition(|item| item.id == payload.id);
    if let Some(deferred) =
        item_index.and_then(|index| projection.turn_items[index].extra.get("deferredWork"))
    {
        payload
            .extra
            .insert("deferredWork".to_owned(), deferred.clone());
    }
    if options.partial_timeline
        && item_index.is_none()
        && should_drop_missing_partial_turn_item(
            projection,
            &payload,
            options.latest_local_turn_ordinal,
        )
    {
        return Ok(Outcome::Ignored);
    }

    let previous_type = match item_index {
        Some(index) => {
            let previous = std::mem::replace(&mut projection.turn_items[index], payload.clone());
            Some(previous.item_type)
        }
        None => {
            projection.turn_items.push(payload.clone());
            None
        }
    };
    // Only interrupt requests can change another item's visibility. Streaming text and tool
    // updates must not recheck every row against every run.
    if payload.item_type == "run_interrupt_request"
        || previous_type.as_deref() == Some("run_interrupt_request")
    {
        refresh_visible_turn_items(projection);
    }
    if is_turn_item_visible(
        &payload,
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    ) {
        upsert_visible_turn_item(projection, &payload, options);
    } else {
        remove_visible_item(&mut projection.visible_turn_items, &payload.id);
    }
    projection.updated_at.clone_from(&event.occurred_at);
    Ok(Outcome::Applied)
}

fn pull_request_key(link: &Value) -> Option<String> {
    let host = link.get("host")?.as_str()?.trim().to_lowercase();
    let repository = link.get("repository")?.as_str()?.trim().to_lowercase();
    let number = link.get("number")?.as_u64()?;
    Some(format!("{host}/{repository}#{number}"))
}

/// `applyThreadPullRequestLinkUpdate`, keyed by host, repository and number.
///
/// Upstream also canonicalizes repository keys through the forge registry. The desktop client
/// does not render pull request links from the projection, so the plain key is enough here.
fn apply_pull_request_link_update(thread: &mut Value, updated: &Value) {
    let Some(key) = pull_request_key(updated) else {
        return;
    };
    let Some(links) = thread.get_mut("pullRequests").and_then(Value::as_array_mut) else {
        return;
    };
    for link in links.iter_mut() {
        if pull_request_key(link).as_deref() == Some(key.as_str()) {
            *link = updated.clone();
        }
    }
}

/// `applyOrchestrationV2ProjectionEvent`, mutating `projection` in place.
pub fn apply_event(
    projection: &mut ThreadProjection,
    event: &DomainEvent,
    options: ApplyOptions,
) -> Result<Outcome, ApplyError> {
    if projection.thread_id().as_ref() != Some(&event.thread_id) {
        return Ok(Outcome::Ignored);
    }
    let event_type = event.event_type.as_str();
    match event_type {
        "turn-item.updated" => return apply_turn_item(projection, event, options),
        "thread.pull-request-link-updated" => {
            apply_pull_request_link_update(&mut projection.thread, &event.payload);
        }
        _ if THREAD_REPLACING_EVENTS.contains(&event_type) => {
            projection.thread = event.payload.clone();
        }
        _ if THREAD_REPLACING_QUIET_EVENTS.contains(&event_type) => {
            projection.thread = event.payload.clone();
            return Ok(Outcome::Applied);
        }
        "run.created" | "run.updated" => {
            upsert(&mut projection.runs, decode::<Run>(event)?);
            refresh_visible_turn_items(projection);
        }
        "run.background-work-cancelled" => {
            let run_id = event.payload.get("runId").and_then(Value::as_str);
            let value = event.payload.get("restartCancelledBackgroundWork").cloned();
            for run in &mut projection.runs {
                if Some(run.id.as_str()) == run_id {
                    run.restart_cancelled_background_work.clone_from(&value);
                }
            }
        }
        "run-attempt.created" | "run-attempt.updated" => {
            upsert(&mut projection.attempts, decode::<RunAttempt>(event)?);
            refresh_visible_turn_items(projection);
        }
        "node.updated" => upsert(&mut projection.nodes, decode::<Entity>(event)?),
        "subagent.updated" => {
            let mut payload: Subagent = decode(event)?;
            let previous = projection
                .subagents
                .iter()
                .find(|subagent| subagent.id == payload.id);
            restore_subagent_bodies(&mut payload.extra, previous.map(|subagent| &subagent.extra));
            upsert(&mut projection.subagents, payload);
        }
        "provider-session.attached" | "provider-session.updated" => {
            upsert(&mut projection.provider_sessions, decode::<Entity>(event)?);
        }
        "provider-session.detached" => {
            let detached = event.payload.get("providerSessionId");
            projection
                .provider_sessions
                .retain(|session| Some(&session.id) != detached);
        }
        "provider-thread.updated" => {
            upsert(&mut projection.provider_threads, decode::<Entity>(event)?);
        }
        "provider-turn.updated" => {
            let mut payload: ProviderTurn = decode(event)?;
            if payload.token_usage.is_none() {
                payload.token_usage = projection
                    .provider_turns
                    .iter()
                    .find(|turn| turn.id == payload.id)
                    .and_then(|turn| turn.token_usage.clone());
            }
            upsert(&mut projection.provider_turns, payload);
        }
        "runtime-request.updated" => {
            upsert(
                &mut projection.runtime_requests,
                decode::<RuntimeRequest>(event)?,
            );
        }
        "message.updated" => {
            upsert(
                &mut projection.messages,
                decode::<ConversationMessage>(event)?,
            );
        }
        "plan.updated" => upsert(&mut projection.plans, decode::<Entity>(event)?),
        "checkpoint-scope.created" => {
            upsert(&mut projection.checkpoint_scopes, decode::<Entity>(event)?);
        }
        "checkpoint.captured" => {
            upsert(&mut projection.checkpoints, decode::<Checkpoint>(event)?);
        }
        "checkpoint.rollback-requested" => {}
        "context-handoff.updated" => {
            upsert(&mut projection.context_handoffs, decode::<Entity>(event)?);
        }
        "context-transfer.created" | "context-transfer.updated" => {
            upsert(&mut projection.context_transfers, decode::<Entity>(event)?);
        }
        _ => return Ok(Outcome::Ignored),
    }
    projection.updated_at.clone_from(&event.occurred_at);
    Ok(Outcome::Applied)
}
