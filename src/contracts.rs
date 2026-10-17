//! Settings and Property Inspector messages shared with the generated TypeScript contracts.
//!
//! Field names stay snake_case so profiles saved by the previous inspector keep working.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use streamdeck_plugin::export_ts;
use ts_rs::TS;

/// Settings stored on every ATEM action.
///
/// Each action reads only the fields it needs. Extra keys are ignored.
/// Numbers may arrive as strings because older inspectors saved `<select>` values that way.
#[derive(Clone, Debug, Default, PartialEq, Serialize, TS)]
pub struct ActionSettings {
    pub atem_ip: Option<String>,
    pub me: Option<u8>,
    pub video_source: Option<u16>,
    pub aux_bus: Option<u8>,
    pub key: Option<u8>,
    pub on_air: Option<bool>,
}

impl<'de> Deserialize<'de> for ActionSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(map) = value else {
            return Ok(Self::default());
        };
        Ok(Self {
            atem_ip: read_string(map.get("atem_ip")),
            me: read_num(map.get("me")),
            video_source: read_num(map.get("video_source")),
            aux_bus: read_num(map.get("aux_bus")),
            key: read_num(map.get("key")),
            on_air: read_bool(map.get("on_air")),
        })
    }
}

/// One row in a Property Inspector `<select>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SelectItem {
    pub label: String,
    pub value: String,
}

/// `sendToPropertyInspector` payload reporting whether the ATEM socket is up.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct AtemStateMessage {
    #[serde(rename = "type")]
    pub r#type: String,
    pub connected: bool,
    pub detail: String,
}

/// `sendToPropertyInspector` payload with dynamic select options.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct OptionsMessage {
    #[serde(rename = "type")]
    pub r#type: String,
    pub kind: String,
    pub items: Vec<SelectItem>,
    pub connected: bool,
    pub detail: String,
}

/// `sendToPlugin` payload from the inspector.
///
/// `atem_ip` is included because the inspector debounces `setSettings`, so a list
/// request can carry a newer address than the action has stored.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
pub struct InspectorCommand {
    #[serde(default, rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub atem_ip: Option<String>,
}

/// One switcher stored in Stream Deck global settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_port() -> u16 {
    9910
}

fn default_true() -> bool {
    true
}

impl InstanceConfig {
    /// Catalog row created from an action's ATEM address.
    pub fn for_host(host: &str) -> Self {
        Self {
            id: host.to_string(),
            name: host.to_string(),
            host: host.to_string(),
            port: default_port(),
            enabled: true,
        }
    }
}

/// Plugin-wide switcher catalog.
///
/// `seeded` records that the first global-settings read has been applied.
/// An empty catalog is valid: this plugin does not insert a localhost switcher.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GlobalSettings {
    #[serde(default)]
    pub instances: Vec<InstanceConfig>,
    #[serde(default)]
    pub seeded: bool,
}

/// One catalog row sent to an open inspector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InstanceStatusMessage {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
    pub status: String,
    pub detail: String,
}

/// `sendToPropertyInspector` payload listing every configured switcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StatusMessage {
    #[serde(rename = "type")]
    pub r#type: String,
    pub instances: Vec<InstanceStatusMessage>,
}

export_ts!(ActionSettings);
export_ts!(SelectItem);
export_ts!(AtemStateMessage);
export_ts!(OptionsMessage);
export_ts!(InspectorCommand);
export_ts!(InstanceConfig);
export_ts!(GlobalSettings);
export_ts!(InstanceStatusMessage);
export_ts!(StatusMessage);

impl AtemStateMessage {
    pub fn new(connected: bool, detail: impl Into<String>) -> Self {
        Self {
            r#type: "atemState".into(),
            connected,
            detail: detail.into(),
        }
    }
}

impl OptionsMessage {
    pub fn new(
        kind: impl Into<String>,
        items: Vec<SelectItem>,
        connected: bool,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            r#type: "options".into(),
            kind: kind.into(),
            items,
            connected,
            detail: detail.into(),
        }
    }
}

impl StatusMessage {
    pub fn new(instances: Vec<InstanceStatusMessage>) -> Self {
        Self {
            r#type: "status".into(),
            instances,
        }
    }
}

/// Fold locally added rows and live action hosts into `incoming`.
///
/// The returned flag is true when the result differs from `incoming`, including
/// the first time `seeded` flips on. Callers persist only then, so an echoed
/// `didReceiveGlobalSettings` does not write the catalog again.
pub fn merge_catalog(
    mut incoming: GlobalSettings,
    memory: &[InstanceConfig],
    hosts: &[String],
) -> (GlobalSettings, bool) {
    let before = incoming.clone();
    for instance in memory {
        if !incoming.instances.iter().any(|item| item.id == instance.id) {
            incoming.instances.push(instance.clone());
        }
    }
    for host in hosts {
        let host = host.trim();
        if host.is_empty() {
            continue;
        }
        if incoming
            .instances
            .iter()
            .any(|item| item.host.trim() == host)
        {
            continue;
        }
        incoming.instances.push(InstanceConfig::for_host(host));
    }
    if !incoming.seeded {
        incoming.seeded = true;
    }
    let changed = incoming != before;
    (incoming, changed)
}

/// Trimmed ATEM address, if the action has one.
pub fn normalized_ip(settings: &ActionSettings) -> Option<String> {
    settings
        .atem_ip
        .as_ref()
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
}

fn read_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        _ => None,
    }
}

fn read_num<T>(value: Option<&Value>) -> Option<T>
where
    T: std::str::FromStr + TryFrom<u64>,
{
    match value {
        Some(Value::Number(number)) => number.as_u64().and_then(|value| T::try_from(value).ok()),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                text.parse().ok()
            }
        }
        _ => None,
    }
}

fn read_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::String(text)) => match text.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Wrap `index` by `ticks` inside a non-empty list.
pub fn cycle_index(len: usize, index: usize, ticks: i64) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    ((index as i64) + ticks).rem_euclid(len) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_property_inspector_strings() {
        let settings: ActionSettings = serde_json::from_str(
            r#"{"atem_ip":" 192.168.1.10 ","me":"1","video_source":"3","aux_bus":0,"key":"2","on_air":"true"}"#,
        )
        .unwrap();
        assert_eq!(
            settings,
            ActionSettings {
                atem_ip: Some("192.168.1.10".into()),
                me: Some(1),
                video_source: Some(3),
                aux_bus: Some(0),
                key: Some(2),
                on_air: Some(true),
            }
        );
        assert_eq!(normalized_ip(&settings).as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn empty_and_missing_fields_stay_unset() {
        let settings: ActionSettings =
            serde_json::from_str(r#"{"atem_ip":"","me":"","on_air":""}"#).unwrap();
        assert_eq!(settings, ActionSettings::default());
    }

    #[test]
    fn cycle_index_wraps_in_both_directions() {
        assert_eq!(cycle_index(4, 0, -1), 3);
        assert_eq!(cycle_index(4, 3, 2), 1);
        assert_eq!(cycle_index(0, 0, 5), 0);
    }

    #[test]
    fn global_settings_default_port_and_enabled() {
        let settings: GlobalSettings = serde_json::from_str(
            r#"{"instances":[{"id":"studio","name":"Studio","host":"192.168.1.10"}]}"#,
        )
        .unwrap();
        assert!(!settings.seeded);
        assert!(settings.instances[0].enabled);
        assert_eq!(settings.instances[0].port, 9910);
    }

    #[test]
    fn merge_catalog_keeps_local_rows_and_seeds_once() {
        let incoming = GlobalSettings::default();
        let memory = vec![InstanceConfig::for_host("192.168.1.10")];
        let (merged, changed) = merge_catalog(incoming, &memory, &["192.168.1.20".into()]);
        assert!(changed);
        assert!(merged.seeded);
        assert_eq!(merged.instances.len(), 2);
        assert_eq!(merged.instances[0].host, "192.168.1.10");
        assert_eq!(merged.instances[1].id, "192.168.1.20");

        let (again, changed) = merge_catalog(merged.clone(), &merged.instances, &[]);
        assert!(!changed);
        assert_eq!(again, merged);
    }
}
