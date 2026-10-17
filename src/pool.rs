//! One supervised ATEM session per configured switcher.
//!
//! The catalog comes from Stream Deck global settings, matching the HyperDeck
//! and vMix pools. A supervisor stays up while its instance remains configured.
//! Removing a key does not drop the session. `AtemController::connect_udp` is
//! called with `reconnect = false` so a dropped session is retried here.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use necromancer::{AtemController, StateUpdate};
use tokio::sync::{broadcast, Mutex};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// ATEM control port used when an instance does not set one.
pub const ATEM_PORT: u16 = 9910;

/// How a supervisor waits between attempts.
#[derive(Clone, Debug)]
pub struct PoolOptions {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

impl PoolOptions {
    /// Short waits for tests that never open a socket.
    pub fn for_tests() -> Self {
        Self {
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(50),
        }
    }
}

/// One switcher in the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtemInstanceConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
}

impl AtemInstanceConfig {
    fn endpoint_eq(&self, other: &Self) -> bool {
        self.host == other.host && self.port == other.port && self.enabled == other.enabled
    }
}

/// Live connection state of one switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disabled,
    Connecting,
    Connected,
    Disconnected { message: String },
}

impl ConnectionStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Short label for logs and the property inspector.
    pub fn label(&self) -> &str {
        match self {
            Self::Disabled => "Disabled",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Disconnected { .. } => "Disconnected",
        }
    }

    /// Inspector text. Empty once the session is up.
    pub fn detail(&self) -> String {
        match self {
            Self::Disabled => "Disabled".to_string(),
            Self::Connecting => "Connecting".to_string(),
            Self::Connected => String::new(),
            Self::Disconnected { message } if message.is_empty() => "Disconnected".to_string(),
            Self::Disconnected { message } => message.clone(),
        }
    }
}

/// Status row for one configured switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceStatus {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
    pub status: ConnectionStatus,
}

/// Something the pool wants the plugin to repaint or report.
#[derive(Clone, Debug)]
pub enum PoolEvent {
    /// Connection label changed.
    Status { id: String },
    /// Cached switcher state changed.
    State { id: String, updated: StateUpdate },
}

struct Slot {
    config: AtemInstanceConfig,
    cancel: CancellationToken,
    generation: u64,
    status: ConnectionStatus,
    controller: Option<Arc<AtemController>>,
}

struct Inner {
    slots: HashMap<String, Slot>,
    configs: Vec<AtemInstanceConfig>,
    next_generation: u64,
}

/// Shared pool of ATEM sessions, keyed by instance id.
#[derive(Clone)]
pub struct AtemPool {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<PoolEvent>,
    options: PoolOptions,
}

impl AtemPool {
    /// Empty pool. Supervisors start from [`Self::reconcile`].
    pub fn new(options: PoolOptions) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                slots: HashMap::new(),
                configs: Vec::new(),
                next_generation: 1,
            })),
            events,
            options,
        }
    }

    /// Stream of status and state updates.
    pub fn subscribe(&self) -> broadcast::Receiver<PoolEvent> {
        self.events.subscribe()
    }

    /// Start, stop, and restart supervisors so they match `configs`.
    ///
    /// An unchanged host, port, and enabled flag keeps the running supervisor.
    /// A disabled instance keeps a slot and does not open a socket. The returned
    /// list is the instances whose supervisors were cancelled because they left
    /// the catalog.
    pub async fn reconcile(&self, configs: Vec<AtemInstanceConfig>) -> Vec<AtemInstanceConfig> {
        let ordered = dedupe_configs(configs);
        let mut inner = self.inner.lock().await;
        let stale: Vec<String> = inner
            .slots
            .keys()
            .filter(|id| !ordered.iter().any(|config| config.id == **id))
            .cloned()
            .collect();
        let mut removed = Vec::new();
        for id in stale {
            if let Some(slot) = inner.slots.remove(&id) {
                slot.cancel.cancel();
                removed.push(slot.config);
            }
        }
        for config in &ordered {
            let same_endpoint = inner
                .slots
                .get(&config.id)
                .is_some_and(|slot| slot.config.endpoint_eq(config));
            if same_endpoint {
                let Some(slot) = inner.slots.get_mut(&config.id) else {
                    continue;
                };
                if slot.config.name != config.name {
                    slot.config.name = config.name.clone();
                    let _ = self.events.send(PoolEvent::Status {
                        id: config.id.clone(),
                    });
                }
                continue;
            }
            if let Some(slot) = inner.slots.remove(&config.id) {
                slot.cancel.cancel();
            }
            let generation = bump(&mut inner);
            if !config.enabled {
                inner
                    .slots
                    .insert(config.id.clone(), disabled_slot(config.clone(), generation));
                let _ = self.events.send(PoolEvent::Status {
                    id: config.id.clone(),
                });
                continue;
            }
            inner
                .slots
                .insert(config.id.clone(), self.spawn(config.clone(), generation));
        }
        inner.configs = ordered;
        removed
    }

    /// Drop the current session and connect again.
    pub async fn reconnect(&self, id: &str) {
        let mut inner = self.inner.lock().await;
        let Some(config) = inner.slots.get(id).map(|slot| slot.config.clone()) else {
            return;
        };
        if !config.enabled {
            return;
        }
        if let Some(slot) = inner.slots.remove(id) {
            slot.cancel.cancel();
        }
        let generation = bump(&mut inner);
        inner
            .slots
            .insert(config.id.clone(), self.spawn(config, generation));
    }

    /// Connection label for one instance.
    pub async fn status(&self, id: &str) -> Option<ConnectionStatus> {
        self.inner
            .lock()
            .await
            .slots
            .get(id)
            .map(|slot| slot.status.clone())
    }

    /// Connection label for the instance that uses `host`.
    ///
    /// An enabled instance wins when several rows share the address.
    pub async fn status_for_host(&self, host: &str) -> Option<ConnectionStatus> {
        let host = host.trim();
        let inner = self.inner.lock().await;
        let mut fallback = None;
        for slot in inner.slots.values() {
            if slot.config.host.trim() != host {
                continue;
            }
            if slot.config.enabled {
                return Some(slot.status.clone());
            }
            fallback = Some(slot.status.clone());
        }
        fallback
    }

    /// Catalog row for one instance.
    pub async fn config(&self, id: &str) -> Option<AtemInstanceConfig> {
        self.inner
            .lock()
            .await
            .slots
            .get(id)
            .map(|slot| slot.config.clone())
    }

    /// Controller for an instance that has finished initialising.
    pub async fn controller(&self, id: &str) -> Option<Arc<AtemController>> {
        let inner = self.inner.lock().await;
        let slot = inner.slots.get(id)?;
        if slot.status.is_connected() {
            slot.controller.clone()
        } else {
            None
        }
    }

    /// Controller for an enabled, connected instance on `host`.
    pub async fn controller_for_host(&self, host: &str) -> Option<Arc<AtemController>> {
        let host = host.trim();
        let inner = self.inner.lock().await;
        inner.slots.values().find_map(|slot| {
            if slot.config.host.trim() == host && slot.config.enabled && slot.status.is_connected()
            {
                slot.controller.clone()
            } else {
                None
            }
        })
    }

    /// Status of every configured switcher, in catalog order.
    pub async fn statuses(&self) -> Vec<InstanceStatus> {
        let inner = self.inner.lock().await;
        inner
            .configs
            .iter()
            .map(|config| {
                let status = inner
                    .slots
                    .get(&config.id)
                    .map(|slot| slot.status.clone())
                    .unwrap_or(ConnectionStatus::Disconnected {
                        message: "missing".into(),
                    });
                InstanceStatus {
                    id: config.id.clone(),
                    name: config.name.clone(),
                    host: config.host.clone(),
                    port: config.port,
                    enabled: config.enabled,
                    status,
                }
            })
            .collect()
    }

    /// Cancel every supervisor.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        for (_, slot) in inner.slots.drain() {
            slot.cancel.cancel();
        }
        inner.configs.clear();
    }

    fn spawn(&self, config: AtemInstanceConfig, generation: u64) -> Slot {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let pool = self.clone();
        let options = self.options.clone();
        let task_config = config.clone();
        tokio::spawn(async move {
            supervise(task_config, generation, task_cancel, pool, options).await;
        });
        Slot {
            config,
            cancel,
            generation,
            status: ConnectionStatus::Connecting,
            controller: None,
        }
    }
}

fn dedupe_configs(configs: Vec<AtemInstanceConfig>) -> Vec<AtemInstanceConfig> {
    let mut ordered = Vec::new();
    for config in configs {
        if let Some(index) = ordered
            .iter()
            .position(|item: &AtemInstanceConfig| item.id == config.id)
        {
            ordered[index] = config;
        } else {
            ordered.push(config);
        }
    }
    ordered
}

fn bump(inner: &mut Inner) -> u64 {
    let generation = inner.next_generation;
    inner.next_generation = inner.next_generation.wrapping_add(1);
    generation
}

fn disabled_slot(config: AtemInstanceConfig, generation: u64) -> Slot {
    Slot {
        config,
        cancel: CancellationToken::new(),
        generation,
        status: ConnectionStatus::Disabled,
        controller: None,
    }
}

async fn set_session(
    pool: &AtemPool,
    id: &str,
    generation: u64,
    status: ConnectionStatus,
    controller: Option<Arc<AtemController>>,
) -> bool {
    let mut inner = pool.inner.lock().await;
    let Some(slot) = inner.slots.get_mut(id) else {
        return false;
    };
    if slot.generation != generation {
        return false;
    }
    slot.status = status;
    slot.controller = controller;
    drop(inner);
    let _ = pool.events.send(PoolEvent::Status { id: id.to_string() });
    true
}

async fn supervise(
    config: AtemInstanceConfig,
    generation: u64,
    cancel: CancellationToken,
    pool: AtemPool,
    options: PoolOptions,
) {
    let mut backoff = options.initial_backoff;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let address = match Ipv4Addr::from_str(config.host.trim()) {
            Ok(address) => address,
            Err(error) => {
                set_session(
                    &pool,
                    &config.id,
                    generation,
                    ConnectionStatus::Disconnected {
                        message: format!("invalid ATEM IP {}: {error}", config.host),
                    },
                    None,
                )
                .await;
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(options.max_backoff);
                continue;
            }
        };
        let socket = SocketAddrV4::new(address, config.port);
        set_session(
            &pool,
            &config.id,
            generation,
            ConnectionStatus::Connecting,
            None,
        )
        .await;
        tracing::info!("connecting to ATEM at {socket}");
        let connected = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = AtemController::connect_udp(socket, false) => result,
        };
        let mut failed = false;
        match connected {
            Ok(controller) => {
                let controller = Arc::new(controller);
                if !set_session(
                    &pool,
                    &config.id,
                    generation,
                    ConnectionStatus::Connected,
                    Some(Arc::clone(&controller)),
                )
                .await
                {
                    break;
                }
                let closed = watch_state(&config.id, &controller, &cancel, &pool).await;
                drop(controller);
                if !closed {
                    break;
                }
                set_session(
                    &pool,
                    &config.id,
                    generation,
                    ConnectionStatus::Disconnected {
                        message: "connection closed".to_string(),
                    },
                    None,
                )
                .await;
                backoff = options.initial_backoff;
            }
            Err(error) => {
                tracing::warn!("ATEM session failed for {}: {error}", config.host);
                set_session(
                    &pool,
                    &config.id,
                    generation,
                    ConnectionStatus::Disconnected {
                        message: error.to_string(),
                    },
                    None,
                )
                .await;
                failed = true;
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = sleep(backoff) => {}
        }
        if failed {
            backoff = (backoff * 2).min(options.max_backoff);
        }
    }
}

/// `true` when the session ended on its own. `false` when `cancel` fired.
async fn watch_state(
    id: &str,
    controller: &AtemController,
    cancel: &CancellationToken,
    pool: &AtemPool,
) -> bool {
    let mut updates = controller.state_update_events();
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            update = updates.recv() => {
                match update {
                    Ok((_txn, updated)) => {
                        let _ = pool.events.send(PoolEvent::State {
                            id: id.to_string(),
                            updated,
                        });
                    }
                    Err(_) => return true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, host: &str, enabled: bool) -> AtemInstanceConfig {
        AtemInstanceConfig {
            id: id.into(),
            name: id.into(),
            host: host.into(),
            port: ATEM_PORT,
            enabled,
        }
    }

    async fn wait_disconnected(pool: &AtemPool, id: &str) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(ConnectionStatus::Disconnected { message }) = pool.status(id).await {
                return message;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("status for {id}: {:?}", pool.status(id).await);
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn protocol_2_32_is_accepted() {
        use necromancer::protocol::atom::Version;
        for minor in [30, 31, 32] {
            let version = Version { major: 2, minor };
            assert!(
                version.check_firmware_version().is_ok(),
                "protocol 2.{minor} should be accepted"
            );
        }
        for version in [
            Version {
                major: 2,
                minor: 29,
            },
            Version {
                major: 2,
                minor: 33,
            },
            Version {
                major: 1,
                minor: 32,
            },
        ] {
            let error = version.check_firmware_version().unwrap_err();
            let text = error.to_string();
            assert!(
                text.contains(&format!("{}.{}", version.major, version.minor)),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_address_retries_and_reconcile_keeps_the_supervisor() {
        let pool = AtemPool::new(PoolOptions {
            initial_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(60),
        });
        pool.reconcile(vec![cfg("a", "999.1.1.1", true)]).await;
        let message = wait_disconnected(&pool, "a").await;
        assert!(message.contains("invalid ATEM IP"), "{message}");
        assert!(pool.controller("a").await.is_none());
        assert!(pool.controller_for_host("999.1.1.1").await.is_none());

        pool.reconcile(vec![cfg("a", "999.1.1.1", true)]).await;
        assert!(
            matches!(
                pool.status("a").await,
                Some(ConnectionStatus::Disconnected { .. })
            ),
            "an unchanged config must not restart the supervisor: {:?}",
            pool.status("a").await
        );

        let mut renamed = cfg("a", "999.1.1.1", true);
        renamed.name = "Studio".into();
        pool.reconcile(vec![renamed, cfg("b", "999.2.2.2", true)])
            .await;
        assert!(matches!(
            pool.status("a").await,
            Some(ConnectionStatus::Disconnected { .. })
        ));
        assert_eq!(pool.statuses().await[0].name, "Studio");
        assert!(pool.status("b").await.is_some());

        let removed = pool.reconcile(Vec::new()).await;
        assert_eq!(removed.len(), 2);
        assert!(pool.status("a").await.is_none());
        assert!(pool.status("b").await.is_none());
    }

    #[tokio::test]
    async fn disabled_instance_has_no_controller() {
        let pool = AtemPool::new(PoolOptions::for_tests());
        pool.reconcile(vec![cfg("a", "999.1.1.1", false)]).await;
        assert_eq!(pool.status("a").await, Some(ConnectionStatus::Disabled));
        assert!(pool.controller("a").await.is_none());
        assert_eq!(
            pool.status_for_host("999.1.1.1").await,
            Some(ConnectionStatus::Disabled)
        );

        pool.reconcile(vec![cfg("a", "999.1.1.1", false)]).await;
        assert_eq!(pool.status("a").await, Some(ConnectionStatus::Disabled));

        pool.reconcile(vec![cfg("a", "999.1.1.1", true)]).await;
        assert!(
            matches!(
                pool.status("a").await,
                Some(ConnectionStatus::Connecting | ConnectionStatus::Disconnected { .. })
            ),
            "{:?}",
            pool.status("a").await
        );
    }

    #[tokio::test]
    async fn replacing_the_host_ignores_the_previous_supervisor() {
        let pool = AtemPool::new(PoolOptions::for_tests());
        pool.reconcile(vec![cfg("a", "999.1.1.1", true)]).await;
        let first = wait_disconnected(&pool, "a").await;
        assert!(first.contains("999.1.1.1"), "{first}");

        pool.reconcile(vec![cfg("a", "999.3.3.3", true)]).await;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let mut saw_new = false;
        while tokio::time::Instant::now() < deadline {
            match pool.status("a").await {
                Some(ConnectionStatus::Disconnected { message }) => {
                    assert!(
                        message.contains("999.3.3.3"),
                        "stale supervisor wrote {message}"
                    );
                    saw_new = true;
                }
                Some(ConnectionStatus::Connecting) => {}
                other => panic!("unexpected status: {other:?}"),
            }
            sleep(Duration::from_millis(15)).await;
        }
        assert!(
            saw_new,
            "replacement supervisor never reported the new host"
        );
        assert!(pool.status("a").await.is_some());
    }
}
