//! AUX output (auxiliary bus source) action.

use std::collections::HashMap;
use std::sync::Arc;

use necromancer::{AtemController, AtemState};
use tokio::sync::{mpsc, RwLock};

use crate::{
    de_u16_from_str_or_num, de_u8_from_str_or_num, get_or_connect_atem, video_source_from_id,
    KeyConfig, LogRequest, PiSubscriptions, SendToPIRequest, SetImageRequest, SetSettingsRequest,
    ShowAlertRequest,
};

/// Per-action settings for AUX Output.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AuxOutputSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atem_ip: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    pub aux_bus: Option<u8>,
    #[serde(default, deserialize_with = "de_u16_from_str_or_num")]
    pub video_source: Option<u16>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_key_down_aux_output(
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
    let settings: AuxOutputSettings =
        serde_json::from_value(payload_settings.clone()).unwrap_or_default();
    let ip = settings
        .atem_ip
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let Some(ip) = ip else {
        crate::log_warn(&log_tx, "KeyDown: no ATEM IP in AUX output settings");
        let _ = alert_tx.send(ShowAlertRequest {
            context: context.to_string(),
        });
        return Ok(());
    };
    let aux_bus = settings.aux_bus.unwrap_or(0);
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

    let action = crate::AtemAction::AuxSource { aux_bus, source };
    if let Err(e) = action.execute(&*controller).await {
        crate::log_warn(
            &log_tx,
            format!(
                "ATEM AUX output failed (aux_bus={}, source={:?}): {}",
                aux_bus, source, e
            ),
        );
    }

    Ok(())
}

/// Build ordered list of AUX source IDs (inputs then M/E Program/Preview), same order as atem_aux_video_sources.
pub fn aux_source_ids_from_state(state: &AtemState) -> Vec<u16> {
    let mut ids: Vec<u16> = state
        .input_properties
        .keys()
        .filter_map(|&src| {
            let id = src as u16;
            if (1..=40).contains(&id) {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    const ME_SOURCE_IDS: [u16; 8] = [
        10010, 10011, 10020, 10021, 10030, 10031, 10040, 10041,
    ];
    let mes = state.topology.mes as usize;
    ids.extend(ME_SOURCE_IDS.iter().take(mes * 2).copied());
    if ids.is_empty() {
        ids = (1u16..=20).collect();
        ids.extend([10010, 10011, 10020, 10021].iter().copied());
    }
    ids
}

/// Handle DialRotate for AUX Output: cycle source by ticks (SetNext-style), then persist via SetSettings.
#[allow(clippy::too_many_arguments)]
pub async fn handle_dial_rotate_aux_output(
    context: &str,
    payload_settings: &serde_json::Value,
    ticks: i64,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
    set_settings_tx: Arc<mpsc::UnboundedSender<SetSettingsRequest>>,
    alert_tx: Arc<mpsc::UnboundedSender<ShowAlertRequest>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut settings: AuxOutputSettings =
        serde_json::from_value(payload_settings.clone()).unwrap_or_default();
    let ip = settings
        .atem_ip
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let Some(ip) = ip else {
        crate::log_warn(&log_tx, "DialRotate AUX: no ATEM IP in settings");
        let _ = alert_tx.send(ShowAlertRequest {
            context: context.to_string(),
        });
        return Ok(());
    };
    let controller = get_or_connect_atem(
        atem_cache,
        &ip,
        pi_tx,
        pi_subscriptions,
        log_tx.clone(),
        key_registry.clone(),
        image_tx,
    )
    .await?;
    let state_guard = controller.get_state().await;
    let state: &AtemState = &*state_guard;
    let source_ids = aux_source_ids_from_state(state);
    if source_ids.is_empty() {
        return Ok(());
    }
    let current_id = settings.video_source.unwrap_or(1) as u16;
    let index = source_ids
        .iter()
        .position(|&id| id == current_id)
        .unwrap_or(0);
    let len = source_ids.len() as i64;
    let new_index = ((index as i64) + ticks).rem_euclid(len) as usize;
    let new_id = source_ids[new_index];
    let source = video_source_from_id(new_id);
    let aux_bus = settings.aux_bus.unwrap_or(0);

    let action = crate::AtemAction::AuxSource { aux_bus, source };
    if let Err(e) = action.execute(&*controller).await {
        crate::log_warn(
            &log_tx,
            format!(
                "DialRotate AUX: ATEM AuxSource failed (aux_bus={}, source_id={}): {}",
                aux_bus, new_id, e
            ),
        );
        return Ok(());
    }

    settings.video_source = Some(new_id);
    let new_settings = serde_json::to_value(&settings).unwrap_or_default();
    let _ = set_settings_tx.send(SetSettingsRequest {
        context: context.to_string(),
        settings: new_settings.clone(),
    });
    // Update key_registry so the next event sees the new source.
    if let Some(entry) = key_registry.write().await.get_mut(context) {
        entry.settings = new_settings;
    }

    Ok(())
}
