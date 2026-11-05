//! What the stop button dispatches, following `interruptThreadTurn` in client-runtime.

use t3_contracts::{Command, CommandId, RunStatus, ThreadId, ThreadProjection};

/// Servers advertising `threadStop` stop the whole thread, including runs a paginated
/// projection may not show. Older servers interrupt the newest interruptible run.
pub fn stop_command(
    thread_id: &ThreadId,
    supports_thread_stop: bool,
    projection: Option<&ThreadProjection>,
) -> Option<Command> {
    if supports_thread_stop {
        return Some(Command::ThreadStop {
            command_id: CommandId::random(),
            thread_id: thread_id.clone(),
        });
    }
    let run = projection?
        .runs
        .iter()
        .rev()
        .find(|run| run.run_status().is_some_and(RunStatus::is_interruptible))?;
    Some(Command::RunInterrupt {
        command_id: CommandId::random(),
        thread_id: thread_id.clone(),
        run_id: run.id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn projection(runs: serde_json::Value) -> ThreadProjection {
        serde_json::from_value(json!({
            "thread": {"id": "t"}, "runs": runs, "attempts": [], "nodes": [], "subagents": [],
            "providerSessions": [], "providerThreads": [], "providerTurns": [],
            "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
            "contextTransfers": [], "visibleTurnItems": [], "updatedAt": "x"
        }))
        .unwrap()
    }

    #[test]
    fn thread_stop_when_supported() {
        let command = stop_command(&"t".into(), true, None).unwrap();
        assert!(
            matches!(command, Command::ThreadStop { ref thread_id, .. } if thread_id.as_str() == "t")
        );
    }

    #[test]
    fn interrupts_the_newest_interruptible_run() {
        let projection = projection(json!([
            {"id": "r1", "status": "running"},
            {"id": "r2", "status": "waiting"},
            {"id": "r3", "status": "queued"},
            {"id": "r4", "status": "completed"}
        ]));
        let command = stop_command(&"t".into(), false, Some(&projection)).unwrap();
        assert!(
            matches!(command, Command::RunInterrupt { ref run_id, .. } if run_id.as_str() == "r2")
        );
    }

    #[test]
    fn nothing_to_stop() {
        let projection = projection(json!([{"id": "r1", "status": "completed"},
            {"id": "r2", "status": "queued"}]));
        assert!(stop_command(&"t".into(), false, Some(&projection)).is_none());
        assert!(stop_command(&"t".into(), false, None).is_none());
    }
}
