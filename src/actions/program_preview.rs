use std::collections::HashMap;
use std::sync::Arc;

use necromancer::{protocol::structs::VideoSource, AtemController, AtemState};
use tokio::sync::{mpsc, RwLock};

use crate::{
    de_u16_from_str_or_num, de_u8_from_str_or_num, get_or_connect_atem, green_image_data_url,
    red_image_data_url, video_source_from_id, KeyConfig, LogRequest, PiSubscriptions,
    SendToPIRequest, SetImageRequest,
};

/// Per-action settings for PROGRAM/PREVIEW INPUT actions.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProgramPreviewSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atem_ip: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    pub me: Option<u8>,
    #[serde(default, deserialize_with = "de_u16_from_str_or_num")]
    pub video_source: Option<u16>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_key_down_program_preview(
    action: &str,
    payload_settings: &serde_json::Value,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings: ProgramPreviewSettings =
        serde_json::from_value(payload_settings.clone()).unwrap_or_default();
    let ip = settings
        .atem_ip
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let Some(ip) = ip else {
        crate::log_warn(&log_tx, "KeyDown: no ATEM IP in program/preview settings");
        return Ok(());
    };
    let me = settings.me.unwrap_or(0);
    let source = video_source_from_id(settings.video_source.unwrap_or(0));

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

    let atem_action = if action == "dev.miseinul.atem.programinput" {
        crate::AtemAction::ProgramInput { me, source }
    } else {
        crate::AtemAction::PreviewInput { me, source }
    };

    if let Err(e) = atem_action.execute(&*controller).await {
        crate::log_warn(
            &log_tx,
            format!("ATEM action failed (uuid={}, me={}): {}", action, me, e),
        );
    }

    Ok(())
}

/// Update Program/Preview action key images based purely on the ATEM's own state.
#[allow(clippy::too_many_arguments)]
pub async fn update_program_preview_key_images_from_state(
    ip_str: &str,
    state: &AtemState,
    key_registry: &Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: &Arc<mpsc::UnboundedSender<SetImageRequest>>,
) {
    // Snapshot the keys we care about (Program/Preview actions for this ATEM IP).
    let keys: Vec<(String, String, u8, u16)> = {
        let guard = key_registry.read().await;
        guard
            .values()
            .filter_map(|k| {
                if k.action != "dev.miseinul.atem.programinput"
                    && k.action != "dev.miseinul.atem.previewinput"
                {
                    return None;
                }

                let settings: ProgramPreviewSettings =
                    serde_json::from_value(k.settings.clone()).unwrap_or_default();

                let ip_match = settings
                    .atem_ip
                    .as_deref()
                    .map(|ip| ip == ip_str)
                    .unwrap_or(false);

                if !ip_match {
                    return None;
                }

                Some((
                    k.context.clone(),
                    k.action.clone(),
                    settings.me.unwrap_or(0),
                    settings.video_source.unwrap_or(0),
                ))
            })
            .collect()
    };

    for (ctx, action_uuid, me, configured_source_id) in keys {
        let configured_source = video_source_from_id(configured_source_id);

        let (current_source_opt, is_program) = if action_uuid == "dev.miseinul.atem.programinput" {
            (state.get_program_source(me), true)
        } else {
            (state.get_preview_source(me), false)
        };

        let is_active = current_source_opt
            .map(|current| current == configured_source)
            .unwrap_or(false);

        let image = if is_program {
            // Program bus: active is red, inactive is green.
            if is_active {
                Some(red_image_data_url())
            } else {
                Some(green_image_data_url())
            }
        } else {
            // Preview bus: active is green, inactive is red.
            if is_active {
                Some(green_image_data_url())
            } else {
                Some(red_image_data_url())
            }
        };

        let _ = image_tx.send(SetImageRequest {
            context: ctx,
            image,
        });
    }
}

