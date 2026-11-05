//! One open thread: its projection, kept current from the thread stream.

use t3_contracts::{ThreadProjection, ThreadStreamItem};

use crate::projection::{ApplyError, ApplyOptions, Outcome, apply_event};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadStatus {
    #[default]
    Loading,
    Live,
    /// The stream sent something this client could not apply; it must resubscribe without a
    /// cursor to get a fresh snapshot.
    NeedsSnapshot,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    projection: Option<ThreadProjection>,
    sequence: Option<u64>,
    status: ThreadStatus,
    last_error: Option<String>,
}

impl ThreadState {
    pub fn projection(&self) -> Option<&ThreadProjection> {
        self.projection.as_ref()
    }

    pub fn status(&self) -> ThreadStatus {
        self.status
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The cursor a resubscription passes as `afterSequence`, unless a fresh snapshot is needed.
    pub fn resume_sequence(&self) -> Option<u64> {
        match self.status {
            ThreadStatus::NeedsSnapshot => None,
            _ => self.sequence,
        }
    }

    /// Applies one stream item. Returns whether anything visible changed.
    pub fn apply(&mut self, item: &ThreadStreamItem) -> bool {
        match item {
            ThreadStreamItem::Snapshot {
                snapshot_sequence,
                projection,
                ..
            } => {
                self.projection = Some((**projection).clone());
                self.sequence = Some(*snapshot_sequence);
                self.status = ThreadStatus::Live;
                self.last_error = None;
                true
            }
            ThreadStreamItem::Event { sequence, event } => {
                let Some(projection) = self.projection.as_mut() else {
                    return false;
                };
                if self.sequence.is_some_and(|current| *sequence <= current) {
                    return false;
                }
                match apply_event(projection, event, ApplyOptions::default()) {
                    Ok(outcome) => {
                        self.sequence = Some(*sequence);
                        outcome == Outcome::Applied
                    }
                    Err(ApplyError { event_type, source }) => {
                        self.status = ThreadStatus::NeedsSnapshot;
                        self.last_error = Some(format!("could not apply {event_type}: {source}"));
                        true
                    }
                }
            }
            ThreadStreamItem::Synchronized => {
                let changed = self.status == ThreadStatus::Loading && self.projection.is_some();
                if self.projection.is_some() && self.status == ThreadStatus::Loading {
                    self.status = ThreadStatus::Live;
                }
                changed
            }
            ThreadStreamItem::Unknown => false,
        }
    }

    pub fn begin_resync(&mut self) {
        if self.status != ThreadStatus::NeedsSnapshot {
            self.status = ThreadStatus::Loading;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items(value: serde_json::Value) -> Vec<ThreadStreamItem> {
        serde_json::from_value(value).unwrap()
    }

    fn projection(thread: &str) -> serde_json::Value {
        json!({
            "thread": {"id": thread}, "runs": [], "attempts": [], "nodes": [], "subagents": [],
            "providerSessions": [], "providerThreads": [], "providerTurns": [],
            "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
            "contextTransfers": [], "visibleTurnItems": [], "updatedAt": "0"
        })
    }

    fn run_event(sequence: u64, run: &str, status: &str) -> serde_json::Value {
        json!({"kind": "event", "sequence": sequence, "event": {
            "id": format!("e{sequence}"), "type": "run.updated", "threadId": "t",
            "occurredAt": format!("t{sequence}"), "payload": {"id": run, "status": status}}})
    }

    #[test]
    fn events_before_a_snapshot_are_ignored() {
        let mut state = ThreadState::default();
        for item in items(json!([run_event(1, "r", "running")])) {
            assert!(!state.apply(&item));
        }
        assert!(state.projection().is_none());
        assert_eq!(state.status(), ThreadStatus::Loading);
    }

    #[test]
    fn snapshot_then_events_then_stale_events() {
        let mut state = ThreadState::default();
        let stream = items(json!([
            {"kind": "snapshot", "snapshotSequence": 10, "projection": projection("t")},
            run_event(11, "r", "running"),
            run_event(11, "r", "completed"),
            run_event(9, "r", "failed"),
            run_event(12, "r", "completed"),
        ]));
        let changed: Vec<bool> = stream.iter().map(|item| state.apply(item)).collect();
        assert_eq!(changed, vec![true, true, false, false, true]);
        let projection = state.projection().unwrap();
        assert_eq!(projection.runs.len(), 1);
        assert_eq!(projection.runs[0].status, "completed");
        assert_eq!(projection.updated_at, "t12");
        assert_eq!(state.resume_sequence(), Some(12));
        assert_eq!(state.status(), ThreadStatus::Live);
    }

    #[test]
    fn unknown_events_advance_the_cursor_without_changes() {
        let mut state = ThreadState::default();
        for item in items(json!([
            {"kind": "snapshot", "snapshotSequence": 1, "projection": projection("t")},
            {"kind": "event", "sequence": 2, "event": {"id": "e", "type": "thread.future",
                "threadId": "t", "occurredAt": "x"}}
        ])) {
            state.apply(&item);
        }
        assert_eq!(state.resume_sequence(), Some(2));
        assert_eq!(state.projection().unwrap().updated_at, "0");
    }

    #[test]
    fn undecodable_payload_requests_a_fresh_snapshot() {
        let mut state = ThreadState::default();
        for item in items(json!([
            {"kind": "snapshot", "snapshotSequence": 1, "projection": projection("t")},
            {"kind": "event", "sequence": 2, "event": {"id": "e", "type": "run.updated",
                "threadId": "t", "occurredAt": "x", "payload": {"nope": true}}}
        ])) {
            state.apply(&item);
        }
        assert_eq!(state.status(), ThreadStatus::NeedsSnapshot);
        assert_eq!(state.resume_sequence(), None);
        assert!(state.last_error().unwrap().contains("run.updated"));
        state.begin_resync();
        assert_eq!(state.status(), ThreadStatus::NeedsSnapshot);
        // The fresh snapshot recovers.
        for item in items(
            json!([{"kind": "snapshot", "snapshotSequence": 5, "projection": projection("t")}]),
        ) {
            state.apply(&item);
        }
        assert_eq!(state.status(), ThreadStatus::Live);
        assert_eq!(state.last_error(), None);
    }

    #[test]
    fn synchronized_marks_live_once() {
        let mut state = ThreadState::default();
        assert!(!state.apply(&ThreadStreamItem::Synchronized));
        for item in items(
            json!([{"kind": "snapshot", "snapshotSequence": 1, "projection": projection("t")}]),
        ) {
            state.apply(&item);
        }
        state.begin_resync();
        assert_eq!(state.status(), ThreadStatus::Loading);
        assert!(state.apply(&ThreadStreamItem::Synchronized));
        assert!(!state.apply(&ThreadStreamItem::Synchronized));
        assert!(!state.apply(&ThreadStreamItem::Unknown));
    }
}
