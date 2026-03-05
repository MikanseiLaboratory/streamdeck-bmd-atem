//! Stream Deck plugin for Blackmagic ATEM control.
//! Connects to ATEM switchers via necromancer, caches by IP, and forwards state to Property Inspector.

use futures::{SinkExt, StreamExt};
use necromancer::{protocol::structs::VideoSource, AtemController};
use num_traits::FromPrimitive;
use std::{
    collections::HashMap,
    net::SocketAddrV4,
    str::FromStr,
    sync::Arc,
};
use streamdeck_rs::registration::RegistrationParams;
use streamdeck_rs::{Message, MessageOut, StreamDeckSocket};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// Plugin-wide settings (persisted by Stream Deck).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct GlobalSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
}

/// Per-action settings (each key instance).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct ActionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    me: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let params = RegistrationParams::from_args(std::env::args())
        .map_err(|e| format!("registration: {}", e))?;

    info!("connecting to Stream Deck on port {}", params.port);
    let mut socket = StreamDeckSocket::<
        GlobalSettings,
        ActionSettings,
        PIMessageIn,
        PIMessageOut,
    >::connect(params.port, params.event, params.uuid)
    .await
    .map_err(|e| format!("connect: {}", e))?;

    let (pi_tx, mut pi_rx) = mpsc::unbounded_channel::<SendToPIRequest>();

    let pi_tx = Arc::new(pi_tx);
    let global_settings = Arc::new(RwLock::new(GlobalSettings::default()));
    let atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let pi_subscriptions = Arc::new(RwLock::new(PiSubscriptions::new()));

    loop {
        tokio::select! {
            Some(msg) = socket.next() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("socket error: {}", e);
                        continue;
                    }
                };
                if let Err(e) = handle_message(
                    &msg,
                    Arc::clone(&pi_tx),
                    Arc::clone(&global_settings),
                    Arc::clone(&atem_cache),
                    Arc::clone(&pi_subscriptions),
                )
                .await
                {
                    warn!("handle_message: {}", e);
                }
            }
            Some(req) = pi_rx.recv() => {
                let out = MessageOut::<GlobalSettings, ActionSettings, PIMessageOut>::SendToPropertyInspector {
                    action: req.action,
                    context: req.context,
                    payload: req.payload,
                };
                if let Err(e) = socket.send(out).await {
                    warn!("send to PI: {}", e);
                }
            }
            else => break,
        }
    }

    Ok(())
}

const ATEM_PORT: u16 = 9910;

async fn handle_message(
    msg: &streamdeck_rs::Message<GlobalSettings, ActionSettings, PIMessageIn>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    global_settings: Arc<RwLock<GlobalSettings>>,
    atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match msg {
        Message::KeyDown {
            action: _action,
            context: _context,
            payload,
            ..
        } => {
            let settings = &payload.settings;
            let ip_str = {
                let global = global_settings.read().await;
                settings
                    .atem_ip
                    .clone()
                    .or_else(|| global.atem_ip.clone())
                    .filter(|s| !s.is_empty())
            };
            let Some(ip_str) = ip_str.as_deref() else {
                warn!("KeyDown: no ATEM IP in action or global settings");
                return Ok(());
            };
            let controller =
                get_or_connect_atem(
                    atem_cache.clone(),
                    ip_str,
                    pi_tx.clone(),
                    pi_subscriptions.clone(),
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
                warn!("ATEM action failed: {}", e);
            }
        }
        Message::KeyUp { action, context, payload, .. } => {
            info!("KeyUp action={} context={} settings={:?}", action, context, payload.settings);
        }
        Message::WillAppear { action, context, device, payload } => {
            info!("WillAppear action={} context={} device={:?} settings={:?}", action, context, device, payload.settings);
        }
        Message::WillDisappear { action, context, .. } => {
            info!("WillDisappear action={} context={}", action, context);
        }
        Message::DidReceiveSettings {
            action,
            context,
            payload,
            ..
        } => {
            info!(
                "DidReceiveSettings action={} context={} settings={:?}",
                action, context, payload.settings
            );
            let ip_str = {
                let global = global_settings.read().await;
                payload
                    .settings
                    .atem_ip
                    .clone()
                    .or_else(|| global.atem_ip.clone())
                    .filter(|s| !s.is_empty())
            };
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
            info!("PropertyInspectorDidAppear action={} context={}", action, context);
        }
        Message::PropertyInspectorDidDisappear { action, context, .. } => {
            info!("PropertyInspectorDidDisappear action={} context={}", action, context);
            pi_subscriptions
                .write()
                .await
                .remove(action, context);
        }
        Message::DidReceiveGlobalSettings { payload } => {
            info!("DidReceiveGlobalSettings settings={:?}", payload.settings);
            *global_settings.write().await = payload.settings.clone();
        }
        Message::SendToPlugin { action, context, payload } => {
            info!("SendToPlugin action={} context={} payload={:?}", action, context, payload);
        }
        Message::DeviceDidConnect { device, device_info } => {
            info!("DeviceDidConnect device={} info={:?}", device, device_info);
        }
        Message::DeviceDidDisconnect { device } => {
            info!("DeviceDidDisconnect device={}", device);
        }
        Message::SystemDidWakeUp => {
            info!("SystemDidWakeUp");
        }
        Message::Unknown => {
            info!("Unknown message");
        }
        _ => {
            info!("other message: {:?}", msg);
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
    info!("connecting to ATEM at {}", addr);
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
