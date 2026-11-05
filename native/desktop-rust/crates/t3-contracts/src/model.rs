use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// The provider instance and model a thread or turn runs with.
///
/// Older records name the provider in `provider` instead of `instanceId`; decoding accepts both,
/// like the TypeScript schema, and encoding always writes `instanceId`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSelection {
    #[serde(rename = "instanceId")]
    pub instance_id: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

impl ModelSelection {
    pub fn new(instance_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            model: model.into(),
            options: None,
        }
    }
}

impl<'de> Deserialize<'de> for ModelSelection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Source {
            #[serde(rename = "instanceId", default)]
            instance_id: Option<Value>,
            #[serde(default)]
            provider: Option<Value>,
            model: String,
            #[serde(default)]
            options: Option<Value>,
        }

        let source = Source::deserialize(deserializer)?;
        let instance_id = match (source.instance_id, source.provider) {
            (Some(Value::String(id)), _) | (None, Some(Value::String(id))) => id,
            _ => {
                return Err(serde::de::Error::custom(
                    "model selection is missing instanceId",
                ));
            }
        };
        let instance_id = instance_id.trim().to_owned();
        let model = source.model.trim().to_owned();
        if instance_id.is_empty() || model.is_empty() {
            return Err(serde::de::Error::custom(
                "model selection has an empty field",
            ));
        }
        Ok(Self {
            instance_id,
            model,
            options: source.options,
        })
    }
}

macro_rules! string_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident = $wire:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant),*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire),*
                }
            }

            /// Reads a wire value; `None` for values newer than this client.
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some($name::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

string_enum!(
    /// `OrchestrationV2RunStatus`.
    RunStatus {
        Preparing = "preparing",
        Queued = "queued",
        Starting = "starting",
        Running = "running",
        Waiting = "waiting",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
        Cancelled = "cancelled",
        RolledBack = "rolled_back",
    }
);

impl RunStatus {
    /// Statuses `isLiveSubagentShell` and the sidebar treat as work in progress.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }

    /// Statuses the stop button interrupts (`interruptThreadTurn`).
    pub fn is_interruptible(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }
}

string_enum!(
    /// How much the agent may do without asking.
    RuntimeMode {
        ApprovalRequired = "approval-required",
        AutoAcceptEdits = "auto-accept-edits",
        Auto = "auto",
        FullAccess = "full-access",
    }
);

impl Default for RuntimeMode {
    /// `DEFAULT_RUNTIME_MODE`.
    fn default() -> Self {
        Self::FullAccess
    }
}

impl RuntimeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "Ask first",
            Self::AutoAcceptEdits => "Auto-accept edits",
            Self::Auto => "Auto",
            Self::FullAccess => "Full access",
        }
    }
}

string_enum!(
    InteractionMode {
        Default = "default",
        Plan = "plan",
    }
);

#[allow(clippy::derivable_impls)] // `string_enum!` defines the enum, so it cannot derive.
impl Default for InteractionMode {
    fn default() -> Self {
        Self::Default
    }
}

string_enum!(
    /// Where a command originated. The desktop renderer reports `web`, like upstream.
    CreationSource {
        Web = "web",
        Mobile = "mobile",
        Mcp = "mcp",
        Provider = "provider",
        Server = "server",
    }
);

string_enum!(
    UserMessageInputIntent {
        TurnStart = "turn_start",
        QueuedTurn = "queued_turn",
        Steer = "steer",
        PromotedQueuedToSteer = "promoted_queued_to_steer",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn model_selection_decodes_instance_id() {
        let selection: ModelSelection =
            serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5"})).unwrap();
        assert_eq!(selection, ModelSelection::new("codex", "gpt-5"));
    }

    #[test]
    fn model_selection_accepts_legacy_provider_field() {
        let selection: ModelSelection = serde_json::from_value(
            json!({"provider": "claudeAgent", "model": "opus", "options": {"effort": "high"}}),
        )
        .unwrap();
        assert_eq!(selection.instance_id, "claudeAgent");
        assert_eq!(selection.options, Some(json!({"effort": "high"})));
        assert_eq!(
            serde_json::to_value(&selection).unwrap(),
            json!({"instanceId": "claudeAgent", "model": "opus", "options": {"effort": "high"}})
        );
    }

    #[test]
    fn model_selection_prefers_instance_id_over_provider() {
        let selection: ModelSelection = serde_json::from_value(
            json!({"instanceId": "codex_work", "provider": "codex", "model": "m"}),
        )
        .unwrap();
        assert_eq!(selection.instance_id, "codex_work");
    }

    #[test]
    fn model_selection_rejects_missing_or_blank_fields() {
        for value in [
            json!({"model": "m"}),
            json!({"instanceId": 3, "model": "m"}),
            json!({"instanceId": "  ", "model": "m"}),
            json!({"instanceId": "c", "model": " "}),
            json!({"instanceId": "c"}),
        ] {
            assert!(
                serde_json::from_value::<ModelSelection>(value.clone()).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn model_selection_trims_like_trimmed_non_empty_string() {
        let selection: ModelSelection =
            serde_json::from_value(json!({"instanceId": " codex ", "model": " gpt "})).unwrap();
        assert_eq!(selection, ModelSelection::new("codex", "gpt"));
    }

    #[test]
    fn string_enums_round_trip_every_variant() {
        for status in RunStatus::ALL {
            assert_eq!(RunStatus::parse(status.as_str()), Some(*status));
            let wire = serde_json::to_value(status).unwrap();
            assert_eq!(wire, json!(status.as_str()));
        }
        for mode in RuntimeMode::ALL {
            assert_eq!(RuntimeMode::parse(mode.as_str()), Some(*mode));
        }
        for intent in UserMessageInputIntent::ALL {
            assert_eq!(
                UserMessageInputIntent::parse(intent.as_str()),
                Some(*intent)
            );
        }
        assert_eq!(RunStatus::parse("future_status"), None);
    }

    #[test]
    fn live_and_interruptible_statuses_match_upstream() {
        let live: Vec<_> = RunStatus::ALL
            .iter()
            .filter(|s| s.is_live())
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            live,
            ["preparing", "queued", "starting", "running", "waiting"]
        );
        let interruptible: Vec<_> = RunStatus::ALL
            .iter()
            .filter(|s| s.is_interruptible())
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            interruptible,
            ["preparing", "starting", "running", "waiting"]
        );
    }

    #[test]
    fn defaults_match_upstream() {
        assert_eq!(RuntimeMode::default(), RuntimeMode::FullAccess);
        assert_eq!(InteractionMode::default().as_str(), "default");
    }
}
