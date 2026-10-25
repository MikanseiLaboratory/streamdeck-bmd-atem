//! Shared ATEM connections, key tallies, and Property Inspector updates.
//!
//! [`AtemPool`] keeps one supervisor per switcher in the global-settings catalog.
//! Status and state events from that pool drive key images and any open inspector.
//! A key selects its switcher with `atem_ip`. Closing the key does not disconnect it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use necromancer::protocol::atom::{InputProperties, MediaPlayerSourceID};
use necromancer::protocol::structs::{PortType, VideoSource};
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
    MediaKind, OptionsMessage, SelectItem, StatusMessage,
};
use crate::pool::{AtemInstanceConfig, AtemPool, ConnectionStatus, PoolEvent, PoolOptions};

const IMAGE_RED: &str = "Images/red.png";
const IMAGE_GREEN: &str = "Images/green.png";
const IMAGE_GRAY: &str = "Images/gray.png";

/// UUID of the media player action, used by the title and tally helpers.
const MEDIA_PLAYER_ACTION: &str = "dev.miseinul.atem.mediaplayer";

/// State changes that reshape the media pool: player count, still/clip banks, and
/// the per-slot descriptions the switcher sends once it is connected.
const MEDIA_PLAYER_TOPOLOGY: StateUpdate = StateUpdate::from_bits_truncate(
    StateUpdate::MEDIA_PLAYER_CAPABILITIES.bits()
        | StateUpdate::MEDIA_PLAYER_FRAME_DESCRIPTION.bits()
        | StateUpdate::MEDIA_POOL_CLIP_DESCRIPTION.bits()
        | StateUpdate::MEDIA_PLAYER_SOURCE.bits(),
);

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
        self.refresh_media_title(action, context, settings).await;
    }

    /// Name the media player key before the switcher reports anything.
    async fn refresh_media_title(&self, action: &str, context: &str, settings: &ActionSettings) {
        if action != MEDIA_PLAYER_ACTION {
            return;
        }
        let state = {
            let global = self.runtime.global.read().await.clone();
            let mut found = None;
            for id in resolve_targets(&global.instances, &global.groups, settings) {
                let Some(instance) = global.instances.iter().find(|item| item.id == id) else {
                    continue;
                };
                if let Some(controller) =
                    self.runtime.pool.controller_for_host(&instance.host).await
                {
                    found = Some(controller.get_state().await);
                    break;
                }
            }
            found
        };
        let title = media_title(state.as_deref(), settings);
        self.runtime.set_title(context, &title).await;
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
        if command.target.is_some() {
            settings.target = command.target.clone();
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
        let items = self.runtime.options(&command.kind, &settings).await;
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
        let source_ids = aux_source_ids(&state);
        if source_ids.is_empty() {
            return None;
        }
        let current = settings.video_source.unwrap_or(1);
        let index = source_ids.iter().position(|id| *id == current).unwrap_or(0);
        let new_id = source_ids[cycle_index(source_ids.len(), index, ticks)];
        let aux_bus = settings.aux_bus.unwrap_or(0);
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

    /// Load the configured still or clip into the media player.
    pub async fn press_media(&self, context: &str, settings: &ActionSettings) {
        let targets = self.targets_for(context, settings, true).await;
        if targets.is_empty() {
            return;
        }
        let player = settings.media_player.unwrap_or(0);
        let mut failed = false;
        for target in &targets {
            let state = target.controller.get_state().await;
            let Some(source) = requested_media_source(&state, settings) else {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM media player {} has no bank for this source on {}",
                        player + 1,
                        target.label
                    ))
                    .await;
                continue;
            };
            if let Err(error) = target
                .controller
                .set_media_player_source(player, source)
                .await
            {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM media player change failed on {} (player={player}, source={source:?}): {error}",
                        target.label
                    ))
                    .await;
                continue;
            }
            let state = target.controller.get_state().await;
            self.runtime.refresh_media(&target.host, &state).await;
        }
        if failed {
            self.runtime.show_alert(context).await;
        }
    }

    /// Step the media player to the next or previous slot and return the settings to save.
    ///
    /// Empty still slots are skipped: the switcher refuses them and the operator meant
    /// to walk through loaded graphics.
    pub async fn rotate_media(
        &self,
        context: &str,
        settings: &ActionSettings,
        ticks: i64,
    ) -> Option<ActionSettings> {
        let targets = self.targets_for(context, settings, true).await;
        let first = targets.first()?;
        let player = settings.media_player.unwrap_or(0);
        let state = first.controller.get_state().await;
        let Some(source) = requested_media_source(&state, settings) else {
            self.runtime.show_alert(context).await;
            return None;
        };
        let bank = match source {
            MediaPlayerSourceID::Still(_) => MediaKind::Still,
            MediaPlayerSourceID::VideoClip(_) => MediaKind::Clip,
        };
        let len = media_count(&state, bank);
        if len == 0 {
            self.runtime.show_alert(context).await;
            return None;
        }
        let current = usize::from(media_source_index(source)).min(len - 1);
        let step = if ticks < 0 { -1 } else { 1 };
        let new_index = match next_slot(&state, bank, current, step) {
            Some(index) => index,
            None => {
                self.runtime
                    .log(&format!(
                        "ATEM media player {} has no other {} slots to step to",
                        player + 1,
                        bank_value(bank)
                    ))
                    .await;
                self.runtime.show_alert(context).await;
                return None;
            }
        };
        let next_source = media_source(bank, new_index);
        let mut failed = false;
        for target in &targets {
            if let Err(error) = target
                .controller
                .set_media_player_source(player, next_source)
                .await
            {
                failed = true;
                self.runtime
                    .log(&format!(
                        "ATEM media player dial failed on {} (player={player}, source={next_source:?}): {error}",
                        target.label
                    ))
                    .await;
            }
        }
        if failed {
            self.runtime.show_alert(context).await;
        }
        let mut updated = settings.clone();
        updated.media_player = Some(player);
        updated.media_kind = Some(bank);
        updated.media_index = Some(new_index);
        if let Some(record) = self.runtime.keys.write().await.get_mut(context) {
            record.settings = updated.clone();
        }
        Some(updated)
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
            let items = self.runtime.options(kind, settings).await;
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
        // Stills arrive after the topology, one `MPfe` at a time, so the media lists
        // and key titles are rebuilt whenever one lands.
        if updated.intersects(MEDIA_PLAYER_TOPOLOGY) {
            let state = controller.get_state().await;
            self.refresh_media(ip, &state).await;
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
                let items = self.options(kind, &settings).await;
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
        self.refresh_media(ip, &state).await;
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

    /// Rename the media player keys and encoders from the switcher's media pool.
    ///
    /// Stills use the name typed on the ATEM; clips have no name on the wire, so the
    /// slot number stays. There is no tally here: the key names its source instead.
    async fn refresh_media(&self, ip: &str, state: &AtemState) {
        for (context, settings) in self.keys_for(MEDIA_PLAYER_ACTION, ip).await {
            let title = media_title(Some(state), &settings);
            self.set_title(&context, &title).await;
        }
    }

    async fn set_title(&self, context: &str, title: &str) {
        let guard = self.sender.lock().await;
        if let Some(sender) = guard.as_ref() {
            let _ = sender.set_title(context, Some(title), Target::HardwareAndSoftware, None);
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

    async fn options(&self, kind: &str, settings: &ActionSettings) -> Vec<SelectItem> {
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
            "video" => video_items(state_ref),
            "aux" => aux_items(state_ref),
            "auxVideo" => aux_video_items(state_ref),
            "dsk" => dsk_items(state_ref),
            "mediaPlayer" | "mediaBank" | "mediaIndex" => {
                let bank = settings
                    .media_kind
                    .or_else(|| {
                        state_ref.and_then(|state| {
                            current_bank(state, settings.media_player.unwrap_or(0))
                        })
                    })
                    .unwrap_or(MediaKind::Still);
                let view = media_view(state_ref, bank);
                match kind {
                    "mediaPlayer" => view.players,
                    "mediaBank" => view.banks,
                    _ => view.slots,
                }
            }
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
        "dev.miseinul.atem.mediaplayer" => &["mediaPlayer", "mediaBank", "mediaIndex"],
        _ => &[],
    }
}

/// One switcher's media topology, used by the media player inspector lists.
#[derive(Default)]
struct MediaView {
    players: Vec<SelectItem>,
    banks: Vec<SelectItem>,
    slots: Vec<SelectItem>,
}

/// Build the media lists for `bank`, falling back to a single ATEM Mini player.
fn media_view(state: Option<&AtemState>, bank: MediaKind) -> MediaView {
    let players = state
        .map(|state| usize::from(state.topology.media_players))
        .filter(|count| *count > 0)
        .unwrap_or(1);
    let banks: Vec<SelectItem> = [MediaKind::Still, MediaKind::Clip]
        .into_iter()
        .filter(|candidate| state.is_none_or(|state| media_count(state, *candidate) > 0))
        .map(|candidate| SelectItem {
            label: match candidate {
                MediaKind::Still => "Still".to_string(),
                MediaKind::Clip => "Clip".to_string(),
            },
            value: bank_value(candidate).to_string(),
        })
        .collect();
    let slots = state
        .map(|state| media_count(state, bank))
        .filter(|count| *count > 0)
        .unwrap_or(1);
    MediaView {
        players: (0..players)
            .map(|player| SelectItem {
                label: format!("Media Player {}", player + 1),
                value: player.to_string(),
            })
            .collect(),
        banks,
        slots: (0..slots)
            .map(|index| SelectItem {
                label: media_slot_label(state, bank, index as u8),
                value: index.to_string(),
            })
            .collect(),
    }
}

fn bank_value(bank: MediaKind) -> &'static str {
    match bank {
        MediaKind::Still => "still",
        MediaKind::Clip => "clip",
    }
}

fn video_source_from_id(id: u16) -> VideoSource {
    VideoSource::from_u16(id).unwrap_or(VideoSource::Unknown)
}

fn me_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let count = state
        .map(|state| state.topology.mes)
        .filter(|count| *count > 0)
        .unwrap_or(4);
    (0..count)
        .map(|me| SelectItem {
            label: format!("M/E {}", me + 1),
            value: me.to_string(),
        })
        .collect()
}

fn video_items(state: Option<&AtemState>) -> Vec<SelectItem> {
    let Some(state) = state.filter(|state| !state.input_properties.is_empty()) else {
        return fallback_video_items();
    };
    listed_sources(&state.input_properties)
}

fn listed_sources<'a>(
    properties: impl IntoIterator<Item = (&'a VideoSource, &'a InputProperties)>,
) -> Vec<SelectItem> {
    let mut rows: Vec<(u16, String)> = properties
        .into_iter()
        .filter(|(_, properties)| selectable_port(properties.port_type))
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
            label: "SuperSource".into(),
            value: (VideoSource::SuperSource as u16).to_string(),
        },
    ];
    items.extend((1..=20).map(|id| SelectItem {
        label: format!("Input {id}"),
        value: id.to_string(),
    }));
    items
}

fn selectable_port(port: PortType) -> bool {
    matches!(
        port,
        PortType::External
            | PortType::Black
            | PortType::ColourBars
            | PortType::ColourGenerator
            | PortType::SuperSource
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
        Some(VideoSource::SuperSource) => "SuperSource".into(),
        _ => format!("Input {id}"),
    }
}

fn source_rank(id: u16) -> u32 {
    match id {
        0 => 0,
        1000 => 1,
        2001..=2008 => 10 + u32::from(id - 2000),
        6000 => 20,
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

/// Longest label the encoder touch strip shows without being cut off.
const TITLE_MAX_CHARS: usize = 16;

/// Bank a media player currently plays from, if the switcher has told us.
fn current_bank(state: &AtemState, player: u8) -> Option<MediaKind> {
    match state.get_media_player_source(player)? {
        MediaPlayerSourceID::Still(_) => Some(MediaKind::Still),
        MediaPlayerSourceID::VideoClip(_) => Some(MediaKind::Clip),
    }
}

/// Source id for `bank`/`index`, matching the protocol encoding necromancer uses.
fn media_source(bank: MediaKind, index: u8) -> MediaPlayerSourceID {
    match bank {
        MediaKind::Still => MediaPlayerSourceID::Still(index),
        MediaKind::Clip => MediaPlayerSourceID::VideoClip(index),
    }
}

/// Index the source points at inside its bank.
fn media_source_index(source: MediaPlayerSourceID) -> u8 {
    match source {
        MediaPlayerSourceID::Still(index) | MediaPlayerSourceID::VideoClip(index) => index,
    }
}

/// Next slot in `direction` that the switcher can actually play.
///
/// Returns `None` when the only populated slot is the current one.
fn next_slot(state: &AtemState, bank: MediaKind, current: usize, direction: i64) -> Option<u8> {
    let len = media_count(state, bank);
    if len == 0 {
        return None;
    }
    let mut index = current;
    for _ in 0..len {
        let stepped = ((index as i64 + direction).rem_euclid(len as i64)) as usize;
        index = stepped;
        if index == current {
            break;
        }
        if media_slot_playable(state, bank, index as u8) {
            return Some(index as u8);
        }
    }
    None
}

/// Whether `index` holds something the switcher will load.
fn media_slot_playable(state: &AtemState, bank: MediaKind, index: u8) -> bool {
    match bank {
        MediaKind::Still => state
            .media_player_frame_descriptions
            .get(&index)
            .is_none_or(|frame| frame.is_valid),
        MediaKind::Clip => state
            .media_pool_clip_descriptions
            .get(&index)
            .is_none_or(|clip| clip.is_used),
    }
}

/// Number of slots the switcher reports for `bank`.
fn media_count(state: &AtemState, bank: MediaKind) -> usize {
    match bank {
        MediaKind::Still => usize::from(state.media_player_capabilities.still_count),
        MediaKind::Clip => usize::from(state.media_player_capabilities.clip_count),
    }
}

/// Bank that holds `index`, preferring `preferred` when both banks are long enough.
fn bank_for_index(state: &AtemState, preferred: Option<MediaKind>, index: u8) -> Option<MediaKind> {
    let banks = [preferred, Some(MediaKind::Still), Some(MediaKind::Clip)];
    banks
        .into_iter()
        .flatten()
        .find(|bank| usize::from(index) < media_count(state, *bank))
}

/// Label for one slot of a media bank.
///
/// Both banks carry the name typed on the ATEM: stills in `MPfe`, clips in
/// `MPCS`. Slots the switcher marks unused say so instead of showing a bare
/// index, and slots it never described keep their number.
fn media_slot_label(state: Option<&AtemState>, bank: MediaKind, index: u8) -> String {
    let fallback = match bank {
        MediaKind::Still => format!("Still {}", index + 1),
        MediaKind::Clip => format!("Clip {}", index + 1),
    };
    let Some(state) = state else {
        return fallback;
    };
    let (used, name) = match bank {
        MediaKind::Still => match state.media_player_frame_descriptions.get(&index) {
            Some(frame) => (frame.is_valid, frame.name.as_str()),
            None => return fallback,
        },
        MediaKind::Clip => match state.media_pool_clip_descriptions.get(&index) {
            Some(clip) => (clip.is_used, clip.name.as_str()),
            None => return fallback,
        },
    };
    if !used {
        return "Empty".to_string();
    }
    let name = name.trim();
    if name.is_empty() {
        fallback
    } else {
        name.to_string()
    }
}

/// Bank an untouched key starts on.
///
/// Stills win because that is what media players ship loaded with, unless the
/// switcher reports no still bank at all.
fn default_bank(state: &AtemState, player: u8) -> MediaKind {
    current_bank(state, player).unwrap_or_else(|| {
        if media_count(state, MediaKind::Still) > 0 {
            MediaKind::Still
        } else {
            MediaKind::Clip
        }
    })
}

/// Source id the action asks for.
///
/// A configured `media_kind` wins. Without one the bank follows the slot the action
/// points at: the switcher's current bank for an untouched key, and whatever bank
/// already contains a moved-to index after dialling.
fn requested_media_source(
    state: &AtemState,
    settings: &ActionSettings,
) -> Option<MediaPlayerSourceID> {
    let index = settings.media_index.unwrap_or(0);
    let player = settings.media_player.unwrap_or(0);
    let bank = match (settings.media_kind, settings.media_index) {
        (Some(bank), _) => Some(bank),
        (None, None) => Some(default_bank(state, player)),
        (None, Some(_)) => bank_for_index(state, current_bank(state, player), index),
    }?;
    Some(media_source(bank, index))
}

/// Short label for the key face and encoder touch strip.
///
/// Falls back to the player number while the switcher is still reporting, and trims
/// to what the touch strip shows.
fn media_title(state: Option<&AtemState>, settings: &ActionSettings) -> String {
    let player = settings.media_player.unwrap_or(0);
    let Some(state) = state else {
        return format!("Media {}", player + 1);
    };
    let Some(source) = requested_media_source(state, settings) else {
        return format!("Media {}", player + 1);
    };
    let bank = match source {
        MediaPlayerSourceID::Still(_) => MediaKind::Still,
        MediaPlayerSourceID::VideoClip(_) => MediaKind::Clip,
    };
    let label = media_slot_label(Some(state), bank, media_source_index(source));
    truncate_label(&label, TITLE_MAX_CHARS)
}

/// Clip to `max` characters, marking the cut with an ellipsis.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let mut cut: String = label.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
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
    video_items(state)
        .into_iter()
        .filter_map(|item| item.value.parse().ok())
        .collect()
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use necromancer::protocol::structs::ExternalPortType;

    use super::*;

    fn properties(port: PortType, name: &str) -> InputProperties {
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
            availability3: 0,
        }
    }

    fn media_state(stills: &[(u8, bool, &str)]) -> AtemState {
        let mut state = AtemState::default();
        state.topology.media_players = 2;
        state.media_player_capabilities.still_count = 8;
        state.media_player_capabilities.clip_count = 4;
        for (index, valid, name) in stills {
            state.media_player_frame_descriptions.insert(
                *index,
                necromancer::protocol::atom::MediaPlayerFrameDescription {
                    store_id: 0,
                    index: u16::from(*index),
                    is_valid: *valid,
                    md5: [0; 16],
                    name: (*name).to_string(),
                },
            );
        }
        state
    }

    #[test]
    fn still_slots_show_the_name_typed_on_the_switcher() {
        let state = media_state(&[(0, true, "Lower Third"), (1, true, "  "), (2, false, "x")]);
        assert_eq!(
            media_slot_label(Some(&state), MediaKind::Still, 0),
            "Lower Third"
        );
        // A valid frame with a blank name falls back to the slot number.
        assert_eq!(
            media_slot_label(Some(&state), MediaKind::Still, 1),
            "Still 2"
        );
        // An invalid frame holds nothing to play.
        assert_eq!(media_slot_label(Some(&state), MediaKind::Still, 2), "Empty");
        // Slots the switcher never described keep the numbered label.
        assert_eq!(
            media_slot_label(Some(&state), MediaKind::Still, 5),
            "Still 6"
        );
    }

    #[test]
    fn clip_slots_show_the_name_from_the_media_pool() {
        let mut state = media_state(&[]);
        state.media_pool_clip_descriptions.insert(
            0,
            necromancer::protocol::atom::MediaPoolClipDescription {
                index: 0,
                is_used: true,
                name: "intro.mov".to_string(),
                frames: 900,
            },
        );
        state.media_pool_clip_descriptions.insert(
            1,
            necromancer::protocol::atom::MediaPoolClipDescription {
                index: 1,
                is_used: false,
                name: String::new(),
                frames: 0,
            },
        );
        assert_eq!(
            media_slot_label(Some(&state), MediaKind::Clip, 0),
            "intro.mov"
        );
        // A slot the switcher marks unused holds nothing to play.
        assert_eq!(media_slot_label(Some(&state), MediaKind::Clip, 1), "Empty");
        // Slots it never described keep the numbered label.
        assert_eq!(media_slot_label(Some(&state), MediaKind::Clip, 3), "Clip 4");
        // Without a switcher there is nothing to name but the slot.
        assert_eq!(media_slot_label(None, MediaKind::Clip, 3), "Clip 4");
    }

    #[test]
    fn media_title_prefers_the_still_name_and_falls_back_to_the_player() {
        let state = media_state(&[(0, true, "Lower Third")]);
        let settings = ActionSettings {
            media_player: Some(1),
            media_index: Some(0),
            ..ActionSettings::default()
        };
        assert_eq!(media_title(Some(&state), &settings), "Lower Third");
        assert_eq!(media_title(None, &settings), "Media 2");
    }

    #[test]
    fn media_title_is_trimmed_for_the_touch_strip() {
        let state = media_state(&[(0, true, "A very long still name here")]);
        let settings = ActionSettings {
            media_index: Some(0),
            ..ActionSettings::default()
        };
        let title = media_title(Some(&state), &settings);
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
        assert_eq!(truncate_label("Short", TITLE_MAX_CHARS), "Short");
    }

    #[test]
    fn dial_skips_empty_still_slots_and_wraps() {
        // Slots 0 and 2 are loaded, 1 is empty, 3..7 were never described.
        let state = media_state(&[
            (0, true, "One"),
            (1, false, "x"),
            (2, true, "Two"),
            (3, false, "x"),
        ]);
        assert_eq!(next_slot(&state, MediaKind::Still, 0, 1), Some(2));
        assert_eq!(next_slot(&state, MediaKind::Still, 2, 1), Some(4));
        assert_eq!(next_slot(&state, MediaKind::Still, 0, -1), Some(7));
        // Nothing but the current slot is populated.
        let only = media_state(&[(0, true, "One")]);
        assert_eq!(next_slot(&only, MediaKind::Still, 0, 1), Some(1));
    }

    #[test]
    fn dial_skips_unused_clip_slots() {
        let mut state = media_state(&[]);
        // Slot 1 is unused, so the dial goes straight past it.
        for (index, is_used) in [(0u8, true), (1, false), (2, true)] {
            state.media_pool_clip_descriptions.insert(
                index,
                necromancer::protocol::atom::MediaPoolClipDescription {
                    index,
                    is_used,
                    name: format!("clip{index}"),
                    frames: 10,
                },
            );
        }
        assert_eq!(next_slot(&state, MediaKind::Clip, 0, 1), Some(2));
        assert_eq!(next_slot(&state, MediaKind::Clip, 0, -1), Some(3));
    }

    #[test]
    fn every_described_clip_slot_is_playable() {
        let state = media_state(&[]);
        // Undescribed slots are assumed usable rather than skipped.
        assert_eq!(next_slot(&state, MediaKind::Clip, 0, 1), Some(1));
        assert_eq!(next_slot(&state, MediaKind::Clip, 0, -1), Some(3));
    }

    #[test]
    fn requested_source_follows_the_configured_bank() {
        let state = media_state(&[]);
        let still = ActionSettings {
            media_kind: Some(MediaKind::Still),
            media_index: Some(3),
            media_player: Some(1),
            ..ActionSettings::default()
        };
        assert_eq!(
            requested_media_source(&state, &still),
            Some(MediaPlayerSourceID::Still(3))
        );
        let clip = ActionSettings {
            media_kind: Some(MediaKind::Clip),
            media_index: Some(2),
            ..ActionSettings::default()
        };
        assert_eq!(
            requested_media_source(&state, &clip),
            Some(MediaPlayerSourceID::VideoClip(2))
        );
    }

    #[test]
    fn requested_source_without_a_bank_prefers_a_bank_that_holds_the_index() {
        let mut state = media_state(&[]);
        // Only stills exist, so a clip-sized index still lands in the still bank.
        assert_eq!(
            requested_media_source(
                &state,
                &ActionSettings {
                    media_index: Some(5),
                    ..ActionSettings::default()
                }
            ),
            Some(MediaPlayerSourceID::Still(5))
        );
        // An untouched key starts on the first still.
        assert_eq!(
            requested_media_source(&state, &ActionSettings::default()),
            Some(MediaPlayerSourceID::Still(0))
        );
        state.media_player_capabilities.still_count = 0;
        assert_eq!(
            requested_media_source(&state, &ActionSettings::default()),
            Some(MediaPlayerSourceID::VideoClip(0))
        );
    }

    #[test]
    fn media_lists_follow_the_switcher_and_fall_back_when_disconnected() {
        let view = media_view(None, MediaKind::Still);
        assert_eq!(view.players.len(), 1);
        assert_eq!(view.players[0].label, "Media Player 1");
        assert_eq!(view.banks.len(), 2);
        assert_eq!(view.slots.len(), 1);
        let state = media_state(&[(0, true, "Lower Third")]);
        let view = media_view(Some(&state), MediaKind::Still);
        assert_eq!(view.players.len(), 2);
        assert_eq!(view.slots.len(), 8);
        assert_eq!(view.slots[0].label, "Lower Third");
        let mut state = state;
        state.media_pool_clip_descriptions.insert(
            0,
            necromancer::protocol::atom::MediaPoolClipDescription {
                index: 0,
                is_used: true,
                name: "intro.mov".to_string(),
                frames: 900,
            },
        );
        let view = media_view(Some(&state), MediaKind::Clip);
        assert_eq!(view.slots.len(), 4);
        assert_eq!(view.slots[0].label, "intro.mov");
    }

    #[test]
    fn me_labels_start_at_one_and_keep_the_protocol_index() {
        let items = me_items(None);
        assert_eq!(items[0].label, "M/E 1");
        assert_eq!(items[0].value, "0");
        assert_eq!(items[1].label, "M/E 2");
        assert_eq!(items[1].value, "1");
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn disconnected_aux_sources_include_inputs() {
        let items = aux_video_items(None);
        let values: Vec<&str> = items.iter().map(|item| item.value.as_str()).collect();
        assert!(values.contains(&"0"), "Black");
        assert!(values.contains(&"1"), "Input 1");
        assert!(values.contains(&"1000"), "Color Bars");
        assert!(values.contains(&"10010"), "M/E 1 Program");
        assert!(values.contains(&"10011"), "M/E 1 Preview");
    }

    #[test]
    fn disconnected_source_list_includes_black_and_color() {
        let items = video_items(None);
        let values: Vec<&str> = items.iter().map(|item| item.value.as_str()).collect();
        assert!(values.contains(&"0"), "Black");
        assert!(values.contains(&"1000"), "Color Bars");
        assert!(values.contains(&"2001"), "Color 1");
        assert!(values.contains(&"2002"), "Color 2");
        assert!(values.contains(&"6000"), "SuperSource");
        assert!(values.contains(&"1"));
        assert_eq!(items[0].label, "Black");
        assert_eq!(items[1].label, "Color Bars");
    }

    #[test]
    fn connected_source_list_uses_switcher_names_and_skips_other_ports() {
        let mut input_properties = HashMap::new();
        input_properties.insert(VideoSource::Input3, properties(PortType::External, "Cam 3"));
        input_properties.insert(VideoSource::Black, properties(PortType::Black, "Black"));
        input_properties.insert(
            VideoSource::ColourBars,
            properties(PortType::ColourBars, "Color Bars"),
        );
        input_properties.insert(
            VideoSource::Colour1,
            properties(PortType::ColourGenerator, "Color 1"),
        );
        input_properties.insert(
            VideoSource::MediaPlayer1,
            properties(PortType::MediaPlayerFill, "Media 1"),
        );
        input_properties.insert(
            VideoSource::SuperSource,
            properties(PortType::SuperSource, "SuperSource"),
        );
        let items = listed_sources(&input_properties);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Black", "Color Bars", "Color 1", "SuperSource", "Cam 3"]
        );
        assert_eq!(items[4].value, "3");
        assert_eq!(items[3].value, "6000");
    }
}
