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
}

/// `sendToPropertyInspector` payload with dynamic select options.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct OptionsMessage {
    #[serde(rename = "type")]
    pub r#type: String,
    pub kind: String,
    pub items: Vec<SelectItem>,
    pub connected: bool,
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

export_ts!(ActionSettings);
export_ts!(SelectItem);
export_ts!(AtemStateMessage);
export_ts!(OptionsMessage);
export_ts!(InspectorCommand);

impl AtemStateMessage {
    pub fn new(connected: bool) -> Self {
        Self {
            r#type: "atemState".into(),
            connected,
        }
    }
}

impl OptionsMessage {
    pub fn new(kind: impl Into<String>, items: Vec<SelectItem>, connected: bool) -> Self {
        Self {
            r#type: "options".into(),
            kind: kind.into(),
            items,
            connected,
        }
    }
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
}
