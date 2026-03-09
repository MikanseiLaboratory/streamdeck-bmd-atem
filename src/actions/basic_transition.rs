use std::collections::HashMap;
use std::sync::Arc;

use necromancer::AtemController;
use tokio::sync::{mpsc, RwLock};

use crate::{
    de_u8_from_str_or_num, get_or_connect_atem, red_image_data_url, AtemAction, KeyConfig,
    LogRequest, PiSubscriptions, SendToPIRequest, SetImageRequest,
};
use necromancer::AtemState;

/// Per-action settings for CUT/AUTO/CUT TO BLACK/FADE TO BLACK actions.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BasicTransitionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atem_ip: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    pub me: Option<u8>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_key_down_basic_transition(
    action: &str,
    context: &str,
    payload_settings: &serde_json::Value,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings: BasicTransitionSettings =
        serde_json::from_value(payload_settings.clone()).unwrap_or_default();
    let ip = settings
        .atem_ip
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let Some(ip) = ip else {
        crate::log_warn(&log_tx, "KeyDown: no ATEM IP in basic transition settings");
        return Ok(());
    };
    let me = settings.me.unwrap_or(0);

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
        "dev.miseinul.atem.cut" => AtemAction::Cut { me },
        "dev.miseinul.atem.auto" => AtemAction::Auto { me },
        "dev.miseinul.atem.cuttoblack" => AtemAction::CutBlack { me },
        "dev.miseinul.atem.fadetoblack" => AtemAction::FadeToBlack { me },
        _ => AtemAction::Cut { me },
    };

    if let Err(e) = atem_action.execute(&*controller).await {
        crate::log_warn(
            &log_tx,
            format!("ATEM action failed (uuid={}, me={}): {}", action, me, e),
        );
    } else if action == "dev.miseinul.atem.auto" {
        let _ = image_tx.send(SetImageRequest {
            context: context.to_string(),
            image: Some(red_image_data_url()),
        });
    }

    Ok(())
}

/// Update Auto Transition key images from ATEM state (in_progress = red, else gray).
#[allow(clippy::too_many_arguments)]
pub async fn update_auto_transition_key_images_from_state(
    ip_str: &str,
    state: &AtemState,
    key_registry: &Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: &Arc<mpsc::UnboundedSender<SetImageRequest>>,
    _log_tx: &Arc<mpsc::UnboundedSender<LogRequest>>,
) {
    let keys: Vec<(String, u8)> = {
        let guard = key_registry.read().await;
        guard
            .values()
            .filter_map(|k| {
                if k.action != "dev.miseinul.atem.auto" {
                    return None;
                }
                let settings: BasicTransitionSettings =
                    serde_json::from_value(k.settings.clone()).unwrap_or_default();
                let ip_match = settings
                    .atem_ip
                    .as_ref()
                    .map(|s| s.trim() == ip_str)
                    .unwrap_or(false);
                if !ip_match {
                    return None;
                }
                Some((k.context.clone(), settings.me.unwrap_or(0)))
            })
            .collect()
    };

    for (ctx, me) in keys {
        let in_progress = state
            .transition_position
            .get(&me)
            .map(|p| p.in_progress)
            .unwrap_or(false);
        let image = if in_progress {
            Some(crate::red_image_data_url())
        } else {
            Some(crate::gray_image_data_url())
        };
        let _ = image_tx.send(SetImageRequest {
            context: ctx,
            image,
        });
    }
}

