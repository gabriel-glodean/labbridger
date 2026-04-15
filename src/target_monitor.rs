use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::app_config::RelayTarget;
use crate::network_scanner::NetworkScanner;
use crate::relay_probe::{probe_target};
use crate::target_status::{TargetInfo, TargetStatus};

// ── TargetMonitor ─────────────────────────────────────────────────────────────

/// Cloneable shared handle to the background target-status monitor.
///
/// Owns the target configuration, scanner handle, and HTTP client so that
/// on-demand probes need no extra arguments.
///
/// All clones share the same underlying state — `Arc` is used for the mutable
/// parts; `NetworkScanner` and `reqwest::Client` are already cheaply cloneable.
#[derive(Clone)]
pub struct TargetMonitor {
    infos: Arc<RwLock<HashMap<String, TargetInfo>>>,
    targets: Arc<HashMap<String, RelayTarget>>,
    scanner: NetworkScanner,
    client: reqwest::Client,
}

impl TargetMonitor {
    /// Create a monitor pre-populated with `Offline` for every configured target.
    pub fn new(targets: HashMap<String, RelayTarget>, scanner: NetworkScanner) -> Self {
        let infos = targets
            .keys()
            .map(|n| {
                (
                    n.clone(),
                    TargetInfo {
                        status: TargetStatus::Offline,
                        ip: None,
                    },
                )
            })
            .collect();

        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .build()
            .expect("Failed to build probe HTTP client");

        Self {
            infos: Arc::new(RwLock::new(infos)),
            targets: Arc::new(targets),
            scanner,
            client,
        }
    }

    /// Spawn a background task that probes every target at `interval` and
    /// updates their status in place.
    pub fn start(&self, interval: Duration) {
        let infos = self.infos.clone();
        let targets = self.targets.clone();
        let scanner = self.scanner.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            loop {
                for (name, target) in targets.iter() {
                    let info = probe_target(&client, target, &scanner).await;
                    eprintln!("[monitor] {} → {:?}", name, info.status);
                    infos.write().unwrap().insert(name.clone(), info);
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// Snapshot of all target statuses — suitable for a `/relays` endpoint.
    pub fn get_all(&self) -> HashMap<String, TargetInfo> {
        self.infos.read().unwrap().clone()
    }

    /// Status of a single named target, or `None` if the name is unknown.
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<TargetInfo> {
        self.infos.read().unwrap().get(name).cloned()
    }

    /// Get a reference to the network scanner used by this monitor.
    pub fn scanner(&self) -> &NetworkScanner {
        &self.scanner
    }

    /// Get a reference to the HTTP client used by this monitor.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

}
