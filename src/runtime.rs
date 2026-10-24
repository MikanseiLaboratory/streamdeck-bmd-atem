//! Shared ATEM connections, key tallies, and Property Inspector updates.
//!
//! [`AtemPool`] keeps one supervisor per switcher in the global-settings catalog.
//! Status and state events from that pool drive key images and any open inspector.
//! A key selects its switcher with `atem_ip`. Closing the key does not disconnect it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use necromancer::protocol::atom::InputProperties;
use necromancer::protocol::structs::{PortType, VideoSource};

use crate::availability::{MeAvailability, SourceAvailability, StateAvailability};
use necromancer::{AtemController, AtemState, StateUpdate};
use num_traits::FromPrimitive;
use serde_json::Value;
use streamdeck_plugin::{
    async_trait, CommandSender, PluginLifecycle, PluginService, Result, Target,
};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::contracts::{
    cycle_index, merge_catalog, normalized_ip, resolve_targets, ActionSettings, AtemStateMessage,
    ConnectionState, GlobalSettings, InspectorCommand, InstanceConfig, InstanceStatusMessage,
    OptionsMessage, SelectItem, StatusMessage,
};
use crate::pool::{AtemInstanceConfig, AtemPool, ConnectionStatus, PoolEvent, PoolOptions};

const IMAGE_RED: &str = "Images/red.png";
const IMAGE_GREEN: &str = "Images/green.png";
const IMAGE_GRAY: &str = "Images/gray.png";

/// Offline mix-effect count for an ATEM Mini, which has a single M/E.
/// A connected switcher uses `topology.mes` instead, and the value stays the protocol index.
const OFFLINE_ME_COUNT: u8 = 1;

/// Offline inputs for an ATEM Mini: four HDMI inputs.
const OFFLINE_HDMI_INPUTS: u16 = 4;

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

/// Plugin-wide state passed to every action.
#[derive(Clone)]
pub struct AppState {
    runtime: Arc<Runtime>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Runtime {
                pool: AtemPool::new(PoolOptions::default()),
                keys: RwLock::new(HashMap::new()),
                inspectors: RwLock::new(HashMap::new()),
                global: RwLock::new(GlobalSettings::default()),
                sender: Mutex::new(None),
                listener_started: AtomicBool::new(false),
            }),
        }
    }
}

struct Runtime {
    pool: AtemPool,
    keys: RwLock<HashMap<String, KeyRecord>>,
    /// Inspector contexts currently visible, mapped to the ATEM IP they watch.
    inspectors: RwLock<HashMap<String, Option<String>>>,
    global: RwLock<GlobalSettings>,
    sender: Mutex<Option<CommandSender>>,
    listener_started: AtomicBool,
}

#[async_trait]
impl PluginService for AppState {
    async fn start(&self, sender: CommandSender) -> Result<()> {
        *self.runtime.sender.lock().await = Some(sender.clone());
        self.runtime.spawn_listener();
        let _ = sender.get_global_settings(None);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.runtime.pool.shutdown().await;
        Ok(())
    }
}

impl AppState {
    pub async fn on_will_appear(&self, action: &str, context: &str, settings: &ActionSettings) {
        self.runtime
            .upsert_key(action, context, settings.clone())
            .await;
        self.runtime.ensure_legacy_instance(settings).await;
        self.runtime.refresh_settings(settings).await;
    }

    pub async fn on_will_disappear(&self, context: &str) {
        self.runtime.keys.write().await.remove(context);
        self.runtime.inspectors.write().await.remove(context);
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
        self.runtime.ensure_legacy_instance(settings).await;
        self.runtime.refresh_settings(settings).await;
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
        self.runtime.ensure_legacy_instance(settings).await;
        self.push_inspector(action, context, settings).await;
        self.runtime.push_catalog_status().await;
    }

    pub async fn on_property_inspector_did_disappear(&self, context: &str) {
        self.runtime.inspectors.write().await.remove(context);
    }

    /// Apply `didReceiveGlobalSettings` and reconcile the connection catalog.
    pub async fn apply_global_value(&self, value: &Value) {
        self.runtime.apply_global_value(value).await;
    }

    pub async fn on_property_inspector_message(&self, context: &str, payload: &Value) {
        let command =
            serde_json::from_value::<InspectorCommand>(payload.clone()).unwrap_or_default();
        match command.r#type.as_str() {
            "ready" | "refresh" => {
                self.runtime
                    .inspectors
                    .write()
                    .await
                    .entry(context.to_string())
                    .or_insert(None);
                self.runtime.push_catalog_status().await;
                return;
            }
            "reconnect" => {
                self.runtime.reconnect(command.id.as_deref()).await;
                return;
            }
            "requestOptions" => {}
            _ => return,
        }
        let (action, mut settings) = self
            .runtime
            .keys
            .read()
            .await
            .get(context)
            .map(|record| (record.action.clone(), record.settings.clone()))
            .unwrap_or_else(|| (String::new(), ActionSettings::default()));
        if let Some(ip) = command
            .atem_ip
            .as_ref()
            .map(|ip| ip.trim().to_string())
            .filter(|ip| !ip.is_empty())
        {
            settings.atem_ip = Some(ip);
        }
        if command.target.is_some() {
            settings.target = command.target.clone();
        }
        if command.me.is_some() {
            settings.me = command.me;
        }
        if command.aux_bus.is_some() {
            settings.aux_bus = command.aux_bus;
        }
        if self.runtime.inspectors.read().await.contains_key(context) {
            self.runtime
                .inspectors
                .write()
                .await
                .insert(context.to_string(), normalized_ip(&settings));
        }
        self.runtime.ensure_legacy_instance(&settings).await;
        let (connected, detail) = self.runtime.connection_view(&settings).await;
        let items = self
            .runtime
            .options(&action, &command.kind, &settings)
            .await;
        self.runtime
            .send_pi(
                context,
                &OptionsMessage::new(command.kind, items, connected, detail),
            )
            .await;
    }

    pub async fn press_transition(
        &self,
        context: &str,
        settings: &ActionSettings,
        kind: TransitionKind,
    ) {
        let targets = self.targets_for(context, settings, false).await;
        if targets.is_empty() {
            return;
        }
        let me = settings.me.unwrap_or(0);
        let mut failed = false;
        for target in &targets {
            let result = match kind {
                TransitionKind::Cut => target.controller.cut(me).await,
                TransitionKind::Auto => target.controller.auto(me).await,
                TransitionKind::CutBlack => target.controller.cut_black(me, true).await,
                TransitionKind::FadeToBlack => target.controller.toggle_auto_black(me).await,
            };
            if let Err(error) = result {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM transition failed on {} (me={me}): {error}",
                        target.label
                    ))
                    .await;
            }
        }
        if failed {
            self.runtime.show_alert(context).await;
        } else if kind == TransitionKind::Auto {
            self.runtime.set_image(context, IMAGE_RED).await;
        }
    }

    pub async fn press_bus(&self, context: &str, settings: &ActionSettings, program: bool) {
        let targets = self.targets_for(context, settings, false).await;
        if targets.is_empty() {
            return;
        }
        let me = settings.me.unwrap_or(0);
        let source = video_source_from_id(settings.video_source.unwrap_or(0));
        let mut failed = false;
        for target in &targets {
            let result = if program {
                target.controller.set_program_input(me, source).await
            } else {
                target.controller.set_preview_input(me, source).await
            };
            if let Err(error) = result {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM bus change failed on {} (me={me}): {error}",
                        target.label
                    ))
                    .await;
                continue;
            }
            let state = target.controller.get_state().await;
            self.runtime
                .refresh_program_preview(&target.host, &state, true)
                .await;
            self.runtime
                .refresh_program_preview(&target.host, &state, false)
                .await;
        }
        if failed {
            self.runtime.show_alert(context).await;
        }
    }

    pub async fn press_aux(&self, context: &str, settings: &ActionSettings) {
        let targets = self.targets_for(context, settings, true).await;
        let aux_bus = settings.aux_bus.unwrap_or(0);
        let source = video_source_from_id(settings.video_source.unwrap_or(0));
        let mut failed = false;
        for target in &targets {
            if let Err(error) = target.controller.set_aux_source(aux_bus, source).await {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM AUX change failed on {} (aux_bus={aux_bus}, source={source:?}): {error}",
                        target.label
                    ))
                    .await;
            }
        }
        if failed {
            self.runtime.show_alert(context).await;
        }
    }

    /// Cycle the AUX source and return the settings that should be persisted.
    pub async fn rotate_aux(
        &self,
        context: &str,
        settings: &ActionSettings,
        ticks: i64,
    ) -> Option<ActionSettings> {
        let targets = self.targets_for(context, settings, true).await;
        let first = targets.first()?;
        let state = first.controller.get_state().await;
        let aux_bus = settings.aux_bus.unwrap_or(0);
        let source_ids = aux_source_ids(&state, aux_bus);
        if source_ids.is_empty() {
            return None;
        }
        let current = settings.video_source.unwrap_or(1);
        let index = source_ids.iter().position(|id| *id == current).unwrap_or(0);
        let new_id = source_ids[cycle_index(source_ids.len(), index, ticks)];
        let source = video_source_from_id(new_id);
        let mut failed = false;
        for target in &targets {
            if let Err(error) = target.controller.set_aux_source(aux_bus, source).await {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM AUX dial failed on {} (aux_bus={aux_bus}, source_id={new_id}): {error}",
                        target.label
                    ))
                    .await;
            }
        }
        if failed {
            self.runtime.show_alert(context).await;
        }
        let mut updated = settings.clone();
        updated.video_source = Some(new_id);
        if let Some(record) = self.runtime.keys.write().await.get_mut(context) {
            record.settings = updated.clone();
        }
        Some(updated)
    }

    pub async fn press_dsk(&self, context: &str, settings: &ActionSettings, kind: DskKind) {
        let targets = self.targets_for(context, settings, true).await;
        if targets.is_empty() {
            return;
        }
        let key = settings.key.unwrap_or(0);
        let mut failed = false;
        for target in &targets {
            let result = match kind {
                DskKind::Auto => target.controller.dsk_auto(key).await,
                DskKind::OnAir => {
                    let on_air = match settings.on_air {
                        Some(on_air) => on_air,
                        None => {
                            let state = target.controller.get_state().await;
                            let current = state
                                .dsk_state
                                .get(&key)
                                .map(|dsk| dsk.on_air)
                                .unwrap_or(false);
                            !current
                        }
                    };
                    target.controller.set_dsk_on_air(key, on_air).await
                }
                DskKind::Cut => {
                    target
                        .controller
                        .set_dsk_cut_source(
                            key,
                            video_source_from_id(settings.video_source.unwrap_or(0)),
                        )
                        .await
                }
                DskKind::Fill => {
                    target
                        .controller
                        .set_dsk_fill_source(
                            key,
                            video_source_from_id(settings.video_source.unwrap_or(0)),
                        )
                        .await
                }
            };
            if let Err(error) = result {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM DSK action failed on {} (key={key}): {error}",
                        target.label
                    ))
                    .await;
                continue;
            }
            if kind == DskKind::OnAir {
                let state = target.controller.get_state().await;
                self.runtime.refresh_dsk_on_air(&target.host, &state).await;
            }
        }
        if failed {
            self.runtime.show_alert(context).await;
        } else if kind == DskKind::Auto {
            self.runtime.set_image(context, IMAGE_RED).await;
        }
    }

    async fn targets_for(
        &self,
        context: &str,
        settings: &ActionSettings,
        alert: bool,
    ) -> Vec<ReadyTarget> {
        self.runtime.ensure_legacy_instance(settings).await;
        let global = self.runtime.global.read().await.clone();
        let ids = resolve_targets(&global.instances, &global.groups, settings);
        if ids.is_empty() {
            self.runtime
                .log("ATEM action ignored: no switcher selected")
                .await;
            if alert {
                self.runtime.show_alert(context).await;
            }
            return Vec::new();
        }
        let mut ready = Vec::new();
        let mut missed = Vec::new();
        for id in ids {
            let Some(instance) = global.instances.iter().find(|item| item.id == id) else {
                continue;
            };
            if let Some(controller) = self.runtime.pool.controller(&id).await {
                ready.push(ReadyTarget {
                    host: instance.host.clone(),
                    label: instance.name.clone(),
                    controller,
                });
            } else {
                missed.push(instance.name.clone());
            }
        }
        if ready.is_empty() {
            self.runtime
                .log(&format!(
                    "ATEM action ignored: {} not connected",
                    missed.join(", ")
                ))
                .await;
            if alert {
                self.runtime.show_alert(context).await;
            }
        } else if !missed.is_empty() {
            self.runtime
                .log(&format!(
                    "ATEM action skipped disconnected switchers: {}",
                    missed.join(", ")
                ))
                .await;
        }
        ready
    }

    async fn push_inspector(&self, action: &str, context: &str, settings: &ActionSettings) {
        let (connected, detail) = self.runtime.connection_view(settings).await;
        self.runtime
            .send_pi(context, &AtemStateMessage::new(connected, &detail))
            .await;
        self.runtime.push_catalog_status().await;
        for kind in option_kinds(action) {
            let items = self.runtime.options(action, kind, settings).await;
            self.runtime
                .send_pi(
                    context,
                    &OptionsMessage::new(*kind, items, connected, &detail),
                )
                .await;
        }
    }
}

struct ReadyTarget {
    host: String,
    label: String,
    controller: Arc<AtemController>,
}

/// Receives `didReceiveGlobalSettings`.
pub struct GlobalHook {
    /// Plugin state shared with the actions.
    pub state: AppState,
}

#[async_trait]
impl PluginLifecycle for GlobalHook {
    async fn on_did_receive_global_settings(&mut self, settings: &Value) -> Result<()> {
        self.state.apply_global_value(settings).await;
        Ok(())
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

    fn spawn_listener(self: &Arc<Self>) {
        if self.listener_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let runtime = Arc::clone(self);
        let mut events = self.pool.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(PoolEvent::Status { id }) => runtime.on_pool_status(&id).await,
                    Ok(PoolEvent::State { id, updated }) => {
                        let Some(config) = runtime.pool.config(&id).await else {
                            continue;
                        };
                        let Some(controller) = runtime.pool.controller(&id).await else {
                            continue;
                        };
                        runtime.on_state(&config.host, &controller, updated).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        runtime.refresh_connected().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    async fn apply_global_value(&self, value: &Value) {
        let incoming = serde_json::from_value::<GlobalSettings>(value.clone()).unwrap_or_default();
        let mut global = self.global.write().await;
        let (settings, changed) = merge_catalog(incoming, &global);
        *global = settings.clone();
        drop(global);
        if changed {
            self.persist_global(&settings).await;
        }
        self.apply_catalog(&settings).await;
    }

    async fn ensure_legacy_instance(&self, settings: &ActionSettings) {
        if settings.target.is_some() {
            return;
        }
        let Some(host) = normalized_ip(settings) else {
            return;
        };
        self.ensure_instance(&host).await;
    }

    async fn ensure_instance(&self, host: &str) {
        let host = host.trim();
        if host.is_empty() {
            return;
        }
        let mut global = self.global.write().await;
        if global.instances.iter().any(|item| item.host.trim() == host) {
            return;
        }
        global.instances.push(InstanceConfig::for_host(host));
        global.revision = global.revision.saturating_add(1);
        let settings = global.clone();
        drop(global);
        self.persist_global(&settings).await;
        self.apply_catalog(&settings).await;
    }

    async fn reconnect(&self, id: Option<&str>) {
        if let Some(id) = id {
            self.pool.reconnect(id).await;
            return;
        }
        let ids: Vec<String> = self
            .global
            .read()
            .await
            .instances
            .iter()
            .map(|instance| instance.id.clone())
            .collect();
        for id in ids {
            self.pool.reconnect(&id).await;
        }
    }

    async fn persist_global(&self, settings: &GlobalSettings) {
        let Ok(payload) = serde_json::to_value(settings) else {
            return;
        };
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.set_global_settings(&payload);
        }
    }

    async fn apply_catalog(&self, settings: &GlobalSettings) {
        let configs: Vec<AtemInstanceConfig> = settings
            .instances
            .iter()
            .cloned()
            .map(|instance| AtemInstanceConfig {
                id: instance.id,
                name: instance.name,
                host: instance.host,
                port: instance.port,
                enabled: instance.enabled,
            })
            .collect();
        let removed = self.pool.reconcile(configs).await;
        for config in removed {
            if self.pool.status_for_host(&config.host).await.is_none() {
                self.notify_ip(&config.host, false, "Disconnected").await;
            }
        }
        self.push_catalog_status().await;
        for instance in &settings.instances {
            if self
                .pool
                .status(&instance.id)
                .await
                .is_some_and(|status| status.is_connected())
            {
                self.refresh_ip(&instance.host).await;
                self.push_options_for_ip(&instance.host, "").await;
            }
        }
    }

    async fn on_pool_status(&self, id: &str) {
        let Some(config) = self.pool.config(id).await else {
            return;
        };
        let Some(status) = self.pool.status(id).await else {
            return;
        };
        let connected = status.is_connected();
        let detail = status.detail();
        let target = if config.port == crate::pool::ATEM_PORT {
            config.host.clone()
        } else {
            format!("{}:{}", config.host, config.port)
        };
        match &status {
            ConnectionStatus::Disabled => {
                self.log(&format!("ATEM {target} disabled")).await;
            }
            ConnectionStatus::Connected => {
                self.log(&format!("ATEM {target} connected")).await;
            }
            ConnectionStatus::Connecting => {
                self.log(&format!("ATEM {target} connecting")).await;
            }
            ConnectionStatus::Disconnected { message } => {
                self.log(&format!("ATEM {target} disconnected: {message}"))
                    .await;
            }
        }
        self.notify_ip(&config.host, connected, &detail).await;
        if connected {
            self.refresh_ip(&config.host).await;
            self.push_options_for_ip(&config.host, &detail).await;
        }
        self.push_catalog_status().await;
    }

    async fn push_catalog_status(&self) {
        let global = self.global.read().await.clone();
        let instances = self
            .pool
            .statuses()
            .await
            .into_iter()
            .map(|instance| {
                let color = global
                    .instances
                    .iter()
                    .find(|item| item.id == instance.id)
                    .map(|item| item.color.clone())
                    .unwrap_or_else(|| "#4c8dff".to_string());
                InstanceStatusMessage {
                    id: instance.id,
                    name: instance.name,
                    host: instance.host,
                    port: instance.port,
                    color,
                    enabled: instance.enabled,
                    status: wire_state(&instance.status),
                }
            })
            .collect();
        let message = StatusMessage::new(instances);
        let contexts: Vec<String> = self.inspectors.read().await.keys().cloned().collect();
        for context in contexts {
            self.send_pi(&context, &message).await;
        }
    }

    async fn on_state(&self, ip: &str, controller: &AtemController, updated: StateUpdate) {
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
        if updated.intersects(StateUpdate::INPUT_PROPERTIES) {
            self.push_options_for_ip(ip, "").await;
        }
    }

    async fn notify_ip(&self, ip: &str, connected: bool, detail: &str) {
        let contexts: Vec<String> = self
            .inspectors
            .read()
            .await
            .iter()
            .filter(|(_, watched)| watched.as_deref() == Some(ip))
            .map(|(context, _)| context.clone())
            .collect();
        let message = AtemStateMessage::new(connected, detail);
        for context in contexts {
            self.send_pi(&context, &message).await;
        }
    }

    async fn push_options_for_ip(&self, ip: &str, detail: &str) {
        let open: Vec<(String, String, ActionSettings)> = {
            let keys = self.keys.read().await;
            let inspectors = self.inspectors.read().await;
            inspectors
                .iter()
                .filter(|(_, watched)| watched.as_deref() == Some(ip))
                .filter_map(|(context, _)| {
                    let record = keys.get(context)?;
                    Some((
                        context.clone(),
                        record.action.clone(),
                        record.settings.clone(),
                    ))
                })
                .collect()
        };
        for (context, action, settings) in open {
            for kind in option_kinds(&action) {
                let items = self.options(&action, kind, &settings).await;
                self.send_pi(&context, &OptionsMessage::new(*kind, items, true, detail))
                    .await;
            }
        }
    }

    async fn connection_view(&self, settings: &ActionSettings) -> (bool, String) {
        let global = self.global.read().await.clone();
        let ids = resolve_targets(&global.instances, &global.groups, settings);
        if ids.is_empty() {
            return (false, "No switcher selected".to_string());
        }
        let mut connected = false;
        let mut detail = String::new();
        for id in ids {
            match self.pool.status(&id).await {
                Some(status) if status.is_connected() => connected = true,
                Some(status) if detail.is_empty() => detail = status.detail(),
                None if detail.is_empty() => detail = "Not connected".to_string(),
                _ => {}
            }
        }
        if connected {
            (true, String::new())
        } else {
            (false, detail)
        }
    }

    async fn refresh_settings(&self, settings: &ActionSettings) {
        let global = self.global.read().await.clone();
        let ids = resolve_targets(&global.instances, &global.groups, settings);
        for id in ids {
            let Some(instance) = global.instances.iter().find(|item| item.id == id) else {
                continue;
            };
            self.refresh_ip(&instance.host).await;
        }
    }

    async fn refresh_connected(&self) {
        for instance in self.pool.statuses().await {
            if instance.status.is_connected() {
                self.refresh_ip(&instance.host).await;
            }
        }
    }

    async fn refresh_ip(&self, ip: &str) {
        let Some(controller) = self.pool.controller_for_host(ip).await else {
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

    async fn keys_for(&self, action: &str, host: &str) -> Vec<(String, ActionSettings)> {
        let records: Vec<(String, ActionSettings)> = self
            .keys
            .read()
            .await
            .iter()
            .filter(|(_, record)| record.action == action)
            .map(|(context, record)| (context.clone(), record.settings.clone()))
            .collect();
        let global = self.global.read().await.clone();
        records
            .into_iter()
            .filter(|(_, settings)| {
                resolve_targets(&global.instances, &global.groups, settings)
                    .iter()
                    .any(|id| {
                        global
                            .instances
                            .iter()
                            .any(|item| item.id == *id && item.host.trim() == host.trim())
                    })
            })
            .collect()
    }

    async fn options(
        &self,
        action: &str,
        kind: &str,
        settings: &ActionSettings,
    ) -> Vec<SelectItem> {
        let global = self.global.read().await.clone();
        let ids = resolve_targets(&global.instances, &global.groups, settings);
        let mut controller = None;
        for id in ids {
            if let Some(found) = self.pool.controller(&id).await {
                controller = Some(found);
                break;
            }
        }
        let state;
        let state_ref = if let Some(controller) = &controller {
            state = controller.get_state().await;
            Some(&*state)
        } else {
            None
        };
        match kind {
            "me" => me_items(state_ref),
            "video" if is_dsk_source_action(action) => dsk_source_items(state_ref),
            "video" => program_preview_items(state_ref, settings.me.unwrap_or(0)),
            "aux" => aux_items(state_ref),
            "auxVideo" => aux_video_items(state_ref, settings.aux_bus.unwrap_or(0)),
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

fn wire_state(status: &ConnectionStatus) -> ConnectionState {
    match status {
        ConnectionStatus::Disabled => ConnectionState::Disabled,
        ConnectionStatus::Connecting => ConnectionState::Connecting,
        ConnectionStatus::Connected => ConnectionState::Connected,
        ConnectionStatus::Disconnected { message } => ConnectionState::Disconnected {
            message: message.clone(),
        },
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
        .map(|state| state.topology.mes)
        .filter(|count| *count > 0)
        .unwrap_or(OFFLINE_ME_COUNT);
    (0..count)
        .map(|me| SelectItem {
            label: format!("M/E {}", me + 1),
            value: me.to_string(),
        })
        .collect()
}

/// Which bus a connected source list is being built for.
#[derive(Clone, Copy)]
enum SourceQuery {
    /// Program or preview on one mix effect. `me` is the protocol index (0 = M/E 1).
    MixEffect(u8),
    /// Downstream keyer fill or cut.
    KeySource,
    /// One auxiliary bus. `bus` is the protocol index (0 = the first aux).
    Auxiliary(u8),
}

fn is_dsk_source_action(action: &str) -> bool {
    matches!(
        action,
        "dev.miseinul.atem.dskcut" | "dev.miseinul.atem.dskfill"
    )
}

fn program_preview_items(state: Option<&AtemState>, me: u8) -> Vec<SelectItem> {
    routed_items(state, SourceQuery::MixEffect(me), fallback_video_items)
}

fn dsk_source_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    routed_items(state, SourceQuery::KeySource, fallback_video_items)
}

fn routed_items(
    state: Option<&AtemState>,
    query: SourceQuery,
    fallback: fn() -> Vec<SelectItem>,
) -> Vec<SelectItem> {
    let Some(state) = state.filter(|state| !state.input_properties.is_empty()) else {
        return fallback();
    };
    listed_sources(&state.input_properties, |source, properties| {
        let Some(source_bits) = state.get_source_availability(*source) else {
            return false;
        };
        let Some(me_bits) = state.get_me_availability(*source) else {
            return false;
        };
        include_source(properties.port_type, source_bits, me_bits, query)
    })
}

fn listed_sources<'a>(
    properties: impl IntoIterator<Item = (&'a VideoSource, &'a InputProperties)>,
    include: impl Fn(&VideoSource, &InputProperties) -> bool,
) -> Vec<SelectItem> {
    let mut rows: Vec<(u16, String)> = properties
        .into_iter()
        .filter(|(source, properties)| include(source, properties))
        .map(|(source, properties)| {
            let id = *source as u16;
            (id, source_label(id, properties))
        })
        .collect();
    rows.sort_by_key(|(id, _)| source_rank(*id));
    rows.into_iter()
        .map(|(id, label)| SelectItem {
            label,
            value: id.to_string(),
        })
        .collect()
}

fn include_source(
    port: PortType,
    source_bits: SourceAvailability,
    me_bits: MeAvailability,
    query: SourceQuery,
) -> bool {
    let port_ok = match query {
        SourceQuery::Auxiliary(_) => aux_routable_port(port),
        SourceQuery::MixEffect(_) | SourceQuery::KeySource => selectable_port(port),
    };
    if !port_ok {
        return false;
    }
    match query {
        SourceQuery::MixEffect(me) => on_selected_me(me_bits, me),
        SourceQuery::KeySource => source_bits.key_source(),
        SourceQuery::Auxiliary(bus) => on_selected_aux(source_bits, bus),
    }
}

/// `me` is the 0-based mix-effect index stored on the action.
fn on_selected_me(availability: MeAvailability, me: u8) -> bool {
    match me {
        0 => availability.me1(),
        1 => availability.me2(),
        2 => availability.me3(),
        3 => availability.me4(),
        _ => false,
    }
}

/// Auxiliary routing from `SourceAvailability`.
///
/// The `auxiliary` flag means any aux bus. `auxiliary1` and `auxiliary2` are
/// per-output masks on some models (necromancer: Aux 1 / Aux 2, LibAtem
/// `Aux1Output` / `Aux2Output`). Those product names are 1-based, so they match
/// protocol bus 0 and bus 1.
fn on_selected_aux(availability: SourceAvailability, aux_bus: u8) -> bool {
    if availability.auxiliary() {
        return true;
    }
    match aux_bus {
        0 => availability.auxiliary1(),
        1 => availability.auxiliary2(),
        _ => false,
    }
}

/// Sources available on an ATEM Mini when no switcher state has arrived.
///
/// Values are necromancer `VideoSource` discriminants, which are the protocol IDs a
/// connected switcher reports in `input_properties`:
/// Black 0, HDMI inputs 1–4, colour bars 1000, colour generators 2001 and 2002,
/// media player 1 fill 3010 and its key output 3011.
/// The Mini input table exposes that key as its own routable source
/// (`PortType::MediaPlayerKey`). The Mini has no SuperSource; a connected
/// switcher that does have one still lists it from `input_properties`.
fn fallback_video_items() -> Vec<SelectItem> {
    let mut items = vec![
        SelectItem {
            label: "Black".into(),
            value: (VideoSource::Black as u16).to_string(),
        },
        SelectItem {
            label: "Color Bars".into(),
            value: (VideoSource::ColourBars as u16).to_string(),
        },
        SelectItem {
            label: "Color 1".into(),
            value: (VideoSource::Colour1 as u16).to_string(),
        },
        SelectItem {
            label: "Color 2".into(),
            value: (VideoSource::Colour2 as u16).to_string(),
        },
        SelectItem {
            label: "Media Player 1".into(),
            value: (VideoSource::MediaPlayer1 as u16).to_string(),
        },
        SelectItem {
            label: "Media Player 1 Key".into(),
            value: (VideoSource::MediaPlayer1Key as u16).to_string(),
        },
    ];
    items.extend((1..=OFFLINE_HDMI_INPUTS).map(|id| SelectItem {
        label: format!("Input {id}"),
        value: id.to_string(),
    }));
    items
}

/// Ports that can be placed on program, preview, or a downstream keyer.
///
/// A connected list also requires the matching availability bits. Auxiliary
/// menus add direct, mix-effect, and multiview outputs on top of this set.
fn selectable_port(port: PortType) -> bool {
    matches!(
        port,
        PortType::External
            | PortType::Black
            | PortType::ColourBars
            | PortType::ColourGenerator
            | PortType::MediaPlayerFill
            | PortType::MediaPlayerKey
            | PortType::SuperSource
    )
}

/// Ports an auxiliary bus can select, before the availability check.
///
/// Direct inputs, mix-effect outputs, and multiview outputs are aux sources on
/// the models that report them. Masks, aux outputs, and the audio monitor stay out.
fn aux_routable_port(port: PortType) -> bool {
    selectable_port(port)
        || matches!(
            port,
            PortType::ExternalDirect | PortType::MEOutput | PortType::Multiview
        )
}

fn source_label(id: u16, properties: &InputProperties) -> String {
    properties
        .get_long_name()
        .ok()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback_source_label(id))
}

fn fallback_source_label(id: u16) -> String {
    match VideoSource::from_u16(id) {
        Some(VideoSource::Black) => "Black".into(),
        Some(VideoSource::ColourBars) => "Color Bars".into(),
        Some(VideoSource::Colour1) => "Color 1".into(),
        Some(VideoSource::Colour2) => "Color 2".into(),
        Some(VideoSource::Colour3) => "Color 3".into(),
        Some(VideoSource::Colour4) => "Color 4".into(),
        Some(VideoSource::Colour5) => "Color 5".into(),
        Some(VideoSource::Colour6) => "Color 6".into(),
        Some(VideoSource::Colour7) => "Color 7".into(),
        Some(VideoSource::Colour8) => "Color 8".into(),
        Some(VideoSource::MediaPlayer1) => "Media Player 1".into(),
        Some(VideoSource::MediaPlayer1Key) => "Media Player 1 Key".into(),
        Some(VideoSource::MediaPlayer2) => "Media Player 2".into(),
        Some(VideoSource::MediaPlayer2Key) => "Media Player 2 Key".into(),
        Some(VideoSource::MediaPlayer3) => "Media Player 3".into(),
        Some(VideoSource::MediaPlayer3Key) => "Media Player 3 Key".into(),
        Some(VideoSource::MediaPlayer4) => "Media Player 4".into(),
        Some(VideoSource::MediaPlayer4Key) => "Media Player 4 Key".into(),
        Some(VideoSource::SuperSource) => "SuperSource".into(),
        Some(VideoSource::Input1Direct) => "Input 1 Direct".into(),
        Some(VideoSource::ME1Prog) => "M/E 1 Program".into(),
        Some(VideoSource::ME1Prev) => "M/E 1 Preview".into(),
        _ => format!("Input {id}"),
    }
}

fn source_rank(id: u16) -> u32 {
    match id {
        0 => 0,
        1000 => 1,
        2001..=2008 => 10 + u32::from(id - 2000),
        3010..=3041 => 30 + u32::from(id - 3010),
        6000 => 80,
        1..=40 => 100 + u32::from(id),
        _ => 10_000 + u32::from(id),
    }
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

fn aux_video_items(state: Option<&AtemState>, aux_bus: u8) -> Vec<SelectItem> {
    routed_items(
        state,
        SourceQuery::Auxiliary(aux_bus),
        fallback_aux_video_items,
    )
}

/// Offline AUX sources for an ATEM Mini HDMI output.
///
/// The Mini/Mini Pro input table and the ATEM Mini product description allow
/// that output to clean-switch the HDMI inputs, direct-switch input 1, or show
/// program and preview. Media players, black, colour bars, and colour
/// generators are inputs on the Mini, and they are not in that output list.
/// Multiview is called out as a Mini Pro output, so the Mini baseline omits it.
fn fallback_aux_video_items() -> Vec<SelectItem> {
    let mut items: Vec<SelectItem> = (1..=OFFLINE_HDMI_INPUTS)
        .map(|id| SelectItem {
            label: format!("Input {id}"),
            value: id.to_string(),
        })
        .collect();
    items.push(SelectItem {
        label: "Input 1 Direct".into(),
        value: (VideoSource::Input1Direct as u16).to_string(),
    });
    items.extend(
        ME_PROGRAM_PREVIEW
            .iter()
            .take(2)
            .map(|(value, label)| SelectItem {
                label: (*label).to_string(),
                value: value.to_string(),
            }),
    );
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

fn aux_source_ids(state: &AtemState, aux_bus: u8) -> Vec<u16> {
    aux_video_items(Some(state), aux_bus)
        .into_iter()
        .filter_map(|item| item.value.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use necromancer::protocol::structs::ExternalPortType;

    use super::*;
    use crate::availability::InputAvailability;

    /// High byte is `SourceAvailability`, low byte is `MeAvailability`.
    fn properties(port: PortType, name: &str, source_bits: u8, me_bits: u8) -> InputProperties {
        let mut long_name = [0u8; 20];
        let bytes = name.as_bytes();
        long_name[..bytes.len()].copy_from_slice(bytes);
        InputProperties {
            video_source: VideoSource::Black,
            long_name,
            short_name: [0; 4],
            input_names_are_default: true,
            available_external_port_types: ExternalPortType::default(),
            external_port_type: ExternalPortType::default(),
            port_type: port,
            availability3: (u16::from(source_bits) << 8) | u16::from(me_bits),
        }
    }

    fn values_for(
        properties: &HashMap<VideoSource, InputProperties>,
        query: SourceQuery,
    ) -> Vec<String> {
        listed_sources(properties, |_source, properties| {
            include_source(
                properties.port_type,
                properties.source_availability(),
                properties.me_availability(),
                query,
            )
        })
        .into_iter()
        .map(|item| item.value)
        .collect()
    }

    #[test]
    fn me_labels_start_at_one_and_keep_the_protocol_index() {
        let items = me_items(None);
        assert_eq!(
            items,
            vec![SelectItem {
                label: "M/E 1".into(),
                value: "0".into(),
            }]
        );
    }

    #[test]
    fn disconnected_aux_sources_follow_the_mini_output() {
        let items = aux_video_items(None, 0);
        let pairs: Vec<(&str, &str)> = items
            .iter()
            .map(|item| (item.label.as_str(), item.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Input 1", "1"),
                ("Input 2", "2"),
                ("Input 3", "3"),
                ("Input 4", "4"),
                ("Input 1 Direct", "11001"),
                ("M/E 1 Program", "10010"),
                ("M/E 1 Preview", "10011"),
            ]
        );
        assert_eq!(aux_video_items(None, 1), items);
        assert_eq!(VideoSource::Input1Direct as u16, 11001);
    }

    #[test]
    fn disconnected_source_list_matches_atem_mini_ids() {
        let items = program_preview_items(None, 0);
        assert_eq!(program_preview_items(None, 1), items);
        let pairs: Vec<(&str, &str)> = items
            .iter()
            .map(|item| (item.label.as_str(), item.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Black", "0"),
                ("Color Bars", "1000"),
                ("Color 1", "2001"),
                ("Color 2", "2002"),
                ("Media Player 1", "3010"),
                ("Media Player 1 Key", "3011"),
                ("Input 1", "1"),
                ("Input 2", "2"),
                ("Input 3", "3"),
                ("Input 4", "4"),
            ]
        );
        assert_eq!(VideoSource::MediaPlayer1 as u16, 3010);
        assert_eq!(VideoSource::MediaPlayer1Key as u16, 3011);
        assert_eq!(VideoSource::Input1 as u16, 1);
        assert_eq!(VideoSource::Input4 as u16, 4);
    }

    #[test]
    fn availability_bit_positions_match_the_protocol_bytes() {
        assert_eq!(
            u8::from(SourceAvailability::new().with_auxiliary(true)),
            1 << 0
        );
        assert_eq!(
            u8::from(SourceAvailability::new().with_key_source(true)),
            1 << 4
        );
        assert_eq!(
            u8::from(SourceAvailability::new().with_auxiliary1(true)),
            1 << 5
        );
        assert_eq!(
            u8::from(SourceAvailability::new().with_auxiliary2(true)),
            1 << 6
        );
        assert_eq!(u8::from(MeAvailability::new().with_me1(true)), 1 << 0);
        assert_eq!(u8::from(MeAvailability::new().with_me2(true)), 1 << 1);
        assert_eq!(u8::from(MeAvailability::new().with_me3(true)), 1 << 2);
        assert_eq!(u8::from(MeAvailability::new().with_me4(true)), 1 << 3);
    }

    #[test]
    fn connected_sources_follow_reported_availability_and_port_type() {
        const AUX: u8 = 1 << 0;
        const KEY: u8 = 1 << 4;
        const AUX1: u8 = 1 << 5;
        const AUX2: u8 = 1 << 6;
        const ME1: u8 = 1 << 0;
        const ME2: u8 = 1 << 1;

        let mut input_properties = HashMap::new();
        input_properties.insert(
            VideoSource::Black,
            properties(PortType::Black, "Black", KEY, ME1),
        );
        input_properties.insert(
            VideoSource::ColourBars,
            properties(PortType::ColourBars, "Color Bars", KEY, ME1),
        );
        input_properties.insert(
            VideoSource::Colour1,
            properties(PortType::ColourGenerator, "Color 1", 0, ME1),
        );
        input_properties.insert(
            VideoSource::MediaPlayer1,
            properties(PortType::MediaPlayerFill, "Media 1", 0, ME1),
        );
        input_properties.insert(
            VideoSource::MediaPlayer1Key,
            properties(PortType::MediaPlayerKey, "Media 1 Key", KEY, ME1),
        );
        input_properties.insert(
            VideoSource::MediaPlayer2,
            properties(PortType::MediaPlayerFill, "Media 2", AUX, ME1 | ME2),
        );
        input_properties.insert(
            VideoSource::SuperSource,
            properties(PortType::SuperSource, "SuperSource", 0, ME2),
        );
        input_properties.insert(
            VideoSource::Input3,
            properties(PortType::External, "Cam 3", AUX | KEY, ME1),
        );
        input_properties.insert(
            VideoSource::Input4,
            properties(PortType::External, "Cam 4", AUX1, ME2),
        );
        input_properties.insert(
            VideoSource::Input5,
            properties(PortType::External, "Cam 5", AUX2, ME1),
        );
        input_properties.insert(
            VideoSource::Input1Direct,
            properties(PortType::ExternalDirect, "Input 1 Direct", AUX, 0),
        );
        input_properties.insert(
            VideoSource::ME1Prog,
            properties(PortType::MEOutput, "Program", AUX, 0),
        );
        input_properties.insert(
            VideoSource::Auxilary1,
            properties(PortType::Auxiliary, "Output", AUX | KEY, ME1),
        );

        let me1 = listed_sources(&input_properties, |_source, properties| {
            include_source(
                properties.port_type,
                properties.source_availability(),
                properties.me_availability(),
                SourceQuery::MixEffect(0),
            )
        });
        let labels: Vec<&str> = me1.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Black",
                "Color Bars",
                "Color 1",
                "Media 1",
                "Media 1 Key",
                "Media 2",
                "Cam 3",
                "Cam 5",
            ]
        );
        assert_eq!(me1[3].value, "3010");
        assert_eq!(me1[4].value, "3011");

        let me2 = values_for(&input_properties, SourceQuery::MixEffect(1));
        assert_eq!(
            me2,
            vec!["3020".to_string(), "6000".to_string(), "4".to_string()]
        );

        let dsk = values_for(&input_properties, SourceQuery::KeySource);
        assert_eq!(
            dsk,
            vec![
                "0".to_string(),
                "1000".to_string(),
                "3011".to_string(),
                "3".to_string(),
            ]
        );

        let aux0 = values_for(&input_properties, SourceQuery::Auxiliary(0));
        assert_eq!(
            aux0,
            vec![
                "3020".to_string(),
                "3".to_string(),
                "4".to_string(),
                "10010".to_string(),
                "11001".to_string(),
            ]
        );
        let aux1 = values_for(&input_properties, SourceQuery::Auxiliary(1));
        assert_eq!(
            aux1,
            vec![
                "3020".to_string(),
                "3".to_string(),
                "5".to_string(),
                "10010".to_string(),
                "11001".to_string(),
            ]
        );
        let aux2 = values_for(&input_properties, SourceQuery::Auxiliary(2));
        assert_eq!(
            aux2,
            vec![
                "3020".to_string(),
                "3".to_string(),
                "10010".to_string(),
                "11001".to_string(),
            ]
        );

        let sample = input_properties.get(&VideoSource::Input3).unwrap();
        assert!(sample.source_availability().auxiliary());
        assert!(sample.source_availability().key_source());
        assert!(sample.me_availability().me1());
        assert!(!sample.me_availability().me2());
    }
}
