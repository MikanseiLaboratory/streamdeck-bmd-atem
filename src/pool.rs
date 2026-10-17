//! One supervised ATEM session per switcher IP.
//!
//! Each address gets a supervisor task, matching the HyperDeck and vMix pools:
//! `Connecting`, then `Connected`, or `Disconnected { message }` with exponential
//! backoff. `AtemController::connect_udp` is called with `reconnect = false` so a
//! dropped session is visible here instead of being retried inside necromancer.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use necromancer::{AtemController, StateUpdate};
use tokio::sync::{broadcast, Mutex};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

const ATEM_PORT: u16 = 9910;

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

/// Live connection state of one switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
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
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Disconnected { .. } => "Disconnected",
        }
    }

    /// Inspector text. Empty once the session is up.
    pub fn detail(&self) -> String {
        match self {
            Self::Connecting => "Connecting".to_string(),
            Self::Connected => String::new(),
            Self::Disconnected { message } if message.is_empty() => "Disconnected".to_string(),
            Self::Disconnected { message } => message.clone(),
        }
    }
}

/// Something the pool wants the plugin to repaint or report.
#[derive(Clone, Debug)]
pub enum PoolEvent {
    /// Connection label changed.
    Status { ip: String },
    /// Cached switcher state changed.
    State { ip: String, updated: StateUpdate },
}

struct Slot {
    cancel: CancellationToken,
    status: ConnectionStatus,
    controller: Option<Arc<AtemController>>,
}

struct Inner {
    slots: HashMap<String, Slot>,
}

/// Shared pool of ATEM sessions, keyed by switcher IP.
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
            })),
            events,
            options,
        }
    }

    /// Stream of status and state updates.
    pub fn subscribe(&self) -> broadcast::Receiver<PoolEvent> {
        self.events.subscribe()
    }

    /// Start, stop, and keep supervisors so they match `ips`.
    ///
    /// Addresses that were already running are left in place. The returned list
    /// is the addresses whose supervisors were cancelled.
    pub async fn reconcile<I>(&self, ips: I) -> Vec<String>
    where
        I: IntoIterator<Item = String>,
    {
        let wanted: HashSet<String> = ips.into_iter().collect();
        let mut inner = self.inner.lock().await;
        let stale: Vec<String> = inner
            .slots
            .keys()
            .filter(|ip| !wanted.contains(*ip))
            .cloned()
            .collect();
        for ip in &stale {
            if let Some(slot) = inner.slots.remove(ip) {
                slot.cancel.cancel();
            }
        }
        for ip in wanted {
            if inner.slots.contains_key(&ip) {
                continue;
            }
            inner.slots.insert(ip.clone(), self.spawn(ip));
        }
        stale
    }

    /// Connection label for one switcher.
    pub async fn status(&self, ip: &str) -> Option<ConnectionStatus> {
        self.inner
            .lock()
            .await
            .slots
            .get(ip)
            .map(|slot| slot.status.clone())
    }

    /// Controller for a switcher that has finished initialising.
    pub async fn controller(&self, ip: &str) -> Option<Arc<AtemController>> {
        let inner = self.inner.lock().await;
        let slot = inner.slots.get(ip)?;
        if slot.status.is_connected() {
            slot.controller.clone()
        } else {
            None
        }
    }

    /// Cancel every supervisor.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        for (_, slot) in inner.slots.drain() {
            slot.cancel.cancel();
        }
    }

    fn spawn(&self, ip: String) -> Slot {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let pool = self.clone();
        let options = self.options.clone();
        tokio::spawn(async move {
            supervise(ip, task_cancel, pool, options).await;
        });
        Slot {
            cancel,
            status: ConnectionStatus::Connecting,
            controller: None,
        }
    }
}

async fn set_session(
    pool: &AtemPool,
    ip: &str,
    status: ConnectionStatus,
    controller: Option<Arc<AtemController>>,
) {
    let mut inner = pool.inner.lock().await;
    let Some(slot) = inner.slots.get_mut(ip) else {
        return;
    };
    slot.status = status;
    slot.controller = controller;
    drop(inner);
    let _ = pool.events.send(PoolEvent::Status { ip: ip.to_string() });
}

async fn supervise(ip: String, cancel: CancellationToken, pool: AtemPool, options: PoolOptions) {
    let mut backoff = options.initial_backoff;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let address = match Ipv4Addr::from_str(&ip) {
            Ok(address) => address,
            Err(error) => {
                set_session(
                    &pool,
                    &ip,
                    ConnectionStatus::Disconnected {
                        message: format!("invalid ATEM IP {ip}: {error}"),
                    },
                    None,
                )
                .await;
                cancel.cancelled().await;
                break;
            }
        };
        let socket = SocketAddrV4::new(address, ATEM_PORT);
        set_session(&pool, &ip, ConnectionStatus::Connecting, None).await;
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
                set_session(
                    &pool,
                    &ip,
                    ConnectionStatus::Connected,
                    Some(Arc::clone(&controller)),
                )
                .await;
                let closed = watch_state(&ip, &controller, &cancel, &pool).await;
                drop(controller);
                if !closed {
                    break;
                }
                set_session(
                    &pool,
                    &ip,
                    ConnectionStatus::Disconnected {
                        message: "connection closed".to_string(),
                    },
                    None,
                )
                .await;
                backoff = options.initial_backoff;
            }
            Err(error) => {
                tracing::warn!("ATEM session failed for {ip}: {error}");
                set_session(
                    &pool,
                    &ip,
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
    ip: &str,
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
                            ip: ip.to_string(),
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
    async fn invalid_address_stays_disconnected_until_removed() {
        let pool = AtemPool::new(PoolOptions::for_tests());
        let mut events = pool.subscribe();
        pool.reconcile(["999.1.1.1".to_string()]).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(ConnectionStatus::Disconnected { message }) = pool.status("999.1.1.1").await
            {
                assert!(message.contains("invalid ATEM IP"), "{message}");
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("status: {:?}", pool.status("999.1.1.1").await);
            }
            let _ = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        }
        assert!(pool.controller("999.1.1.1").await.is_none());
        let mut removed = pool.reconcile(Vec::<String>::new()).await;
        removed.sort();
        assert_eq!(removed, vec!["999.1.1.1".to_string()]);
        assert!(pool.status("999.1.1.1").await.is_none());
    }

    #[tokio::test]
    async fn reconcile_keeps_an_existing_supervisor() {
        let pool = AtemPool::new(PoolOptions::for_tests());
        pool.reconcile(["999.1.1.1".to_string()]).await;
        let removed = pool
            .reconcile(["999.1.1.1".to_string(), "999.2.2.2".to_string()])
            .await;
        assert!(removed.is_empty());
        assert!(pool.status("999.1.1.1").await.is_some());
        assert!(pool.status("999.2.2.2").await.is_some());
    }
}
