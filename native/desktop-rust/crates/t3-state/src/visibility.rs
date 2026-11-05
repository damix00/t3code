//! Port of `@t3tools/shared/orchestrationV2Timeline`: which turn items belong in a transcript.

use std::collections::{HashMap, HashSet};

use t3_contracts::{NodeId, Run, RunAttempt, RunId, TurnItem, UserMessageInputIntent};

fn is_queued_user_message(item: &TurnItem) -> bool {
    item.item_type == "user_message"
        && item.input_intent() == Some(UserMessageInputIntent::QueuedTurn)
}

/// `isOrchestrationV2SupersededInterrupt`.
pub fn is_superseded_interrupt(
    item: &TurnItem,
    attempts: &[RunAttempt],
    items: &[TurnItem],
) -> bool {
    let (Some(run_id), Some(node_id)) = (&item.run_id, &item.node_id) else {
        return false;
    };
    if item.item_type != "run_interrupt_result" {
        return false;
    }
    let superseded = attempts.iter().any(|attempt| {
        &attempt.run_id == run_id
            && &attempt.root_node_id == node_id
            && attempt.status == "superseded"
    });
    if !superseded {
        return false;
    }
    // Paired stop-then-steer results stay; legacy plain-steer results without requests hide.
    !items.iter().any(|candidate| {
        candidate.item_type == "run_interrupt_request" && candidate.run_id.as_ref() == Some(run_id)
    })
}

/// `isOrchestrationV2TurnItemVisible`.
pub fn is_turn_item_visible(
    item: &TurnItem,
    runs: &[Run],
    attempts: &[RunAttempt],
    items: &[TurnItem],
) -> bool {
    if let Some(run_id) = &item.run_id {
        if runs
            .iter()
            .any(|run| &run.id == run_id && run.status == "rolled_back")
        {
            return false;
        }
        // Cancelled queued input never reached the provider.
        if is_queued_user_message(item)
            && runs
                .iter()
                .any(|run| &run.id == run_id && run.status == "cancelled")
        {
            return false;
        }
    }
    !is_superseded_interrupt(item, attempts, items)
}

/// `createOrchestrationV2TurnItemVisibility`: the same rule, indexed once for many items.
#[derive(Debug)]
pub struct TurnItemVisibility {
    statuses: HashMap<RunId, String>,
    superseded_roots: HashMap<RunId, HashSet<NodeId>>,
    interrupt_runs: HashSet<Option<RunId>>,
}

impl TurnItemVisibility {
    pub fn new(runs: &[Run], attempts: &[RunAttempt], items: &[TurnItem]) -> Self {
        let mut statuses = HashMap::new();
        for run in runs {
            if run.status == "rolled_back" || run.status == "cancelled" {
                statuses.insert(run.id.clone(), run.status.clone());
            } else {
                statuses.remove(&run.id);
            }
        }
        let mut superseded_roots: HashMap<RunId, HashSet<NodeId>> = HashMap::new();
        for attempt in attempts
            .iter()
            .filter(|attempt| attempt.status == "superseded")
        {
            superseded_roots
                .entry(attempt.run_id.clone())
                .or_default()
                .insert(attempt.root_node_id.clone());
        }
        let mut interrupt_runs = HashSet::new();
        if !superseded_roots.is_empty() {
            for item in items
                .iter()
                .filter(|item| item.item_type == "run_interrupt_request")
            {
                interrupt_runs.insert(item.run_id.clone());
            }
        }
        Self {
            statuses,
            superseded_roots,
            interrupt_runs,
        }
    }

    pub fn is_trivial(&self) -> bool {
        self.statuses.is_empty() && self.superseded_roots.is_empty()
    }

    pub fn is_visible(&self, item: &TurnItem) -> bool {
        if self.is_trivial() {
            return true;
        }
        let status = item
            .run_id
            .as_ref()
            .and_then(|run_id| self.statuses.get(run_id));
        match status.map(String::as_str) {
            Some("rolled_back") => return false,
            Some("cancelled") if is_queued_user_message(item) => return false,
            _ => {}
        }
        let superseded = item.item_type == "run_interrupt_result"
            && match (&item.run_id, &item.node_id) {
                (Some(run_id), Some(node_id)) => {
                    self.superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                        && !self.interrupt_runs.contains(&Some(run_id.clone()))
                }
                _ => false,
            };
        !superseded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, item_type: &str, run: Option<&str>, node: Option<&str>) -> TurnItem {
        serde_json::from_value(json!({
            "id": id, "type": item_type, "threadId": "t", "runId": run, "nodeId": node,
            "ordinal": 1, "status": "completed"
        }))
        .unwrap()
    }

    fn queued_user(id: &str, run: &str) -> TurnItem {
        let mut value = item(id, "user_message", Some(run), None);
        value
            .extra
            .insert("inputIntent".into(), json!("queued_turn"));
        value
    }

    fn run(id: &str, status: &str) -> Run {
        serde_json::from_value(json!({"id": id, "status": status})).unwrap()
    }

    fn attempt(run: &str, root: &str, status: &str) -> RunAttempt {
        serde_json::from_value(json!({"id": format!("a-{run}-{root}"), "runId": run,
            "rootNodeId": root, "status": status}))
        .unwrap()
    }

    /// Both implementations must agree on every case.
    fn check(item: &TurnItem, runs: &[Run], attempts: &[RunAttempt], items: &[TurnItem]) -> bool {
        let single = is_turn_item_visible(item, runs, attempts, items);
        let indexed = TurnItemVisibility::new(runs, attempts, items).is_visible(item);
        assert_eq!(single, indexed, "implementations disagree for {}", item.id);
        single
    }

    #[test]
    fn rolled_back_runs_hide_their_items() {
        let runs = [run("r1", "rolled_back"), run("r2", "completed")];
        assert!(!check(
            &item("a", "assistant_message", Some("r1"), None),
            &runs,
            &[],
            &[]
        ));
        assert!(check(
            &item("b", "assistant_message", Some("r2"), None),
            &runs,
            &[],
            &[]
        ));
        assert!(check(
            &item("c", "notification", None, None),
            &runs,
            &[],
            &[]
        ));
    }

    #[test]
    fn cancelled_runs_hide_only_queued_user_input() {
        let runs = [run("r1", "cancelled")];
        assert!(!check(&queued_user("q", "r1"), &runs, &[], &[]));
        assert!(check(
            &item("u", "user_message", Some("r1"), None),
            &runs,
            &[],
            &[]
        ));
        assert!(check(
            &item("a", "assistant_message", Some("r1"), None),
            &runs,
            &[],
            &[]
        ));
    }

    #[test]
    fn superseded_interrupt_results_hide_without_a_request() {
        let attempts = [attempt("r1", "n1", "superseded")];
        let result = item("x", "run_interrupt_result", Some("r1"), Some("n1"));
        assert!(!check(&result, &[], &attempts, &[]));

        let request = item("y", "run_interrupt_request", Some("r1"), None);
        assert!(check(
            &result,
            &[],
            &attempts,
            std::slice::from_ref(&request)
        ));

        // Another node, another run, or a non-superseded attempt keep it.
        assert!(check(
            &item("x", "run_interrupt_result", Some("r1"), Some("n2")),
            &[],
            &attempts,
            &[]
        ));
        assert!(check(
            &item("x", "run_interrupt_result", Some("r2"), Some("n1")),
            &[],
            &attempts,
            &[]
        ));
        assert!(check(
            &result,
            &[],
            &[attempt("r1", "n1", "completed")],
            &[]
        ));
        assert!(check(
            &item("x", "run_interrupt_result", Some("r1"), None),
            &[],
            &attempts,
            &[]
        ));
    }

    #[test]
    fn later_runs_with_the_same_id_override_earlier_status_in_the_index() {
        let visibility =
            TurnItemVisibility::new(&[run("r", "rolled_back"), run("r", "running")], &[], &[]);
        assert!(visibility.is_trivial());
        assert!(visibility.is_visible(&item("a", "assistant_message", Some("r"), None)));
    }
}
