use serde_json::Value;

use crate::message::CauseEncoded;

/// Why a request did not succeed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RpcError {
    /// The handler failed with an error its RPC declares, such as an authorization failure.
    #[error("{}", describe_failure(.tag.as_deref(), .message.as_deref()))]
    Failed {
        tag: Option<String>,
        message: Option<String>,
        error: Value,
    },
    /// The server hit an unexpected failure.
    #[error("server defect: {message}")]
    Defect { message: String, defect: Value },
    #[error("request was interrupted")]
    Interrupted,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("connection closed")]
    Disconnected,
}

fn describe_failure(tag: Option<&str>, message: Option<&str>) -> String {
    match (tag, message) {
        (_, Some(message)) => message.to_owned(),
        (Some(tag), None) => tag.to_owned(),
        (None, None) => "request failed".to_owned(),
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn defect_message(defect: &Value) -> String {
    match defect {
        Value::String(text) => text.clone(),
        Value::Null => "unknown defect".to_owned(),
        other => string_field(other, "message").unwrap_or_else(|| other.to_string()),
    }
}

impl RpcError {
    /// A typed failure's `_tag`, e.g. `EnvironmentAuthorizationError`.
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Failed { tag, .. } => tag.as_deref(),
            _ => None,
        }
    }

    pub fn defect(defect: Value) -> Self {
        Self::Defect {
            message: defect_message(&defect),
            defect,
        }
    }

    /// Picks the most informative entry of a failure cause: a typed failure, then a defect,
    /// then an interruption.
    pub fn from_cause(cause: &[CauseEncoded]) -> Self {
        if let Some(error) = cause.iter().find_map(|entry| match entry {
            CauseEncoded::Fail { error } => Some(error),
            _ => None,
        }) {
            return Self::Failed {
                tag: string_field(error, "_tag"),
                message: string_field(error, "message"),
                error: error.clone(),
            };
        }
        if let Some(defect) = cause.iter().find_map(|entry| match entry {
            CauseEncoded::Die { defect } => Some(defect),
            _ => None,
        }) {
            return Self::defect(defect.clone());
        }
        if cause
            .iter()
            .any(|entry| matches!(entry, CauseEncoded::Interrupt { .. }))
        {
            return Self::Interrupted;
        }
        Self::Protocol("failure without a cause".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn typed_failure_wins_over_defects_and_interrupts() {
        let error = RpcError::from_cause(&[
            CauseEncoded::Interrupt { fiber_id: None },
            CauseEncoded::Die {
                defect: json!("boom"),
            },
            CauseEncoded::Fail {
                error: json!({"_tag": "Denied", "message": "no access"}),
            },
        ]);
        assert_eq!(error.tag(), Some("Denied"));
        assert_eq!(error.to_string(), "no access");
    }

    #[test]
    fn defect_wins_over_interrupt() {
        let error = RpcError::from_cause(&[
            CauseEncoded::Interrupt { fiber_id: None },
            CauseEncoded::Die {
                defect: json!({"message": "kaboom", "stack": "..."}),
            },
        ]);
        assert_eq!(error.to_string(), "server defect: kaboom");
        assert_eq!(error.tag(), None);
    }

    #[test]
    fn interrupt_only() {
        assert_eq!(
            RpcError::from_cause(&[CauseEncoded::Interrupt {
                fiber_id: Some(json!(1))
            }]),
            RpcError::Interrupted
        );
    }

    #[test]
    fn empty_cause_is_a_protocol_error() {
        assert!(matches!(RpcError::from_cause(&[]), RpcError::Protocol(_)));
        assert!(matches!(
            RpcError::from_cause(&[CauseEncoded::Unknown]),
            RpcError::Protocol(_)
        ));
    }

    #[test]
    fn failure_display_falls_back_to_tag_then_generic_text() {
        let tagged = RpcError::from_cause(&[CauseEncoded::Fail {
            error: json!({"_tag": "T"}),
        }]);
        assert_eq!(tagged.to_string(), "T");
        let bare = RpcError::from_cause(&[CauseEncoded::Fail { error: json!(42) }]);
        assert_eq!(bare.to_string(), "request failed");
    }

    #[test]
    fn defect_messages() {
        assert_eq!(
            RpcError::defect(json!("plain")).to_string(),
            "server defect: plain"
        );
        assert_eq!(
            RpcError::defect(Value::Null).to_string(),
            "server defect: unknown defect"
        );
        assert_eq!(
            RpcError::defect(json!([1])).to_string(),
            "server defect: [1]"
        );
    }
}
