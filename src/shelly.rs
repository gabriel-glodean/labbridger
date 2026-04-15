use std::time::Duration;
use serde::Deserialize;

use crate::network_scanner::NetworkScanner;
use crate::target_startable::Startable;
use crate::target_stoppable::Stoppable;

// ── Shelly Gen2 API response types ────────────────────────────────────────────

/// Response from Shelly Gen2 Switch.GetStatus RPC call
#[derive(Debug, Deserialize)]
struct ShellyGen2SwitchStatus {
    output: bool,
}

// ── ShellyStartable – concrete Startable for Shelly smart plugs ───────────────

/// A startable handle for Shelly smart plug devices.
/// Attempts to turn on a Shelly plug by sending an HTTP command to its local API.
/// The device must be reachable on the network (MAC resolved to IP) to be started.
pub struct ShellyStartable<'a> {
    mac: &'a str,
    target_mac: &'a str,
    scanner: &'a NetworkScanner,
    client: &'a reqwest::Client,
}

impl<'a> ShellyStartable<'a> {
    /// Create a new Shelly startable handle.
    ///
    /// # Arguments
    /// * `mac` - MAC address of the Shelly device (case-insensitive)
    /// * `target_mac` - MAC address of the device powered by this Shelly plug
    /// * `scanner` - Network scanner to resolve MAC to IP
    /// * `client` - HTTP client for sending commands
    pub fn new(mac: &'a str, target_mac: &'a str, scanner: &'a NetworkScanner, client: &'a reqwest::Client) -> Self {
        Self { mac, target_mac, scanner, client }
    }
}

impl<'a> Startable for ShellyStartable<'a> {
    async fn start(&self) -> Result<(), String> {
        // First, check if the guarded device is already online
        if self.scanner.get_ip_by_mac(self.target_mac).is_some() {
            eprintln!("[ShellyStartable] Target device {} is already online, no action needed", self.target_mac);
            return Ok(());
        }

        // Target device is offline, check if the Shelly plug is on the network
        let plug_ip = self.scanner.get_ip_by_mac(self.mac)
            .ok_or_else(|| format!("Shelly device with MAC {} not found on network", self.mac))?;

        // Check current plug status using Gen2 API: http://IP/rpc/Switch.GetStatus?id=0
        let status_url = format!("http://{}/rpc/Switch.GetStatus?id=0", plug_ip);

        let plug_is_on = match self.client.get(&status_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<ShellyGen2SwitchStatus>().await {
                    Ok(status) => status.output,
                    Err(e) => {
                        eprintln!("[ShellyStartable] Warning: Failed to parse plug status, assuming off: {}", e);
                        false
                    }
                }
            }
            _ => {
                eprintln!("[ShellyStartable] Warning: Failed to get plug status, assuming off");
                false
            }
        };

        // If plug is on but device is off, we need to power cycle
        if plug_is_on {
            eprintln!("[ShellyStartable] Plug is on but device is off, power cycling...");

            // Turn off the plug
            let off_url = format!("http://{}/rpc/Switch.Set?id=0&on=false", plug_ip);
            match self.client.get(&off_url)
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    eprintln!("[ShellyStartable] Turned off Shelly device at {}", plug_ip);
                }
                Ok(response) => {
                    return Err(format!(
                        "Failed to turn off Shelly device, status: {}",
                        response.status()
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to communicate with Shelly device while turning off: {}",
                        e
                    ));
                }
            }

            // Wait a moment before turning it back on (give device time to fully power down)
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        // Turn on the plug (either it was off, or we just turned it off)
        let on_url = format!("http://{}/rpc/Switch.Set?id=0&on=true", plug_ip);
        match self.client.get(&on_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                eprintln!("[ShellyStartable] Successfully turned on Shelly device at {}", plug_ip);
                Ok(())
            }
            Ok(response) => Err(format!(
                "Shelly device responded with error status: {}",
                response.status()
            )),
            Err(e) => Err(format!(
                "Failed to communicate with Shelly device at {}: {}",
                plug_ip, e
            )),
        }
    }
}

// ── ShellyPlugStoppable – concrete Stoppable for Shelly smart plugs ───────────

/// Turns off a Shelly smart plug by sending a `Switch.Set(on=false)` command
/// via its local Gen2 HTTP API.
pub struct ShellyPlugStoppable {
    shelly_mac: String,
    scanner: NetworkScanner,
    client: reqwest::Client,
}

impl ShellyPlugStoppable {
    pub fn new(shelly_mac: String, scanner: NetworkScanner, client: reqwest::Client) -> Self {
        Self { shelly_mac, scanner, client }
    }
}

impl Stoppable for ShellyPlugStoppable {
    async fn stop(&self) -> Result<(), String> {
        let plug_ip = self.scanner.get_ip_by_mac(&self.shelly_mac)
            .ok_or_else(|| format!("Shelly device with MAC {} not found on network", self.shelly_mac))?;

        let off_url = format!("http://{}/rpc/Switch.Set?id=0&on=false", plug_ip);
        match self.client.get(&off_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                eprintln!("[ShellyPlugStoppable] Turned off Shelly plug {} at {}", self.shelly_mac, plug_ip);
                Ok(())
            }
            Ok(response) => Err(format!(
                "Shelly plug responded with error status: {}", response.status()
            )),
            Err(e) => Err(format!(
                "Failed to communicate with Shelly plug at {}: {}", plug_ip, e
            )),
        }
    }
}
