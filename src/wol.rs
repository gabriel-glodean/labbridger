use crate::network_scanner::NetworkScanner;
use crate::target_startable::Startable;

// ── WolStartable – concrete Startable for Wake-on-LAN ──────────────────────────

/// A startable handle for Wake-on-LAN (WOL) compatible devices.
/// Sends a magic packet to wake up a device via its MAC address.
/// The device must support Wake-on-LAN and have it enabled in BIOS/firmware.
pub struct WolStartable<'a> {
    mac: &'a str,
    scanner: &'a NetworkScanner,
    broadcast_addr: String,
}

impl<'a> WolStartable<'a> {
    /// Create a new WOL startable handle.
    ///
    /// # Arguments
    /// * `mac` - MAC address of the device to wake (case-insensitive, format: "aa:bb:cc:dd:ee:ff")
    /// * `scanner` - Network scanner to check if device is already online
    /// * `network_base` - Network base address (e.g., "192.168.1") for determining broadcast address
    pub fn new(mac: &'a str, scanner: &'a NetworkScanner, network_base: &str) -> Self {
        // Create broadcast address from network base (e.g., "192.168.1" -> "192.168.1.255")
        let broadcast_addr = format!("{}.255:9", network_base);
        Self { mac, scanner, broadcast_addr }
    }
}

impl<'a> Startable for WolStartable<'a> {
    async fn start(&self) -> Result<(), String> {
        // First, check if the device is already online
        if self.scanner.get_ip_by_mac(self.mac).is_some() {
            eprintln!("[WolStartable] Target device {} is already online, no action needed", self.mac);
            return Ok(());
        }

        // Parse the MAC address
        let mac_bytes = parse_mac_address(self.mac)
            .map_err(|e| format!("Invalid MAC address '{}': {}", self.mac, e))?;

        // Create the magic packet
        let magic_packet = wake_on_lan::MagicPacket::new(&mac_bytes);

        // Send the magic packet
        eprintln!("[WolStartable] Sending Wake-on-LAN magic packet to {} via {}", self.mac, self.broadcast_addr);

        // Use string format for both addresses (to_addr and from_addr)
        match magic_packet.send_to(
            self.broadcast_addr.as_str(),
            "0.0.0.0:0"
        ) {
            Ok(_) => {
                eprintln!("[WolStartable] Successfully sent WOL packet to {}", self.mac);
                Ok(())
            }
            Err(e) => {
                Err(format!("Failed to send WOL packet to {}: {}", self.mac, e))
            }
        }
    }
}

/// Parse a MAC address string into a 6-byte array.
/// Accepts formats: "aa:bb:cc:dd:ee:ff", "aa-bb-cc-dd-ee-ff", "aabbccddeeff"
fn parse_mac_address(mac: &str) -> Result<[u8; 6], String> {
    // Remove common separators
    let cleaned = mac.replace([':', '-'], "");

    if cleaned.len() != 12 {
        return Err(format!("MAC address must be 12 hex characters, got {}", cleaned.len()));
    }

    let mut bytes = [0u8; 6];
    for i in 0..6 {
        let hex_pair = &cleaned[i * 2..i * 2 + 2];
        bytes[i] = u8::from_str_radix(hex_pair, 16)
            .map_err(|_| format!("Invalid hex in MAC address: {}", hex_pair))?;
    }

    Ok(bytes)
}

