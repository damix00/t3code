use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::model::ModelSelection;

/// `ServerProviderModel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_legacy: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `ServerProvider`: one configured agent runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProvider {
    pub instance_id: String,
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub enabled: bool,
    pub installed: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub models: Vec<ProviderModel>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ServerProvider {
    /// `isProviderAvailable`.
    pub fn is_available(&self) -> bool {
        self.availability.as_deref() != Some("unavailable")
    }

    /// Whether the composer may offer this provider for new turns.
    pub fn is_selectable(&self) -> bool {
        self.enabled && self.installed && self.is_available() && self.status != "disabled"
    }

    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.instance_id)
    }

    /// The provider's default model, else its first non-legacy model, else its first model.
    pub fn default_model(&self) -> Option<&ProviderModel> {
        self.models
            .iter()
            .find(|model| model.is_default == Some(true))
            .or_else(|| {
                self.models
                    .iter()
                    .find(|model| model.is_legacy != Some(true))
            })
            .or_else(|| self.models.first())
    }

    pub fn default_selection(&self) -> Option<ModelSelection> {
        self.default_model()
            .map(|model| ModelSelection::new(self.instance_id.clone(), model.slug.clone()))
    }
}

/// `ServerConfig`, reduced to what the desktop client reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default)]
    pub environment: Value,
    pub cwd: String,
    /// Upstream decodes this as a forward-compatible array: entries that fail to decode are
    /// dropped rather than failing the whole config.
    #[serde(deserialize_with = "forward_compatible_providers")]
    pub providers: Vec<ServerProvider>,
    #[serde(default)]
    pub settings: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

fn forward_compatible_providers<'de, D>(deserializer: D) -> Result<Vec<ServerProvider>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

impl ServerConfig {
    /// `config.environment.capabilities[name] === true`.
    pub fn has_capability(&self, name: &str) -> bool {
        self.environment
            .get("capabilities")
            .and_then(|capabilities| capabilities.get(name))
            .and_then(Value::as_bool)
            == Some(true)
    }

    pub fn environment_label(&self) -> Option<&str> {
        self.environment.get("label").and_then(Value::as_str)
    }

    pub fn selectable_providers(&self) -> impl Iterator<Item = &ServerProvider> {
        self.providers
            .iter()
            .filter(|provider| provider.is_selectable())
    }

    pub fn provider(&self, instance_id: &str) -> Option<&ServerProvider> {
        self.providers
            .iter()
            .find(|provider| provider.instance_id == instance_id)
    }

    /// The model a new thread starts with when the user has not picked one.
    pub fn default_model_selection(&self) -> Option<ModelSelection> {
        self.selectable_providers()
            .find_map(ServerProvider::default_selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn provider(id: &str, enabled: bool, models: Value) -> Value {
        json!({
            "instanceId": id,
            "driver": "codex",
            "enabled": enabled,
            "installed": true,
            "version": "1.0.0",
            "status": "ready",
            "auth": {"status": "authenticated"},
            "checkedAt": "2026-11-01T10:00:00.000Z",
            "models": models,
            "slashCommands": [],
            "skills": []
        })
    }

    fn model(slug: &str, extra: Value) -> Value {
        let mut value = json!({"slug": slug, "name": slug.to_uppercase(), "isCustom": false,
            "capabilities": null});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    }

    fn config(providers: Value) -> ServerConfig {
        serde_json::from_value(json!({
            "environment": {"environmentId": "e", "label": "My Mac",
                "capabilities": {"threadStop": true, "other": "yes"}},
            "auth": {},
            "cwd": "/home/me",
            "keybindingsConfigPath": "/x",
            "keybindings": [],
            "issues": [],
            "providers": providers,
            "availableEditors": [],
            "observability": {},
            "settings": {}
        }))
        .unwrap()
    }

    #[test]
    fn reads_capabilities_and_label() {
        let config = config(json!([]));
        assert!(config.has_capability("threadStop"));
        assert!(!config.has_capability("other"));
        assert!(!config.has_capability("missing"));
        assert_eq!(config.environment_label(), Some("My Mac"));
        assert_eq!(config.extra["keybindingsConfigPath"], json!("/x"));
    }

    #[test]
    fn drops_providers_that_fail_to_decode() {
        let config = config(json!([
            provider("codex", true, json!([])),
            {"instanceId": "broken"},
        ]));
        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn default_model_prefers_flagged_default_then_non_legacy() {
        let config = config(json!([
            provider(
                "off",
                false,
                json!([model("x", json!({"isDefault": true}))])
            ),
            provider(
                "codex",
                true,
                json!([
                    model("old", json!({"isLegacy": true})),
                    model("new", json!({})),
                    model("best", json!({"isDefault": true}))
                ])
            ),
        ]));
        assert_eq!(
            config.default_model_selection(),
            Some(ModelSelection::new("codex", "best"))
        );
        let codex = config.provider("codex").unwrap();
        assert_eq!(codex.label(), "codex");

        let config = self::config(json!([provider(
            "codex",
            true,
            json!([
                model("old", json!({"isLegacy": true})),
                model("new", json!({}))
            ])
        )]));
        assert_eq!(
            config.default_model_selection(),
            Some(ModelSelection::new("codex", "new"))
        );

        let config = self::config(json!([provider(
            "c",
            true,
            json!([model("old", json!({"isLegacy": true}))])
        )]));
        assert_eq!(
            config.default_model_selection(),
            Some(ModelSelection::new("c", "old"))
        );
    }

    #[test]
    fn selectable_excludes_disabled_uninstalled_unavailable() {
        let mut unavailable = provider("u", true, json!([model("m", json!({}))]));
        unavailable["availability"] = json!("unavailable");
        let mut uninstalled = provider("i", true, json!([model("m", json!({}))]));
        uninstalled["installed"] = json!(false);
        let mut disabled_status = provider("d", true, json!([model("m", json!({}))]));
        disabled_status["status"] = json!("disabled");
        let config = config(json!([
            unavailable,
            uninstalled,
            disabled_status,
            provider("ok", true, json!([]))
        ]));
        let ids: Vec<_> = config
            .selectable_providers()
            .map(|provider| provider.instance_id.as_str())
            .collect();
        assert_eq!(ids, vec!["ok"]);
        // No models: nothing to default to.
        assert_eq!(config.default_model_selection(), None);
    }
}
