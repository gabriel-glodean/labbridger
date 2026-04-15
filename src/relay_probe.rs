use crate::app_config::{ProbeMethod, RelayTarget};
use crate::network_scanner::NetworkScanner;
use crate::target_probeable::Probeable;
use crate::target_status::{TargetInfo, TargetStatus};
use std::net::IpAddr;
use std::time::Duration;
use surge_ping::{Client, Config, PingIdentifier, PingSequence};
use tokio::time::timeout;

// ── RelayTargetProbe – concrete Probeable ─────────────────────────────────────

/// A per-target probe handle. Borrows the monitor's shared client, scanner,
/// and target config to issue a live readiness check without side-effects.
pub struct RelayTargetProbe<'a> {
    target: &'a RelayTarget,
    scanner: &'a NetworkScanner,
    client: &'a reqwest::Client,
}

impl<'a> RelayTargetProbe<'a> {
    /// Create a new probe for the given target configuration.
    pub fn new(
        target: &'a RelayTarget,
        scanner: &'a NetworkScanner,
        client: &'a reqwest::Client,
    ) -> Self {
        Self {
            target,
            scanner,
            client,
        }
    }
}

impl<'a> Probeable for RelayTargetProbe<'a> {
    async fn probe(&self) -> TargetStatus {
        probe_target_impl(self.client, self.target, self.scanner)
            .await
            .status
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Convenience function that creates a probe and returns full TargetInfo.
/// This is the main entry point for probing a target.
pub async fn probe_target(
    client: &reqwest::Client,
    target: &RelayTarget,
    scanner: &NetworkScanner,
) -> TargetInfo {
    let probe = RelayTargetProbe::new(target, scanner, client);
    let status = probe.probe().await;

    // Reconstruct TargetInfo with IP if it's a MAC-based target
    let ip = match target {
        RelayTarget::Mac { mac, .. } => scanner.get_ip_by_mac(mac),
        _ => None,
    };

    TargetInfo { status, ip }
}

// ── Probe logic (internal) ────────────────────────────────────────────────────

/// Issue a single ICMP ping to the given IP address.
/// Returns `true` if the host responds within 2 seconds.
async fn ping_probe(ip: IpAddr) -> bool {
    let client = match Client::new(&Config::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to create ping client for probe: {}", e);
            return false;
        }
    };

    // Use a fixed identifier for probes
    let mut pinger = client.pinger(ip, PingIdentifier(1)).await;

    timeout(
        Duration::from_secs(2),
        pinger.ping(PingSequence(0), &[0; 8]),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some()
}

/// Issue a single GET to `url`; returns `true` when the response status < 500.
async fn http_probe(client: &reqwest::Client, url: &str) -> bool {
    client
        .get(url)
        .send()
        .await
        .map(|r| r.status().as_u16() < 500)
        .unwrap_or(false)
}

/// Build a probe URL from a base and a path, normalising slashes.
fn probe_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Determine the current [`TargetInfo`] for a single target.
///
/// MAC-based targets are **only** probed via HTTP once their MAC is visible in
/// the network scanner. Until then the status stays `Offline` without ever
/// touching the network.
async fn probe_target_impl(
    client: &reqwest::Client,
    target: &RelayTarget,
    scanner: &NetworkScanner,
) -> TargetInfo {
    match target {
        // ── Static shorthand: probe root of the URL (HTTP only) ──────────────
        RelayTarget::Static(url) => {
            let up = http_probe(client, &probe_url(url, "/")).await;
            TargetInfo {
                status: if up {
                    TargetStatus::Online
                } else {
                    TargetStatus::Offline
                },
                ip: None,
            }
        }

        // ── Static URL with explicit probe path and method ────────────────────
        RelayTarget::StaticManaged { url, probe_path, probe_method } => {
            let up = match probe_method {
                ProbeMethod::Http => http_probe(client, &probe_url(url, probe_path)).await,
                ProbeMethod::Ping => {
                    // Extract IP from URL for ping probing
                    if let Ok(parsed_url) = url.parse::<reqwest::Url>() {
                        if let Some(host) = parsed_url.host_str() {
                            // Try to parse as IP, or resolve if it's a hostname
                            if let Ok(ip) = host.parse::<IpAddr>() {
                                ping_probe(ip).await
                            } else {
                                // Hostname - try to resolve (basic attempt)
                                eprintln!("Warning: ping probe for hostname '{}' requires IP address", host);
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
            };
            TargetInfo {
                status: if up {
                    TargetStatus::Online
                } else {
                    TargetStatus::Offline
                },
                ip: None,
            }
        }

        // ── MAC target: check network presence, then probe ────────────────────
        RelayTarget::Mac {
            mac,
            port,
            probe_path,
            probe_method,
            ..  // Ignore shelly_power_mac (only used for starting, not probing)
        } => match scanner.get_ip_by_mac(mac) {
            // MAC not on the network yet → Offline, skip probe entirely
            None => TargetInfo {
                status: TargetStatus::Offline,
                ip: None,
            },
            // MAC visible → probe using configured method
            Some(ip) => {
                let up = match probe_method {
                    ProbeMethod::Http => {
                        let base = format!("http://{}:{}", ip, port);
                        http_probe(client, &probe_url(&base, probe_path)).await
                    }
                    ProbeMethod::Ping => ping_probe(ip).await,
                };
                TargetInfo {
                    status: if up {
                        TargetStatus::Online
                    } else {
                        TargetStatus::Starting
                    },
                    ip: Some(ip),
                }
            }
        },
    }
}
