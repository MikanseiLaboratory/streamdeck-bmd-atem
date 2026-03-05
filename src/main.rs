//! Stream Deck plugin for Blackmagic ATEM control.
//! Connects to ATEM switchers via necromancer, caches by IP, and forwards state to Property Inspector.

use futures::{SinkExt, StreamExt};
use necromancer::{protocol::structs::VideoSource, AtemController, AtemState, StateUpdate};
use num_traits::FromPrimitive;
use serde::Deserialize;
use std::{collections::HashMap, env, net::SocketAddrV4, str::FromStr, sync::Arc};
use streamdeck_rs::registration::RegistrationParams;
use streamdeck_rs::{ImagePayload, LogMessagePayload, Message, MessageOut, StreamDeckSocket, Target};
use tokio::sync::{mpsc, RwLock};
use tracing::warn;

/// Plugin-wide settings (persisted by Stream Deck).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct GlobalSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
}

/// Per-key configuration we track while the plugin is running.
/// Indexed by Stream Deck context.
#[derive(Clone, Debug)]
struct KeyConfig {
    action: String,
    context: String,
    device: Option<String>,
    settings: ActionSettings,
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
/// Keys must match PropertyInspector form `setting` attributes: atem_ip, me, video_source.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct ActionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    atem_ip: Option<String>,
    #[serde(default, deserialize_with = "de_u8_from_str_or_num")]
    me: Option<u8>,
    #[serde(default, deserialize_with = "de_u16_from_str_or_num")]
    video_source: Option<u16>,
}

/// High-level ATEM action derived from Stream Deck action UUID and settings.
enum AtemAction {
    Cut { me: u8 },
    Auto { me: u8 },
    ProgramInput { me: u8, source: VideoSource },
    PreviewInput { me: u8, source: VideoSource },
    CutBlack { me: u8 },
    FadeToBlack { me: u8 },
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

/// Request to update a key's image via SetImage.
struct SetImageRequest {
    context: String,
    image: Option<String>,
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

// 1x1 solid-color PNGs encoded as base64 (small and efficient).
const RED_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGNgYAAAAAMAASsJTYQAAAAASUVORK5CYII=";
const GREEN_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGNgYAAAAAMAAWgmWQ0AAAAASUVORK5CYII=";

fn red_image_data_url() -> String {
    format!("data:image/png;base64,{}", RED_PNG_BASE64)
}

fn green_image_data_url() -> String {
    format!("data:image/png;base64,{}", GREEN_PNG_BASE64)
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

    let params =
        RegistrationParams::from_args(std::env::args()).unwrap_or_else(|_| std::process::exit(1));

    log_info(
        &log_tx,
        format!("connecting to Stream Deck on port {}", params.port),
    );
    let mut socket =
        StreamDeckSocket::<GlobalSettings, ActionSettings, PIMessageIn, PIMessageOut>::connect(
            params.port,
            params.event.clone(),
            params.uuid.clone(),
        )
        .await
        .unwrap_or_else(|_| std::process::exit(1));

    // Request global settings on startup (expected by Stream Deck after registration)
    let get_ctx = params.uuid.clone();
    if let Err(e) = socket
        .send(MessageOut::GetGlobalSettings { context: get_ctx })
        .await
    {
        warn!("failed to send GetGlobalSettings: {}", e);
    }

    let (pi_tx, mut pi_rx) = mpsc::unbounded_channel::<SendToPIRequest>();
    let (image_tx, mut image_rx) = mpsc::unbounded_channel::<SetImageRequest>();

    let pi_tx = Arc::new(pi_tx);
    let image_tx = Arc::new(image_tx);
    let atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let pi_subscriptions = Arc::new(RwLock::new(PiSubscriptions::new()));
    let key_registry: Arc<RwLock<HashMap<String, KeyConfig>>> =
        Arc::new(RwLock::new(HashMap::new()));

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
                    Arc::clone(&key_registry),
                    Arc::clone(&image_tx),
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
            Some(img_req) = image_rx.recv() => {
                let payload = ImagePayload {
                    image: img_req.image,
                    target: Target::Both,
                    state: None,
                };
                let out = MessageOut::<GlobalSettings, ActionSettings, PIMessageOut>::SetImage {
                    context: img_req.context,
                    payload,
                };
                if let Err(e) = socket.send(out).await {
                    log_warn(&log_tx, format!("send SetImage failed: {}", e));
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
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
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
            let controller = get_or_connect_atem(
                atem_cache.clone(),
                ip_str,
                pi_tx.clone(),
                pi_subscriptions.clone(),
                Arc::clone(&log_tx),
                Arc::clone(&key_registry),
                Arc::clone(&image_tx),
            )
            .await?;
            let me = settings.me.unwrap_or(0);
            let video_source_id = settings.video_source.unwrap_or(0);
            let atem_action = build_atem_action(action, me, video_source_id);
            if let Err(e) = atem_action.execute(&*controller).await {
                log_warn(
                    &log_tx,
                    format!(
                        "ATEM action failed (uuid={}, me={}, source_id={}): {}",
                        action, me, video_source_id, e
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
            key_registry.write().await.insert(
                context.clone(),
                KeyConfig {
                    action: action.clone(),
                    context: context.clone(),
                    device: device.clone(),
                    settings: payload.settings.clone(),
                },
            );
        }
        Message::WillDisappear { action, context, .. } => {
            log_info(
                &log_tx,
                format!("WillDisappear action={} context={}", action, context),
            );
            key_registry.write().await.remove(context);
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
            // Update stored settings for this key (if we are tracking it).
            if let Some(entry) = key_registry.write().await.get_mut(context) {
                entry.settings = payload.settings.clone();
            }
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
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
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
        key_registry,
        image_tx,
    ));
    Ok(controller)
}

/// Task that subscribes to ATEM state updates and sends them to all Property Inspectors subscribed to this IP.
async fn forward_atem_state_to_pi(
    ip: String,
    controller: Arc<AtemController>,
    pi_tx: Arc<mpsc::UnboundedSender<SendToPIRequest>>,
    pi_subscriptions: Arc<RwLock<PiSubscriptions>>,
    key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>,
    image_tx: Arc<mpsc::UnboundedSender<SetImageRequest>>,
) {
    let mut state_rx = controller.state_update_events();
    while let Ok((_txn, updated)) = state_rx.recv().await {
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

        // Only touch key images when ATEM actually reports program/preview changes.
        if !updated.intersects(StateUpdate::PREVIEW_OR_PROGRAM_SOURCE) {
            continue;
        }

        // Query the authoritative state from the controller.
        let state_guard = controller.get_state().await;
        let state: &AtemState = &*state_guard;
        update_program_preview_key_images_from_state(&ip, state, &key_registry, &image_tx).await;
    }
}

/// Update Program/Preview action key images based purely on the ATEM's own state.
async fn update_program_preview_key_images_from_state(
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
            .filter(|k| {
                k.settings
                    .atem_ip
                    .as_deref()
                    .map(|ip| ip == ip_str)
                    .unwrap_or(false)
                    && (k.action == "dev.miseinul.atem.program-input"
                        || k.action == "dev.miseinul.atem.preview-input")
            })
            .map(|k| {
                (
                    k.context.clone(),
                    k.action.clone(),
                    k.settings.me.unwrap_or(0),
                    k.settings.video_source.unwrap_or(0),
                )
            })
            .collect()
    };

    for (ctx, action_uuid, me, configured_source_id) in keys {
        let configured_source =
            VideoSource::from_u16(configured_source_id).unwrap_or(VideoSource::Unknown);

        let (current_source_opt, is_program) = if action_uuid == "dev.miseinul.atem.program-input" {
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

/// Helper to convert a numeric video source ID into a `VideoSource` enum with a safe default.
fn video_source_from_id(video_source_id: u16) -> VideoSource {
    VideoSource::from_u16(video_source_id).unwrap_or(VideoSource::Unknown)
}

/// Build a high-level ATEM action from Stream Deck action UUID and settings.
fn build_atem_action(action_uuid: &str, me: u8, video_source_id: u16) -> AtemAction {
    match action_uuid {
        "dev.miseinul.atem.cut" => AtemAction::Cut { me },
        "dev.miseinul.atem.auto" => AtemAction::Auto { me },
        "dev.miseinul.atem.program-input" => AtemAction::ProgramInput {
            me,
            source: video_source_from_id(video_source_id),
        },
        "dev.miseinul.atem.preview-input" => AtemAction::PreviewInput {
            me,
            source: video_source_from_id(video_source_id),
        },
        "dev.miseinul.atem.cut-black" => AtemAction::CutBlack { me },
        "dev.miseinul.atem.fade-to-black" => AtemAction::FadeToBlack { me },
        // Fallback to a simple cut for unknown/legacy UUIDs.
        _ => AtemAction::Cut { me },
    }
}

impl AtemAction {
    /// Execute this high-level ATEM action against the controller.
    async fn execute(&self, controller: &AtemController) -> Result<(), necromancer::Error> {
        match *self {
            AtemAction::Cut { me } => controller.cut(me).await,
            AtemAction::Auto { me } => controller.auto(me).await,
            AtemAction::ProgramInput { me, source } => {
                controller.set_program_input(me, source).await
            }
            AtemAction::PreviewInput { me, source } => {
                controller.set_preview_input(me, source).await
            }
            AtemAction::CutBlack { me } => controller.cut_black(me, true).await,
            AtemAction::FadeToBlack { me } => controller.toggle_auto_black(me).await,
        }
    }
}
