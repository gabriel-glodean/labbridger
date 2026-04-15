use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surge_ping::{Client, Config, PingIdentifier, PingSequence};
use tokio::task::JoinSet;
use tokio::time::timeout;

// ── 1. Result types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub mac_address: Option<String>,
    pub discovered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestDevice {
    pub ip: IpAddr,
    pub mac_address: Option<String>,
    pub discovered_at: DateTime<Utc>,
}

// ── 2. ARP helper (Linux: /proc/net/arp) ───────────────────────────────────

fn read_arp_table() -> HashMap<IpAddr, String> {
    let mut map = HashMap::new();
    let Ok(content) = std::fs::read_to_string("/proc/net/arp") else {
        return map;
    };
    // Header: IP address  HW type  Flags  HW address  Mask  Device
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 4 {
            if let Ok(ip) = cols[0].parse::<IpAddr>() {
                let mac = cols[3].to_string();
                if mac != "00:00:00:00:00:00" {
                    map.insert(ip, mac);
                }
            }
        }
    }
    map
}

// ── 3. Single-pass scan ─────────────────────────────────────────────────────

/// Pings every host on `network_base`.2–254 (e.g. "192.168.1"), then looks up
/// their MACs from the kernel ARP cache. Returns only the hosts that responded.
pub async fn scan_network(network_base: &str) -> HashMap<IpAddr, DeviceInfo> {
    let client = match Client::new(&Config::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to create ping client (try running with sudo or set CAP_NET_RAW): {e}");
            return HashMap::new();
        }
    };

    let parts: Vec<u8> = network_base
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    if parts.len() != 3 {
        eprintln!("Invalid network_base '{}': expected format 'a.b.c'", network_base);
        return HashMap::new();
    }
    let (a, b, c) = (parts[0], parts[1], parts[2]);

    let mut set = JoinSet::new();
    for i in 2u8..=254 {
        let client = client.clone();
        let ip_addr = IpAddr::V4(Ipv4Addr::new(a, b, c, i));
        set.spawn(async move {
            let mut pinger = client.pinger(ip_addr, PingIdentifier(i as u16)).await;
            let alive = timeout(
                Duration::from_secs(2),
                pinger.ping(PingSequence(0), &[0; 8]),
            )
            .await
            .ok()
            .and_then(|r| r.ok())
            .is_some();
            (ip_addr, alive)
        });
    }

    let mut alive_ips = Vec::new();
    let mut count = 0usize;
    while let Some(Ok((ip_addr, alive))) = set.join_next().await {
        count += 1;
        if alive {
            alive_ips.push(ip_addr);
        }
        if count % 25 == 0 {
            println!("Progress: scanned {} IPs so far...", count);
        }
    }

    // ARP table is populated by the OS after the pings above
    let arp = read_arp_table();
    let now = Utc::now();

    alive_ips
        .into_iter()
        .map(|ip| {
            let info = DeviceInfo {
                mac_address: arp.get(&ip).cloned(),
                discovered_at: now,
            };
            (ip, info)
        })
        .collect()
}

// ── 4. Background scanner ───────────────────────────────────────────────────

/// Cloneable handle to a background scan loop.
/// Call `start()` once to kick off the loop, then call `get_devices()` from
/// any thread/task to get the latest snapshot.
#[derive(Clone)]
pub struct NetworkScanner {
    devices: Arc<RwLock<HashMap<IpAddr, DeviceInfo>>>,
    latest: Arc<RwLock<Option<LatestDevice>>>,
    network_base: String,
}

impl NetworkScanner {
    pub fn new(network_base: impl Into<String>) -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            latest: Arc::new(RwLock::new(None)),
            network_base: network_base.into(),
        }
    }

    /// Spawns a Tokio task that scans continuously, waiting `delay_seconds`
    /// between passes. Must be called from an async context.
    pub fn start(&self, delay_seconds: u64) {
        let devices = self.devices.clone();
        let latest = self.latest.clone();
        let network_base = self.network_base.clone();
        tokio::spawn(async move {
            loop {
                println!("Starting network scan...");
                let mut new_map = scan_network(&network_base).await;
                println!("Network scan finished — {} device(s) found. Waiting for next scan...", new_map.len());
                {
                    let mut guard = devices.write().unwrap();
                    for (ip, new_info) in new_map.iter_mut() {
                        if let Some(existing) = guard.get(ip) {
                            let same_device = match (&existing.mac_address, &new_info.mac_address) {
                                (Some(old_mac), Some(new_mac)) => old_mac == new_mac,
                                _ => true,
                            };

                            if same_device {
                                new_info.discovered_at = existing.discovered_at;
                                if new_info.mac_address.is_none() {
                                    new_info.mac_address = existing.mac_address.clone();
                                }
                            } else {
                                println!(
                                    "WARNING: IP {} reassigned — MAC changed from {} to {}",
                                    ip,
                                    existing.mac_address.as_deref().unwrap_or("unknown"),
                                    new_info.mac_address.as_deref().unwrap_or("unknown")
                                );
                            }
                        }
                    }

                    // Update latest: pick the device with the most recent discovered_at
                    // that is either new or had its MAC change (fresh discovered_at == now).
                    if let Some((ip, info)) = new_map
                        .iter()
                        .max_by_key(|(_, info)| info.discovered_at)
                    {
                        let mut latest_guard = latest.write().unwrap();
                        let is_newer = latest_guard
                            .as_ref()
                            .map_or(true, |l| info.discovered_at > l.discovered_at);
                        if is_newer {
                            *latest_guard = Some(LatestDevice {
                                ip: *ip,
                                mac_address: info.mac_address.clone(),
                                discovered_at: info.discovered_at,
                            });
                        }
                    }

                    *guard = new_map;
                }
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
            }
        });
    }

    /// Returns a snapshot of the most recently completed scan.
    pub fn get_devices(&self) -> HashMap<IpAddr, DeviceInfo> {
        self.devices.read().unwrap().clone()
    }

    /// Looks up the current IP address for a given MAC address (case-insensitive).
    /// Returns `None` when the MAC has not been seen in the last completed scan.
    pub fn get_ip_by_mac(&self, mac: &str) -> Option<IpAddr> {
        let mac_lower = mac.to_ascii_lowercase();
        self.devices
            .read()
            .unwrap()
            .iter()
            .find_map(|(ip, info)| {
                info.mac_address
                    .as_deref()
                    .filter(|m| m.to_ascii_lowercase() == mac_lower)
                    .map(|_| *ip)
            })
    }
    
    /// Returns the most recently detected (ip, mac) pair across all scans.
    pub fn get_latest(&self) -> Option<LatestDevice> {
        self.latest.read().unwrap().clone()
    }

    /// Returns the network base address (e.g., "192.168.1") used for scanning.
    pub fn network_base(&self) -> &str {
        &self.network_base
    }
}
