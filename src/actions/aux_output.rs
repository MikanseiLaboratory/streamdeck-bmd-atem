//! AUX output (auxiliary bus source) action.

use std::collections::HashMap;
use std::sync::Arc;

use necromancer::AtemController;
use tokio::sync::{mpsc, RwLock};

use crate::{
    de_u16_from_str_or_num, de_u8_from_str_or_num, get_or_connect_atem, video_source_from_id,
    KeyConfig, LogRequest, PiSubscriptions, SendToPIRequest, SetImageRequest, ShowAlertRequest,
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
