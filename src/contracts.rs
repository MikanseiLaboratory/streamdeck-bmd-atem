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
    pub target: Option<TargetSelector>,
    pub me: Option<u8>,
    pub video_source: Option<u16>,
    pub aux_bus: Option<u8>,
    pub key: Option<u8>,
    pub on_air: Option<bool>,
    /// Mix-effect-free media player index, `0` being Media Player 1.
    pub media_player: Option<u8>,
    /// Index inside the still or clip bank the media player plays.
    pub media_index: Option<u8>,
    /// `"still"` or `"clip"`. `None` keeps the bank the player already uses.
    pub media_kind: Option<MediaKind>,
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
            target: read_target(map.get("target")),
            me: read_num(map.get("me")),
            video_source: read_num(map.get("video_source")),
            aux_bus: read_num(map.get("aux_bus")),
            key: read_num(map.get("key")),
            on_air: read_bool(map.get("on_air")),
            media_player: read_num(map.get("media_player")),
            media_index: read_num(map.get("media_index")),
            media_kind: read_media_bank(map.get("media_kind")),
        })
    }
}

/// Media pool bank a media player can play from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MediaKind {
    Still,
    Clip,
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
/// `atem_ip`, `me`, and `aux_bus` are included because the inspector debounces
/// `setSettings`, so a list request can be ahead of the stored action settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
pub struct InspectorCommand {
    #[serde(default, rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub atem_ip: Option<String>,
    #[serde(default)]
    pub target: Option<TargetSelector>,
    #[serde(default)]
    pub me: Option<u8>,
    #[serde(default)]
    pub aux_bus: Option<u8>,
    #[serde(default)]
    pub id: Option<String>,
}

/// One switcher stored in Stream Deck global settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A named set of switcher ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TargetGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
}

/// Which configured switchers an action talks to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum TargetSelector {
    All,
    Group { id: String },
    Instances { ids: Vec<String> },
}

/// Connection label pushed to the property inspector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionState {
    Disabled,
    Connecting,
    Connected,
    Disconnected { message: String },
}

fn default_port() -> u16 {
    9910
}

fn default_true() -> bool {
    true
}

fn default_color() -> String {
    "#4c8dff".into()
}

impl InstanceConfig {
    /// Catalog row created from an action's ATEM address.
    pub fn for_host(host: &str) -> Self {
        Self {
            id: host.to_string(),
            name: host.to_string(),
            host: host.to_string(),
            port: default_port(),
            color: default_color(),
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
    pub groups: Vec<TargetGroup>,
    #[serde(default)]
    pub seeded: bool,
    #[serde(default)]
    pub revision: u32,
}

/// One catalog row sent to an open inspector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InstanceStatusMessage {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub color: String,
    pub enabled: bool,
    pub status: ConnectionState,
}

/// `sendToPropertyInspector` payload listing every configured switcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StatusMessage {
    #[serde(rename = "type")]
    pub r#type: String,
    pub instances: Vec<InstanceStatusMessage>,
}

export_ts!(ActionSettings);
export_ts!(MediaKind);
export_ts!(SelectItem);
export_ts!(AtemStateMessage);
export_ts!(OptionsMessage);
export_ts!(InspectorCommand);
export_ts!(InstanceConfig);
export_ts!(TargetGroup);
export_ts!(TargetSelector);
export_ts!(ConnectionState);
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

/// Adopt `incoming` when it is at least as new as `memory`.
///
/// A lower `revision` is an echo of an older write and is ignored, so deleting a
/// switcher in the configuration window is not undone by a late settings event.
/// The first read sets `seeded` and bumps `revision`.
pub fn merge_catalog(incoming: GlobalSettings, memory: &GlobalSettings) -> (GlobalSettings, bool) {
    if incoming.revision < memory.revision {
        return (memory.clone(), false);
    }
    let mut next = incoming;
    if !next.seeded {
        next.seeded = true;
        next.revision = next.revision.saturating_add(1);
    }
    (next.clone(), next != *memory)
}

/// Enabled instance ids selected by the action.
///
/// An explicit `target` wins. A saved `atem_ip` with no target still addresses that
/// one host. A key with neither addresses every enabled switcher.
pub fn resolve_targets(
    configs: &[InstanceConfig],
    groups: &[TargetGroup],
    settings: &ActionSettings,
) -> Vec<String> {
    let enabled: Vec<&InstanceConfig> = configs.iter().filter(|config| config.enabled).collect();
    if let Some(target) = &settings.target {
        return match target {
            TargetSelector::All => enabled.iter().map(|config| config.id.clone()).collect(),
            TargetSelector::Group { id } => {
                let Some(group) = groups.iter().find(|group| group.id == *id) else {
                    return Vec::new();
                };
                group
                    .members
                    .iter()
                    .filter(|member| enabled.iter().any(|config| config.id == **member))
                    .cloned()
                    .collect()
            }
            TargetSelector::Instances { ids } => ids
                .iter()
                .filter(|id| enabled.iter().any(|config| config.id == **id))
                .cloned()
                .collect(),
        };
    }
    if let Some(host) = normalized_ip(settings) {
        return enabled
            .iter()
            .filter(|config| config.host.trim() == host)
            .map(|config| config.id.clone())
            .collect();
    }
    enabled.iter().map(|config| config.id.clone()).collect()
}

fn read_target(value: Option<&Value>) -> Option<TargetSelector> {
    value.and_then(|value| serde_json::from_value(value.clone()).ok())
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

fn read_media_bank(value: Option<&Value>) -> Option<MediaKind> {
    match read_string(value)?.to_ascii_lowercase().as_str() {
        "still" => Some(MediaKind::Still),
        "clip" => Some(MediaKind::Clip),
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
                target: None,
                me: Some(1),
                video_source: Some(3),
                aux_bus: Some(0),
                key: Some(2),
                on_air: Some(true),
                media_player: None,
                media_index: None,
                media_kind: None,
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
    fn media_settings_accept_strings_and_bank_names() {
        let settings: ActionSettings =
            serde_json::from_str(r#"{"media_player":"1","media_index":2,"media_kind":"Clip"}"#)
                .unwrap();
        assert_eq!(settings.media_player, Some(1));
        assert_eq!(settings.media_index, Some(2));
        assert_eq!(settings.media_kind, Some(MediaKind::Clip));
        let text = serde_json::to_string(&settings.media_kind).unwrap();
        assert_eq!(text, "\"clip\"");

        let unknown: ActionSettings =
            serde_json::from_str(r#"{"media_kind":"mediapool","media_index":""}"#).unwrap();
        assert_eq!(unknown.media_kind, None);
        assert_eq!(unknown.media_index, None);
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
        assert_eq!(settings.instances[0].color, "#4c8dff");
        assert!(settings.groups.is_empty());
    }

    #[test]
    fn merge_catalog_seeds_once_and_keeps_a_newer_local_write() {
        let incoming = GlobalSettings::default();
        let memory = GlobalSettings::default();
        let (merged, changed) = merge_catalog(incoming, &memory);
        assert!(changed);
        assert!(merged.seeded);
        assert_eq!(merged.revision, 1);

        let mut local = merged.clone();
        local
            .instances
            .push(InstanceConfig::for_host("192.168.1.10"));
        local.revision = 2;
        let stale = GlobalSettings {
            seeded: true,
            revision: 1,
            ..GlobalSettings::default()
        };
        let (kept, changed) = merge_catalog(stale, &local);
        assert!(!changed);
        assert_eq!(kept.instances.len(), 1);

        let mut deleted = local.clone();
        deleted.instances.clear();
        let (applied, changed) = merge_catalog(deleted, &local);
        assert!(changed);
        assert!(applied.instances.is_empty());
    }

    #[test]
    fn resolve_targets_prefers_the_selector_over_a_legacy_address() {
        let configs = vec![
            InstanceConfig::for_host("192.168.1.10"),
            InstanceConfig {
                enabled: false,
                ..InstanceConfig::for_host("192.168.1.11")
            },
            InstanceConfig::for_host("192.168.1.12"),
        ];
        let groups = vec![TargetGroup {
            id: "studio".into(),
            name: "Studio".into(),
            members: vec!["192.168.1.10".into(), "192.168.1.11".into()],
        }];
        let legacy = ActionSettings {
            atem_ip: Some("192.168.1.12".into()),
            ..ActionSettings::default()
        };
        assert_eq!(
            resolve_targets(&configs, &groups, &legacy),
            vec!["192.168.1.12".to_string()]
        );
        let grouped = ActionSettings {
            atem_ip: Some("192.168.1.12".into()),
            target: Some(TargetSelector::Group {
                id: "studio".into(),
            }),
            ..ActionSettings::default()
        };
        assert_eq!(
            resolve_targets(&configs, &groups, &grouped),
            vec!["192.168.1.10".to_string()]
        );
        assert_eq!(
            resolve_targets(&configs, &groups, &ActionSettings::default()),
            vec!["192.168.1.10".to_string(), "192.168.1.12".to_string()]
        );
        let empty_group = ActionSettings {
            target: Some(TargetSelector::Group {
                id: "studio".into(),
            }),
            ..ActionSettings::default()
        };
        let empty_members = vec![TargetGroup {
            members: Vec::new(),
            ..groups[0].clone()
        }];
        assert!(resolve_targets(&configs, &empty_members, &empty_group).is_empty());
    }
}
