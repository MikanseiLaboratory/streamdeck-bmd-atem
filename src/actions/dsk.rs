//! Downstream keyer (DSK) actions: Auto, On Air, Cut source, Fill source.

use std::collections::HashMap;
use std::sync::Arc;

use necromancer::{AtemController, AtemState};
use tokio::sync::{mpsc, RwLock};

use crate::{
    de_optional_bool_from_str_or_bool, de_u16_from_str_or_num, de_u8_from_str_or_num,
    get_or_connect_atem, gray_image_data_url, red_image_data_url, video_source_from_id,
    KeyConfig, LogRequest, ShowAlertRequest,
    PiSubscriptions, SendToPIRequest, SetImageRequest,
};

/// Per-action settings for DSK actions.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DskSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atem_ip: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    pub key: Option<u8>,
    #[serde(default, deserialize_with = "de_u16_from_str_or_num")]
    pub video_source: Option<u16>,
    /// For DSK On Air: optional explicit on_air (true/false). If not set, key toggles.
    #[serde(default, deserialize_with = "de_optional_bool_from_str_or_bool")]
    pub on_air: Option<bool>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_key_down_dsk(
    action: &str,
    context: &str,
    payload_settings: &serde_json::Value,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
    alert_tx: Arc<mpsc::UnboundedSender<ShowAlertRequest>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings: DskSettings =
        serde_json::from_value(payload_settings.clone()).unwrap_or_default();
    let ip = settings
        .atem_ip
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let Some(ip) = ip else {
        crate::log_warn(&log_tx, "KeyDown: no ATEM IP in DSK settings");
        let _ = alert_tx.send(ShowAlertRequest {
            context: context.to_string(),
        });
        return Ok(());
    };
    let key = settings.key.unwrap_or(0);

    let controller = get_or_connect_atem(
        atem_cache.clone(),
        &ip,
        pi_tx.clone(),
        pi_subscriptions.clone(),
        Arc::clone(&log_tx),
        Arc::clone(&key_registry),
        Arc::clone(&image_tx),
    )
    .await?;

    let atem_action = match action {
        "dev.miseinul.atem.dskauto" => crate::AtemAction::DskAuto { key },
        "dev.miseinul.atem.dskonair" => {
            let on_air = match settings.on_air {
                Some(b) => b,
                None => {
                    let state_guard = controller.get_state().await;
                    let state: &AtemState = &*state_guard;
                    let current = state.dsk_state.get(&key).map(|s| s.on_air).unwrap_or(false);
                    !current
                }
            };
            crate::AtemAction::SetDskOnAir { key, on_air }
        }
        "dev.miseinul.atem.dskcut" => {
            let source = video_source_from_id(settings.video_source.unwrap_or(0));
            crate::AtemAction::SetDskCutSource { key, source }
        }
        "dev.miseinul.atem.dskfill" => {
            let source = video_source_from_id(settings.video_source.unwrap_or(0));
            crate::AtemAction::SetDskFillSource { key, source }
        }
        _ => {
            crate::log_warn(&log_tx, format!("KeyDown: unknown DSK action {}", action));
            return Ok(());
        }
    };

    if let Err(e) = atem_action.execute(&*controller).await {
        crate::log_warn(
            &log_tx,
            format!("ATEM DSK action failed (uuid={}, key={}): {}", action, key, e),
        );
    } else if action == "dev.miseinul.atem.dskauto" {
        let _ = image_tx.send(SetImageRequest {
            context: context.to_string(),
            image: Some(red_image_data_url()),
        });
    } else if action == "dev.miseinul.atem.dskonair" {
        let state_guard = controller.get_state().await;
        let state: &AtemState = &*state_guard;
        update_dsk_on_air_key_images_from_state(&ip, state, &key_registry, &image_tx, &log_tx)
            .await;
    }

    Ok(())
}

/// Update DSK On Air action key images from ATEM state (on-air = red, off = gray).
#[allow(clippy::too_many_arguments)]
pub async fn update_dsk_on_air_key_images_from_state(
    ip_str: &str,
    state: &AtemState,
    key_registry: &Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: &Arc<mpsc::UnboundedSender<SetImageRequest>>,
    log_tx: &Arc<mpsc::UnboundedSender<LogRequest>>,
) {
    let keys: Vec<(String, u8)> = {
        let guard = key_registry.read().await;
        guard
            .values()
            .filter_map(|k| {
                if k.action != "dev.miseinul.atem.dskonair" {
                    return None;
                }
                let settings: DskSettings =
                    serde_json::from_value(k.settings.clone()).unwrap_or_default();
                let ip_match = settings
                    .atem_ip
                    .as_ref()
                    .map(|s| s.trim() == ip_str)
                    .unwrap_or(false);
                if !ip_match {
                    return None;
                }
                Some((k.context.clone(), settings.key.unwrap_or(0)))
            })
            .collect()
    };

    for (ctx, dsk_key) in keys {
        let on_air = state.dsk_state.get(&dsk_key).map(|s| s.on_air).unwrap_or(false);
        let image = if on_air {
            Some(red_image_data_url())
        } else {
            Some(gray_image_data_url())
        };
        crate::log_info(
            log_tx,
            format!(
                "update_dsk_on_air: ctx={} key={} on_air={}",
                ctx, dsk_key, on_air
            ),
        );
        let _ = image_tx.send(SetImageRequest {
            context: ctx,
            image,
        });
    }
}

/// Update DSK Auto key images from ATEM state (in_transition = red, else gray).
#[allow(clippy::too_many_arguments)]
pub async fn update_dsk_auto_key_images_from_state(
    ip_str: &str,
    state: &AtemState,
    key_registry: &Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: &Arc<mpsc::UnboundedSender<SetImageRequest>>,
) {
    let keys: Vec<(String, u8)> = {
        let guard = key_registry.read().await;
        guard
            .values()
            .filter_map(|k| {
                if k.action != "dev.miseinul.atem.dskauto" {
                    return None;
                }
                let settings: DskSettings =
                    serde_json::from_value(k.settings.clone()).unwrap_or_default();
                let ip_match = settings
                    .atem_ip
                    .as_ref()
                    .map(|s| s.trim() == ip_str)
                    .unwrap_or(false);
                if !ip_match {
                    return None;
                }
                Some((k.context.clone(), settings.key.unwrap_or(0)))
            })
            .collect()
    };

    for (ctx, dsk_key) in keys {
        let in_transition = state
            .dsk_state
            .get(&dsk_key)
            .map(|s| s.in_transition)
            .unwrap_or(false);
        let image = if in_transition {
            Some(red_image_data_url())
        } else {
            Some(gray_image_data_url())
        };
        let _ = image_tx.send(SetImageRequest {
            context: ctx,
            image,
        });
    }
}
