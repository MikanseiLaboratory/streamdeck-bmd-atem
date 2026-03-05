//! Stream Deck plugin for Blackmagic ATEM control.
//! Connects to ATEM switchers via necromancer, caches by IP, and forwards state to Property Inspector.

use futures::{SinkExt, StreamExt};
use necromancer::{protocol::structs::VideoSource, AtemController};
use num_traits::FromPrimitive;
use serde::Deserialize;
use std::{collections::HashMap, env, net::SocketAddrV4, str::FromStr, sync::Arc};
use streamdeck_rs::registration::RegistrationParams;
use streamdeck_rs::{LogMessagePayload, Message, MessageOut, StreamDeckSocket};
use tokio::sync::{mpsc, RwLock};
use tracing::warn;

/// Plugin-wide settings (persisted by Stream Deck).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct GlobalSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
}

/// Deserializes JSON number or string into u8 (for PI textfield/range values).
fn de_u8_from_str_or_num<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match v {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|u| u8::try_from(u).ok())
            .map(Some)
            .ok_or_else(|| D::Error::custom("me: expected u8")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u8>()
            .map(Some)
            .map_err(|_| D::Error::custom("me: invalid number string")),
        _ => Err(D::Error::custom("me: expected number or string")),
    }
}

/// Deserializes JSON number or string into u16 (for PI textfield values).
fn de_u16_from_str_or_num<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match v {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|u| u16::try_from(u).ok())
            .map(Some)
            .ok_or_else(|| D::Error::custom("video_source: expected u16")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u16>()
            .map(Some)
            .map_err(|_| D::Error::custom("video_source: invalid number string")),
        _ => Err(D::Error::custom("video_source: expected number or string")),
    }
}

/// Per-action settings (each key instance).
/// Keys must match PropertyInspector form `setting` attributes: atem_ip, action_type, me, video_source.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct ActionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_type: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    me: Option<u8>,
    #[serde(default, deserialize_with = "de_u16_from_str_or_num")]
    video_source: Option<u16>,
}

/// Message from Property Inspector to plugin (SendToPlugin payload).
type PIMessageIn = serde_json::Value;

/// Message from plugin to Property Inspector (SendToPropertyInspector payload).
type PIMessageOut = serde_json::Value;

/// Request for the main loop to send a message to the Property Inspector.
struct SendToPIRequest {
    action: String,
    context: String,
    payload: PIMessageOut,
}

/// Request to send a log message to Stream Deck via LogMessage API.
struct LogRequest {
    message: String,
}

/// Tracks which (action, context) want ATEM state updates for which IP.
struct PiSubscriptions {
    /// IP -> set of (action, context) that show PI for that ATEM.
    ip_to_contexts: HashMap<String, std::collections::HashSet<(String, String)>>,
    /// (action, context) -> IP, for quick removal when PI disappears.
    context_to_ip: HashMap<(String, String), String>,
}

impl PiSubscriptions {
    fn new() -> Self {
        Self {
            ip_to_contexts: HashMap::new(),
            context_to_ip: HashMap::new(),
        }
    }

    fn add(&mut self, action: String, context: String, ip: String) {
        self.remove(&action, &context);
        self.context_to_ip
            .insert((action.clone(), context.clone()), ip.clone());
        self.ip_to_contexts
            .entry(ip)
            .or_default()
            .insert((action, context));
    }

    fn remove(&mut self, action: &str, context: &str) {
        if let Some(old_ip) = self.context_to_ip.remove(&(action.to_string(), context.to_string())) {
            if let Some(set) = self.ip_to_contexts.get_mut(&old_ip) {
                set.remove(&(action.to_string(), context.to_string()));
                if set.is_empty() {
                    self.ip_to_contexts.remove(&old_ip);
                }
            }
        }
    }

    fn contexts_for_ip(&self, ip: &str) -> Vec<(String, String)> {
        self.ip_to_contexts
            .get(ip)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

fn log_info(log_tx: &Arc<mpsc::UnboundedSender<LogRequest>>, msg: impl AsRef<str>) {
    let _ = log_tx.send(LogRequest {
        message: format!("[INFO] {}", msg.as_ref()),
    });
}

fn log_warn(log_tx: &Arc<mpsc::UnboundedSender<LogRequest>>, msg: impl AsRef<str>) {
    let _ = log_tx.send(LogRequest {
        message: format!("[WARN] {}", msg.as_ref()),
    });
}

#[tokio::main]
async fn main() {
    let (log_tx, mut log_rx) = mpsc::unbounded_channel::<LogRequest>();
    let log_tx = Arc::new(log_tx);

    let args: Vec<String> = env::args().collect();
    log_info(
        &log_tx,
        format!("launch args ({}): {:?}", args.len(), args),
    );

    let params = match RegistrationParams::from_args(std::env::args()) {
        Ok(p) => p,
        Err(e) => {
            std::process::exit(1);
        }
    };

    log_info(&log_tx, format!("connecting to Stream Deck on port {}", params.port));
    let mut socket = match StreamDeckSocket::<
        GlobalSettings,
        ActionSettings,
        PIMessageIn,
        PIMessageOut,
    >::connect(params.port, params.event.clone(), params.uuid.clone())
    .await
    {
        Ok(s) => s,
        Err(e) => {
            std::process::exit(1);
        }
    };

    // Request global settings on startup (expected by Stream Deck after registration)
    let get_ctx = params.uuid.clone();
    if let Err(e) = socket
        .send(MessageOut::GetGlobalSettings { context: get_ctx })
        .await
    {
        warn!("failed to send GetGlobalSettings: {}", e);
    }

    let (pi_tx, mut pi_rx) = mpsc::unbounded_channel::<SendToPIRequest>();

    let pi_tx = Arc::new(pi_tx);
    let atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let pi_subscriptions = Arc::new(RwLock::new(PiSubscriptions::new()));

    loop {
        tokio::select! {
            Some(msg) = socket.next() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        log_warn(&log_tx, format!("socket error: {}", e));
                        continue;
                    }
                };
                if let Err(e) = handle_message(
                    &msg,
                    Arc::clone(&pi_tx),
                    Arc::clone(&atem_cache),
                    Arc::clone(&pi_subscriptions),
                    Arc::clone(&log_tx),
                )
                .await
                {
                    log_warn(&log_tx, format!("handle_message error: {}", e));
                }
            }
            Some(req) = pi_rx.recv() => {
                let out = MessageOut::<GlobalSettings, ActionSettings, PIMessageOut>::SendToPropertyInspector {
                    action: req.action,
                    context: req.context,
                    payload: req.payload,
                };
                if let Err(e) = socket.send(out).await {
                    log_warn(&log_tx, format!("send to PI failed: {}", e));
                }
            }
            Some(log_req) = log_rx.recv() => {
                let out = MessageOut::<GlobalSettings, ActionSettings, PIMessageOut>::LogMessage {
                    payload: LogMessagePayload {
                        message: log_req.message,
                    },
                };
                // Avoid recursive logging on error here.
                let _ = socket.send(out).await;
            }
            else => break,
        }
    }
}

const ATEM_PORT: u16 = 9910;

async fn handle_message(
    msg: &streamdeck_rs::Message<GlobalSettings, ActionSettings, PIMessageIn>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match msg {
        Message::KeyDown {
            action,
            context,
            payload,
            ..
        } => {
            log_info(
                &log_tx,
                format!(
                    "KeyDown action={} context={} settings={:?}",
                    action, context, payload.settings
                ),
            );
            let settings = &payload.settings;
            let ip_str = settings
                .atem_ip
                .clone()
                .filter(|s| !s.is_empty());
            let Some(ip_str) = ip_str.as_deref() else {
                log_warn(&log_tx, "KeyDown: no ATEM IP in action settings");
                return Ok(());
            };
            let controller =
                get_or_connect_atem(
                    atem_cache.clone(),
                    ip_str,
                    pi_tx.clone(),
                    pi_subscriptions.clone(),
                    Arc::clone(&log_tx),
                )
                .await?;
            let me = settings.me.unwrap_or(0);
            let action_type = settings
                .action_type
                .as_deref()
                .unwrap_or("cut")
                .to_lowercase();
            let video_source_id = settings.video_source.unwrap_or(0);
            if let Err(e) = run_atem_action(&*controller, &action_type, me, video_source_id).await {
                log_warn(
                    &log_tx,
                    format!(
                        "ATEM action failed (action_type={}, me={}, source_id={}): {}",
                        action_type, me, video_source_id, e
                    ),
                );
            }
        }
        Message::KeyUp { action, context, payload, .. } => {
            log_info(
                &log_tx,
                format!(
                    "KeyUp action={} context={} settings={:?}",
                    action, context, payload.settings
                ),
            );
        }
        Message::WillAppear { action, context, device, payload } => {
            log_info(
                &log_tx,
                format!(
                    "WillAppear action={} context={} device={:?} settings={:?}",
                    action, context, device, payload.settings
                ),
            );
        }
        Message::WillDisappear { action, context, .. } => {
            log_info(
                &log_tx,
                format!("WillDisappear action={} context={}", action, context),
            );
        }
        Message::DidReceiveSettings {
            action,
            context,
            payload,
            ..
        } => {
            log_info(
                &log_tx,
                format!(
                    "DidReceiveSettings action={} context={} settings={:?}",
                    action, context, payload.settings
                ),
            );
            let ip_str = payload
                .settings
                .atem_ip
                .clone()
                .filter(|s| !s.is_empty());
            if let Some(ip) = ip_str {
                pi_subscriptions
                    .write()
                    .await
                    .add(action.clone(), context.clone(), ip.clone());
                if atem_cache.read().await.contains_key(&ip) {
                    let payload = serde_json::json!({ "event": "atem_state", "connected": true });
                    let _ = pi_tx.send(SendToPIRequest {
                        action: action.clone(),
                        context: context.clone(),
                        payload,
                    });
                }
            }
        }
        Message::PropertyInspectorDidAppear { action, context, .. } => {
            log_info(
                &log_tx,
                format!(
                    "PropertyInspectorDidAppear action={} context={}",
                    action, context
                ),
            );
        }
        Message::PropertyInspectorDidDisappear { action, context, .. } => {
            log_info(
                &log_tx,
                format!(
                    "PropertyInspectorDidDisappear action={} context={}",
                    action, context
                ),
            );
            pi_subscriptions
                .write()
                .await
                .remove(action, context);
        }
        Message::DidReceiveGlobalSettings { payload } => {
            log_info(
                &log_tx,
                format!(
                    "DidReceiveGlobalSettings settings={:?}",
                    payload.settings
                ),
            );
        }
        Message::SendToPlugin { action, context, payload } => {
            log_info(
                &log_tx,
                format!(
                    "SendToPlugin action={} context={} payload={:?}",
                    action, context, payload
                ),
            );
        }
        Message::DeviceDidConnect { device, device_info } => {
            log_info(
                &log_tx,
                format!("DeviceDidConnect device={} info={:?}", device, device_info),
            );
        }
        Message::DeviceDidDisconnect { device } => {
            log_info(
                &log_tx,
                format!("DeviceDidDisconnect device={}", device),
            );
        }
        Message::SystemDidWakeUp => {
            log_info(&log_tx, "SystemDidWakeUp");
        }
        Message::Unknown => {
            log_info(&log_tx, "Unknown message received from Stream Deck");
        }
        _ => {
            log_info(&log_tx, format!("Other message: {:?}", msg));
        }
    }
    Ok(())
}

/// Get or create a cached ATEM controller for the given IP.
/// When a new connection is created, spawns a task that forwards ATEM state updates to the Property Inspector.
async fn get_or_connect_atem(
    cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    ip_str: &str,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    log_tx: Arc<mpsc::UnboundedSender<LogRequest>>,
) -> Result<Arc<AtemController>, Box<dyn std::error::Error + Send + Sync>> {
    {
        let guard = cache.read().await;
        if let Some(ctrl) = guard.get(ip_str) {
            return Ok(Arc::clone(ctrl));
        }
    }
    let ip = std::net::Ipv4Addr::from_str(ip_str)
        .map_err(|e| format!("invalid ATEM IP {:?}: {}", ip_str, e))?;
    let addr = SocketAddrV4::new(ip, ATEM_PORT);
    log_info(&log_tx, format!("connecting to ATEM at {}", addr));
    let controller = AtemController::connect_udp(addr, true).await?;
    let controller = Arc::new(controller);
    let ip_owned = ip_str.to_string();
    cache
        .write()
        .await
        .insert(ip_owned.clone(), Arc::clone(&controller));
    tokio::spawn(forward_atem_state_to_pi(
        ip_owned,
        Arc::clone(&controller),
        pi_tx,
        pi_subscriptions,
    ));
    Ok(controller)
}

/// Task that subscribes to ATEM state updates and sends them to all Property Inspectors subscribed to this IP.
async fn forward_atem_state_to_pi(
    ip: String,
    controller: Arc<AtemController>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
) {
    let mut state_rx = controller.state_update_events();
    while let Ok((_txn, _state_update)) = state_rx.recv().await {
        let contexts = pi_subscriptions.read().await.contexts_for_ip(&ip);
        if contexts.is_empty() {
            continue;
        }
        let payload = serde_json::json!({
            "event": "atem_state",
            "connected": true,
        });
        for (action, context) in contexts {
            let _ = pi_tx.send(SendToPIRequest {
                action,
                context,
                payload: payload.clone(),
            });
        }
    }
}

/// Execute the requested ATEM action (cut, auto, program_input, preview_input, etc.).
async fn run_atem_action(
    controller: &AtemController,
    action_type: &str,
    me: u8,
    video_source_id: u16,
) -> Result<(), necromancer::Error> {
    match action_type {
        "cut" => controller.cut(me).await,
        "auto" => controller.auto(me).await,
        "program_input" | "program" => {
            let source = VideoSource::from_u16(video_source_id).unwrap_or(VideoSource::Unknown);
            controller.set_program_input(me, source).await
        }
        "preview_input" | "preview" => {
            let source = VideoSource::from_u16(video_source_id).unwrap_or(VideoSource::Unknown);
            controller.set_preview_input(me, source).await
        }
        "cut_black" => controller.cut_black(me, true).await,
        "fade_to_black" | "ftb" => controller.toggle_auto_black(me).await,
        _ => {
            warn!("unknown action_type {:?}, defaulting to cut", action_type);
            controller.cut(me).await
        }
    }
}
