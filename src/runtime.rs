//! Shared ATEM connections, key tallies, and Property Inspector updates.
//!
//! One [`AtemController`] is cached per switcher IP. Background tasks push bus
//! changes into key images and into any open inspector for that address.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::{Arc, Weak};

use necromancer::protocol::structs::VideoSource;
use necromancer::{AtemController, AtemState, StateUpdate};
use num_traits::FromPrimitive;
use serde_json::Value;
use streamdeck_plugin::{async_trait, CommandSender, PluginService, Result, Target};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::contracts::{
    cycle_index, normalized_ip, ActionSettings, AtemStateMessage, InspectorCommand, OptionsMessage,
    SelectItem,
};

const ATEM_PORT: u16 = 9910;
const IMAGE_RED: &str = "Images/red.png";
const IMAGE_GREEN: &str = "Images/green.png";
const IMAGE_GRAY: &str = "Images/gray.png";

const ME_PROGRAM_PREVIEW: [(u16, &str); 8] = [
    (10010, "M/E 1 Program"),
    (10011, "M/E 1 Preview"),
    (10020, "M/E 2 Program"),
    (10021, "M/E 2 Preview"),
    (10030, "M/E 3 Program"),
    (10031, "M/E 3 Preview"),
    (10040, "M/E 4 Program"),
    (10041, "M/E 4 Preview"),
];

#[derive(Clone, Debug)]
struct KeyRecord {
    action: String,
    settings: ActionSettings,
}

struct LiveAtem {
    controller: Arc<AtemController>,
    /// Dropping this sender ends the forward task and releases the socket.
    _stop: mpsc::UnboundedSender<()>,
}

/// Plugin-wide state passed to every action.
#[derive(Clone)]
pub struct AppState {
    runtime: Arc<Runtime>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new_cyclic(|weak| Runtime {
                this: weak.clone(),
                connections: RwLock::new(HashMap::new()),
                keys: RwLock::new(HashMap::new()),
                inspectors: RwLock::new(HashMap::new()),
                sender: Mutex::new(None),
            }),
        }
    }
}

struct Runtime {
    this: Weak<Runtime>,
    connections: RwLock<HashMap<String, LiveAtem>>,
    keys: RwLock<HashMap<String, KeyRecord>>,
    /// Inspector contexts currently visible, mapped to the ATEM IP they watch.
    inspectors: RwLock<HashMap<String, Option<String>>>,
    sender: Mutex<Option<CommandSender>>,
}

#[async_trait]
impl PluginService for AppState {
    async fn start(&self, sender: CommandSender) -> Result<()> {
        *self.runtime.sender.lock().await = Some(sender);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.runtime.connections.write().await.clear();
        Ok(())
    }
}

impl AppState {
    pub async fn on_will_appear(&self, action: &str, context: &str, settings: &ActionSettings) {
        self.runtime
            .upsert_key(action, context, settings.clone())
            .await;
        self.runtime.ensure_connected(settings).await;
        if let Some(ip) = normalized_ip(settings) {
            self.runtime.refresh_ip(&ip).await;
        }
    }

    pub async fn on_will_disappear(&self, context: &str) {
        self.runtime.keys.write().await.remove(context);
        self.runtime.inspectors.write().await.remove(context);
        self.runtime.evict_unused().await;
    }

    pub async fn on_settings_changed(
        &self,
        action: &str,
        context: &str,
        settings: &ActionSettings,
    ) {
        self.runtime
            .upsert_key(action, context, settings.clone())
            .await;
        self.runtime.evict_unused().await;
        self.runtime.ensure_connected(settings).await;
        if let Some(ip) = normalized_ip(settings) {
            self.runtime.refresh_ip(&ip).await;
        }
        if self.runtime.inspectors.read().await.contains_key(context) {
            self.runtime
                .inspectors
                .write()
                .await
                .insert(context.to_string(), normalized_ip(settings));
            self.push_inspector(action, context, settings).await;
        }
    }

    pub async fn on_property_inspector_did_appear(
        &self,
        action: &str,
        context: &str,
        settings: &ActionSettings,
    ) {
        self.runtime
            .inspectors
            .write()
            .await
            .insert(context.to_string(), normalized_ip(settings));
        self.runtime.ensure_connected(settings).await;
        self.push_inspector(action, context, settings).await;
    }

    pub async fn on_property_inspector_did_disappear(&self, context: &str) {
        self.runtime.inspectors.write().await.remove(context);
    }

    pub async fn on_property_inspector_message(&self, context: &str, payload: &Value) {
        let command =
            serde_json::from_value::<InspectorCommand>(payload.clone()).unwrap_or_default();
        if command.r#type != "requestOptions" {
            return;
        }
        let mut settings = self
            .runtime
            .keys
            .read()
            .await
            .get(context)
            .map(|record| record.settings.clone())
            .unwrap_or_default();
        if let Some(ip) = command
            .atem_ip
            .as_ref()
            .map(|ip| ip.trim().to_string())
            .filter(|ip| !ip.is_empty())
        {
            settings.atem_ip = Some(ip);
        }
        self.runtime.ensure_connected(&settings).await;
        let connected = self
            .runtime
            .is_connected(normalized_ip(&settings).as_deref())
            .await;
        let items = self.runtime.options(&command.kind, &settings).await;
        self.runtime
            .send_pi(
                context,
                &OptionsMessage::new(command.kind, items, connected),
            )
            .await;
    }

    pub async fn press_transition(
        &self,
        context: &str,
        settings: &ActionSettings,
        kind: TransitionKind,
    ) {
        let Some(controller) = self.controller_for(context, settings, false).await else {
            return;
        };
        let me = settings.me.unwrap_or(0);
        let result = match kind {
            TransitionKind::Cut => controller.cut(me).await,
            TransitionKind::Auto => controller.auto(me).await,
            TransitionKind::CutBlack => controller.cut_black(me, true).await,
            TransitionKind::FadeToBlack => controller.toggle_auto_black(me).await,
        };
        if let Err(error) = result {
            self.runtime
                .log(&format!("ATEM transition failed (me={me}): {error}"))
                .await;
            return;
        }
        if kind == TransitionKind::Auto {
            self.runtime.set_image(context, IMAGE_RED).await;
        }
    }

    pub async fn press_bus(&self, context: &str, settings: &ActionSettings, program: bool) {
        let Some(controller) = self.controller_for(context, settings, false).await else {
            return;
        };
        let me = settings.me.unwrap_or(0);
        let source = video_source_from_id(settings.video_source.unwrap_or(0));
        let result = if program {
            controller.set_program_input(me, source).await
        } else {
            controller.set_preview_input(me, source).await
        };
        if let Err(error) = result {
            self.runtime
                .log(&format!("ATEM bus change failed (me={me}): {error}"))
                .await;
            return;
        }
        let state = controller.get_state().await;
        if let Some(ip) = normalized_ip(settings) {
            self.runtime
                .refresh_program_preview(&ip, &state, true)
                .await;
            self.runtime
                .refresh_program_preview(&ip, &state, false)
                .await;
        }
    }

    pub async fn press_aux(&self, context: &str, settings: &ActionSettings) {
        let Some(controller) = self.controller_for(context, settings, true).await else {
            return;
        };
        let aux_bus = settings.aux_bus.unwrap_or(0);
        let source = video_source_from_id(settings.video_source.unwrap_or(0));
        if let Err(error) = controller.set_aux_source(aux_bus, source).await {
            self.runtime
                .log(&format!(
                    "ATEM AUX change failed (aux_bus={aux_bus}, source={source:?}): {error}"
                ))
                .await;
        }
    }

    /// Cycle the AUX source and return the settings that should be persisted.
    pub async fn rotate_aux(
        &self,
        context: &str,
        settings: &ActionSettings,
        ticks: i64,
    ) -> Option<ActionSettings> {
        let controller = self.controller_for(context, settings, true).await?;
        let state = controller.get_state().await;
        let source_ids = aux_source_ids(&state);
        if source_ids.is_empty() {
            return None;
        }
        let current = settings.video_source.unwrap_or(1);
        let index = source_ids.iter().position(|id| *id == current).unwrap_or(0);
        let new_id = source_ids[cycle_index(source_ids.len(), index, ticks)];
        let aux_bus = settings.aux_bus.unwrap_or(0);
        let source = video_source_from_id(new_id);
        if let Err(error) = controller.set_aux_source(aux_bus, source).await {
            self.runtime
                .log(&format!(
                    "ATEM AUX dial failed (aux_bus={aux_bus}, source_id={new_id}): {error}"
                ))
                .await;
            return None;
        }
        let mut updated = settings.clone();
        updated.video_source = Some(new_id);
        if let Some(record) = self.runtime.keys.write().await.get_mut(context) {
            record.settings = updated.clone();
        }
        Some(updated)
    }

    pub async fn press_dsk(&self, context: &str, settings: &ActionSettings, kind: DskKind) {
        let Some(controller) = self.controller_for(context, settings, true).await else {
            return;
        };
        let key = settings.key.unwrap_or(0);
        let result = match kind {
            DskKind::Auto => controller.dsk_auto(key).await,
            DskKind::OnAir => {
                let on_air = match settings.on_air {
                    Some(on_air) => on_air,
                    None => {
                        let state = controller.get_state().await;
                        let current = state
                            .dsk_state
                            .get(&key)
                            .map(|dsk| dsk.on_air)
                            .unwrap_or(false);
                        !current
                    }
                };
                controller.set_dsk_on_air(key, on_air).await
            }
            DskKind::Cut => {
                controller
                    .set_dsk_cut_source(
                        key,
                        video_source_from_id(settings.video_source.unwrap_or(0)),
                    )
                    .await
            }
            DskKind::Fill => {
                controller
                    .set_dsk_fill_source(
                        key,
                        video_source_from_id(settings.video_source.unwrap_or(0)),
                    )
                    .await
            }
        };
        if let Err(error) = result {
            self.runtime
                .log(&format!("ATEM DSK action failed (key={key}): {error}"))
                .await;
            return;
        }
        if kind == DskKind::Auto {
            self.runtime.set_image(context, IMAGE_RED).await;
        } else if kind == DskKind::OnAir {
            let state = controller.get_state().await;
            if let Some(ip) = normalized_ip(settings) {
                self.runtime.refresh_dsk_on_air(&ip, &state).await;
            }
        }
    }

    async fn controller_for(
        &self,
        context: &str,
        settings: &ActionSettings,
        alert: bool,
    ) -> Option<Arc<AtemController>> {
        let Some(ip) = normalized_ip(settings) else {
            self.runtime
                .log("ATEM action ignored: IP address is empty")
                .await;
            if alert {
                self.runtime.show_alert(context).await;
            }
            return None;
        };
        match self.runtime.connect(&ip).await {
            Ok(controller) => Some(controller),
            Err(error) => {
                self.runtime
                    .log(&format!("failed to connect to ATEM {ip}: {error}"))
                    .await;
                if alert {
                    self.runtime.show_alert(context).await;
                }
                None
            }
        }
    }

    async fn push_inspector(&self, action: &str, context: &str, settings: &ActionSettings) {
        let connected = self
            .runtime
            .is_connected(normalized_ip(settings).as_deref())
            .await;
        self.runtime
            .send_pi(context, &AtemStateMessage::new(connected))
            .await;
        for kind in option_kinds(action) {
            let items = self.runtime.options(kind, settings).await;
            self.runtime
                .send_pi(context, &OptionsMessage::new(*kind, items, connected))
                .await;
        }
    }
}

/// CUT / AUTO / CUT TO BLACK / FADE TO BLACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    Cut,
    Auto,
    CutBlack,
    FadeToBlack,
}

/// Downstream keyer operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DskKind {
    Auto,
    OnAir,
    Cut,
    Fill,
}

impl Runtime {
    async fn upsert_key(&self, action: &str, context: &str, settings: ActionSettings) {
        self.keys.write().await.insert(
            context.to_string(),
            KeyRecord {
                action: action.to_string(),
                settings,
            },
        );
    }

    async fn ensure_connected(&self, settings: &ActionSettings) {
        let Some(ip) = normalized_ip(settings) else {
            return;
        };
        if let Err(error) = self.connect(&ip).await {
            self.log(&format!("failed to connect to ATEM {ip}: {error}"))
                .await;
        }
    }

    async fn connect(&self, ip: &str) -> std::result::Result<Arc<AtemController>, String> {
        if let Some(live) = self.connections.read().await.get(ip) {
            return Ok(Arc::clone(&live.controller));
        }
        let address =
            Ipv4Addr::from_str(ip).map_err(|error| format!("invalid ATEM IP {ip:?}: {error}"))?;
        let socket = SocketAddrV4::new(address, ATEM_PORT);
        self.log(&format!("connecting to ATEM at {socket}")).await;
        let controller = AtemController::connect_udp(socket, true)
            .await
            .map_err(|error| error.to_string())?;
        let controller = Arc::new(controller);
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        {
            let mut guard = self.connections.write().await;
            if let Some(live) = guard.get(ip) {
                return Ok(Arc::clone(&live.controller));
            }
            guard.insert(
                ip.to_string(),
                LiveAtem {
                    controller: Arc::clone(&controller),
                    _stop: stop_tx,
                },
            );
        }
        let runtime = self
            .this
            .upgrade()
            .ok_or_else(|| "ATEM runtime has been dropped".to_string())?;
        let ip_owned = ip.to_string();
        let forwarded = Arc::clone(&controller);
        tokio::spawn(async move {
            runtime.forward(ip_owned, forwarded, stop_rx).await;
        });
        Ok(controller)
    }

    async fn forward(
        &self,
        ip: String,
        controller: Arc<AtemController>,
        mut stop_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let mut updates = controller.state_update_events();
        loop {
            tokio::select! {
                biased;
                _ = stop_rx.recv() => break,
                update = updates.recv() => {
                    let Ok((_txn, updated)) = update else {
                        self.on_disconnected(&ip, &controller).await;
                        break;
                    };
                    self.on_state(&ip, &controller, updated).await;
                }
            }
        }
    }

    async fn on_state(&self, ip: &str, controller: &AtemController, updated: StateUpdate) {
        self.notify_connected(ip, true).await;
        if updated.intersects(StateUpdate::PREVIEW_OR_PROGRAM_SOURCE) {
            let state = controller.get_state().await;
            if updated.intersects(StateUpdate::PROGRAM_SOURCE) {
                self.refresh_program_preview(ip, &state, true).await;
            }
            if updated.intersects(StateUpdate::PREVIEW_SOURCE) {
                self.refresh_program_preview(ip, &state, false).await;
            }
        }
        if updated.intersects(StateUpdate::TRANSITION_POSITION) {
            let state = controller.get_state().await;
            self.refresh_auto(ip, &state).await;
        }
        if updated.intersects(StateUpdate::DSK_STATE) {
            let state = controller.get_state().await;
            self.refresh_dsk_on_air(ip, &state).await;
            self.refresh_dsk_auto(ip, &state).await;
        }
    }

    async fn on_disconnected(&self, ip: &str, controller: &Arc<AtemController>) {
        let mut guard = self.connections.write().await;
        if guard
            .get(ip)
            .is_some_and(|live| Arc::ptr_eq(&live.controller, controller))
        {
            guard.remove(ip);
        }
        drop(guard);
        self.log(&format!("ATEM connection closed for {ip}")).await;
        self.notify_connected(ip, false).await;
    }

    async fn notify_connected(&self, ip: &str, connected: bool) {
        let contexts: Vec<String> = self
            .inspectors
            .read()
            .await
            .iter()
            .filter(|(_, watched)| watched.as_deref() == Some(ip))
            .map(|(context, _)| context.clone())
            .collect();
        let message = AtemStateMessage::new(connected);
        for context in contexts {
            self.send_pi(&context, &message).await;
        }
    }

    async fn refresh_ip(&self, ip: &str) {
        let Some(controller) = self
            .connections
            .read()
            .await
            .get(ip)
            .map(|live| Arc::clone(&live.controller))
        else {
            return;
        };
        let state = controller.get_state().await;
        self.refresh_program_preview(ip, &state, true).await;
        self.refresh_program_preview(ip, &state, false).await;
        self.refresh_auto(ip, &state).await;
        self.refresh_dsk_on_air(ip, &state).await;
        self.refresh_dsk_auto(ip, &state).await;
    }

    async fn refresh_program_preview(&self, ip: &str, state: &AtemState, program: bool) {
        let action = if program {
            "dev.miseinul.atem.programinput"
        } else {
            "dev.miseinul.atem.previewinput"
        };
        for (context, settings) in self.keys_for(action, ip).await {
            let configured = video_source_from_id(settings.video_source.unwrap_or(0));
            let me = settings.me.unwrap_or(0);
            let current = if program {
                state.get_program_source(me)
            } else {
                state.get_preview_source(me)
            };
            let active = current.is_some_and(|source| source == configured);
            let image = match (program, active) {
                (true, true) => IMAGE_RED,
                (false, true) => IMAGE_GREEN,
                _ => IMAGE_GRAY,
            };
            self.set_image(&context, image).await;
        }
    }

    async fn refresh_auto(&self, ip: &str, state: &AtemState) {
        for (context, settings) in self.keys_for("dev.miseinul.atem.auto", ip).await {
            let me = settings.me.unwrap_or(0);
            let in_progress = state
                .transition_position
                .get(&me)
                .is_some_and(|position| position.in_progress);
            self.set_image(&context, if in_progress { IMAGE_RED } else { IMAGE_GRAY })
                .await;
        }
    }

    async fn refresh_dsk_on_air(&self, ip: &str, state: &AtemState) {
        for (context, settings) in self.keys_for("dev.miseinul.atem.dskonair", ip).await {
            let key = settings.key.unwrap_or(0);
            let on_air = state.dsk_state.get(&key).is_some_and(|dsk| dsk.on_air);
            self.set_image(&context, if on_air { IMAGE_RED } else { IMAGE_GRAY })
                .await;
        }
    }

    async fn refresh_dsk_auto(&self, ip: &str, state: &AtemState) {
        for (context, settings) in self.keys_for("dev.miseinul.atem.dskauto", ip).await {
            let key = settings.key.unwrap_or(0);
            let in_transition = state
                .dsk_state
                .get(&key)
                .is_some_and(|dsk| dsk.in_transition);
            self.set_image(&context, if in_transition { IMAGE_RED } else { IMAGE_GRAY })
                .await;
        }
    }

    async fn keys_for(&self, action: &str, ip: &str) -> Vec<(String, ActionSettings)> {
        self.keys
            .read()
            .await
            .iter()
            .filter(|(_, record)| {
                record.action == action && normalized_ip(&record.settings).as_deref() == Some(ip)
            })
            .map(|(context, record)| (context.clone(), record.settings.clone()))
            .collect()
    }

    async fn evict_unused(&self) {
        let used: HashSet<String> = self
            .keys
            .read()
            .await
            .values()
            .filter_map(|record| normalized_ip(&record.settings))
            .collect();
        let removed = {
            let mut guard = self.connections.write().await;
            let stale: Vec<String> = guard
                .keys()
                .filter(|ip| !used.contains(*ip))
                .cloned()
                .collect();
            for ip in &stale {
                guard.remove(ip);
            }
            stale
        };
        for ip in removed {
            self.notify_connected(&ip, false).await;
        }
    }

    async fn is_connected(&self, ip: Option<&str>) -> bool {
        let Some(ip) = ip else {
            return false;
        };
        self.connections.read().await.contains_key(ip)
    }

    async fn options(&self, kind: &str, settings: &ActionSettings) -> Vec<SelectItem> {
        let controller = match normalized_ip(settings) {
            Some(ip) => self
                .connections
                .read()
                .await
                .get(&ip)
                .map(|live| Arc::clone(&live.controller)),
            None => None,
        };
        let state;
        let state_ref = if let Some(controller) = &controller {
            state = controller.get_state().await;
            Some(&*state)
        } else {
            None
        };
        match kind {
            "me" => me_items(state_ref),
            "video" => video_items(state_ref),
            "aux" => aux_items(state_ref),
            "auxVideo" => aux_video_items(state_ref),
            "dsk" => dsk_items(state_ref),
            _ => Vec::new(),
        }
    }

    async fn set_image(&self, context: &str, image: &str) {
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.set_image(context, Some(image), Target::HardwareAndSoftware, None);
        }
    }

    async fn show_alert(&self, context: &str) {
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.show_alert(context);
        }
    }

    async fn send_pi(&self, context: &str, payload: &impl serde::Serialize) {
        let Ok(value) = serde_json::to_value(payload) else {
            return;
        };
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send_to_property_inspector(context, &value);
        }
    }

    async fn log(&self, message: &str) {
        tracing::info!("{message}");
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.log_message(message);
        }
    }
}

fn option_kinds(action: &str) -> &'static [&'static str] {
    match action {
        "dev.miseinul.atem.cut"
        | "dev.miseinul.atem.auto"
        | "dev.miseinul.atem.cuttoblack"
        | "dev.miseinul.atem.fadetoblack" => &["me"],
        "dev.miseinul.atem.programinput" | "dev.miseinul.atem.previewinput" => &["me", "video"],
        "dev.miseinul.atem.auxoutput" => &["aux", "auxVideo"],
        "dev.miseinul.atem.dskauto" | "dev.miseinul.atem.dskonair" => &["dsk"],
        "dev.miseinul.atem.dskcut" | "dev.miseinul.atem.dskfill" => &["dsk", "video"],
        _ => &[],
    }
}

fn video_source_from_id(id: u16) -> VideoSource {
    VideoSource::from_u16(id).unwrap_or(VideoSource::Unknown)
}

fn me_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let count = state
        .map(|state| state.get_program_sources().len())
        .filter(|count| *count > 0)
        .unwrap_or(4);
    (0..count as u8)
        .map(|me| SelectItem {
            label: format!("M/E {me}"),
            value: me.to_string(),
        })
        .collect()
}

fn video_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let mut ids = input_ids(state);
    if ids.is_empty() {
        ids = (1..=20).collect();
    }
    ids.into_iter()
        .map(|id| SelectItem {
            label: format!("Input {id}"),
            value: id.to_string(),
        })
        .collect()
}

fn aux_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let count = state
        .map(|state| state.topology.auxs)
        .filter(|count| *count > 0)
        .unwrap_or(4);
    (0..count)
        .map(|bus| SelectItem {
            label: format!("AUX {bus}"),
            value: bus.to_string(),
        })
        .collect()
}

fn aux_video_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let mut items = video_items(state);
    let me_count = state.map(|state| state.topology.mes as usize).unwrap_or(2);
    for (value, label) in ME_PROGRAM_PREVIEW.iter().take(me_count.saturating_mul(2)) {
        items.push(SelectItem {
            label: (*label).to_string(),
            value: value.to_string(),
        });
    }
    if items.is_empty() {
        items = video_items(None);
    }
    items
}

fn dsk_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let mut keys: Vec<u8> = state
        .map(|state| state.dsk_state.keys().copied().collect())
        .unwrap_or_default();
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        keys = vec![0, 1];
    }
    keys.into_iter()
        .map(|key| SelectItem {
            label: format!("DSK {key}"),
            value: key.to_string(),
        })
        .collect()
}

fn input_ids(state: Option<&AtemState>) -> Vec<u16> {
    let Some(state) = state else {
        return Vec::new();
    };
    let mut ids: Vec<u16> = state
        .input_properties
        .keys()
        .filter_map(|source| {
            let id = *source as u16;
            (1..=40).contains(&id).then_some(id)
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn aux_source_ids(state: &AtemState) -> Vec<u16> {
    let mut ids = input_ids(Some(state));
    let me_count = state.topology.mes as usize;
    ids.extend(
        ME_PROGRAM_PREVIEW
            .iter()
            .take(me_count.saturating_mul(2))
            .map(|(id, _)| *id),
    );
    if ids.is_empty() {
        ids = (1..=20).collect();
        ids.extend([10010, 10011, 10020, 10021]);
    }
    ids
}
