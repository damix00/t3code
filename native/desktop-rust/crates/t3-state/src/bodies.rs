//! Port of `@t3tools/shared/orchestrationV2DedupedBodies` (the restore half).
//!
//! A server may blank a turn item's body that its record already carries and mark the field.
//! The client copies the value back from the record it holds.

use serde_json::Value;
use t3_contracts::{Extra, ThreadProjection, TurnItem};

fn flag(map: &Extra, key: &str) -> bool {
    map.get(key).and_then(Value::as_bool) == Some(true)
}

/// `restoreSubagentBodies`: fills `prompt`/`result` marked omitted from `previous`, unless the
/// previous record is itself still marked.
pub fn restore_subagent_bodies(value: &mut Extra, previous: Option<&Extra>) {
    let Some(previous) = previous else { return };
    let prompt = flag(value, "promptOmitted") && !flag(previous, "promptOmitted");
    let result = flag(value, "resultOmitted") && !flag(previous, "resultOmitted");
    if !prompt && !result {
        return;
    }
    let prompt_was_omitted = value
        .remove("promptOmitted")
        .is_some_and(|v| v == Value::Bool(true));
    let result_was_omitted = value
        .remove("resultOmitted")
        .is_some_and(|v| v == Value::Bool(true));
    restore_field(
        value,
        previous,
        "prompt",
        "promptOmitted",
        prompt,
        prompt_was_omitted,
    );
    restore_field(
        value,
        previous,
        "result",
        "resultOmitted",
        result,
        result_was_omitted,
    );
}

fn restore_field(
    value: &mut Extra,
    previous: &Extra,
    field: &str,
    marker: &str,
    restore: bool,
    was_omitted: bool,
) {
    if restore {
        match previous.get(field) {
            Some(body) => {
                value.insert(field.to_owned(), body.clone());
            }
            None => {
                value.remove(field);
            }
        }
    } else if was_omitted {
        value.insert(marker.to_owned(), Value::Bool(true));
    }
}

/// `restoreTurnItemBodies` against the newest matching records of `projection`.
pub fn restore_turn_item_bodies(item: &mut TurnItem, projection: &ThreadProjection) {
    match item.item_type.as_str() {
        "user_message" | "assistant_message" => {
            if !item.text_omitted() {
                return;
            }
            let Some(message_id) = item.message_id() else {
                return;
            };
            let Some(message) = projection
                .messages
                .iter()
                .rev()
                .find(|m| m.id == message_id)
            else {
                return;
            };
            item.extra.remove("textOmitted");
            item.extra
                .insert("text".to_owned(), Value::String(message.text.clone()));
        }
        "checkpoint" => {
            if !flag(&item.extra, "filesOmitted") {
                return;
            }
            let Some(checkpoint_id) = item.extra.get("checkpointId") else {
                return;
            };
            let Some(checkpoint) = projection
                .checkpoints
                .iter()
                .rev()
                .find(|c| &c.id == checkpoint_id)
            else {
                return;
            };
            let files = checkpoint.files.clone();
            item.extra.remove("filesOmitted");
            item.extra.insert("files".to_owned(), files);
        }
        "subagent" => {
            if !flag(&item.extra, "promptOmitted") && !flag(&item.extra, "resultOmitted") {
                return;
            }
            let Some(subagent_id) = item.extra.get("subagentId").and_then(Value::as_str) else {
                return;
            };
            let previous = projection
                .subagents
                .iter()
                .rev()
                .find(|subagent| subagent.id.as_str() == subagent_id)
                .map(|subagent| &subagent.extra);
            restore_subagent_bodies(&mut item.extra, previous);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn map(value: Value) -> Extra {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn restores_both_subagent_bodies() {
        let mut value = map(json!({"prompt": "", "promptOmitted": true, "result": null,
            "resultOmitted": true, "progress": "x"}));
        restore_subagent_bodies(
            &mut value,
            Some(&map(json!({"prompt": "brief", "result": "done"}))),
        );
        assert_eq!(
            value,
            map(json!({"prompt": "brief", "result": "done", "progress": "x"}))
        );
    }

    #[test]
    fn keeps_marker_when_previous_is_still_marked() {
        let mut value = map(json!({"prompt": "", "promptOmitted": true, "result": null,
            "resultOmitted": true}));
        restore_subagent_bodies(
            &mut value,
            Some(&map(
                json!({"prompt": "", "promptOmitted": true, "result": "r"}),
            )),
        );
        assert_eq!(
            value,
            map(json!({"prompt": "", "promptOmitted": true, "result": "r"}))
        );
    }

    #[test]
    fn without_previous_or_markers_nothing_changes() {
        let original = map(json!({"prompt": "", "promptOmitted": true}));
        let mut value = original.clone();
        restore_subagent_bodies(&mut value, None);
        assert_eq!(value, original);

        let original = map(json!({"prompt": "p"}));
        let mut value = original.clone();
        restore_subagent_bodies(&mut value, Some(&map(json!({"prompt": "other"}))));
        assert_eq!(value, original);
    }

    #[test]
    fn missing_previous_body_drops_the_field() {
        let mut value = map(json!({"prompt": "", "promptOmitted": true}));
        restore_subagent_bodies(&mut value, Some(&map(json!({}))));
        assert_eq!(value, map(json!({})));
    }
}
