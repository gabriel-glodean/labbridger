use actix_web::{web, HttpResponse, Responder};
use serde::Serialize;

use crate::app_config::RelayTarget;
use crate::relay::RelayState;
use crate::shelly::ShellyStartable;
use crate::target_monitor::TargetMonitor;
use crate::target_startable::Startable;
use crate::wol::WolStartable;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ErrorResponse {
    status: &'static str,
    target: String,
    message: String,
}

#[derive(Serialize)]
struct SuccessResponse {
    status: &'static str,
    target: String,
    #[serde(rename = "type")]
    starter_type: String,
    message: String,
}

#[derive(Serialize)]
struct DetailedErrorResponse {
    status: &'static str,
    target: String,
    #[serde(rename = "type")]
    starter_type: String,
    error: &'static str,
    message: String,
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Create a `ShellyStartable` handle for a Shelly smart plug with the given MAC address.
/// The returned handle can be used to send a turn-on command to the device.
///
/// This is a convenience function that extracts the scanner and client from the monitor.
fn create_shelly_startable<'a>(mac: &'a str, target_mac: &'a str, monitor: &'a TargetMonitor) -> ShellyStartable<'a> {
    ShellyStartable::new(mac, target_mac, monitor.scanner(), monitor.client())
}

// ── StarterType enum ──────────────────────────────────────────────────────────

/// Enum representing different types of startable implementations
enum StarterType<'a> {
    /// Shelly smart plug controlled via HTTP API
    Shelly(ShellyStartable<'a>),
    /// Wake-on-LAN magic packet
    WakeOnLan(WolStartable<'a>),
    // Future implementations can be added here:
    // HomeAssistant(HaStartable<'a>),
    // Custom(CustomStartable<'a>),
}

impl<'a> StarterType<'a> {
    /// Polymorphic start method that delegates to the appropriate implementation
    async fn start(&self) -> Result<(), String> {
        match self {
            StarterType::Shelly(shelly) => shelly.start().await,
            StarterType::WakeOnLan(wol) => wol.start().await,
            // Future cases:
            // StarterType::HomeAssistant(ha) => ha.start().await,
        }
    }

    /// Get a human-readable description of the starter type
    fn description(&self) -> &'static str {
        match self {
            StarterType::Shelly(_) => "Shelly smart plug",
            StarterType::WakeOnLan(_) => "Wake-on-LAN",
            // Future cases:
            // StarterType::HomeAssistant(_) => "Home Assistant",
        }
    }
}

/// Try to create a startable implementation for the given target configuration.
/// Returns None if the target type doesn't support starting.
fn create_starter<'a>(
    target: &'a RelayTarget,
    monitor: &'a TargetMonitor,
) -> Option<StarterType<'a>> {
    match target {
        // MAC-based targets with optional WOL or Shelly power control
        RelayTarget::Mac { mac, shelly_power_mac, wol_enabled, .. } => {
            // WOL takes precedence over Shelly if enabled
            if *wol_enabled {
                let network_base = monitor.scanner().network_base();
                Some(StarterType::WakeOnLan(WolStartable::new(mac, monitor.scanner(), network_base)))
            } else if let Some(plug_mac) = shelly_power_mac {
                // If a Shelly power MAC is specified, use it
                Some(StarterType::Shelly(create_shelly_startable(plug_mac, mac, monitor)))
            } else {
                // Otherwise, the target doesn't support remote start
                None
            }
        }
        // Static targets don't support starting (yet)
        RelayTarget::Static(_) | RelayTarget::StaticManaged { .. } => None,

        // Future: We could add more patterns here:
        // RelayTarget::HomeAssistant { entity_id, .. } => Some(StarterType::HomeAssistant(...)),
    }
}

/// HTTP handler for POST /start/{target}
///
/// Attempts to start the named target by:
/// 1. Looking up the target in the relay configuration
/// 2. Determining if the target type supports starting
/// 3. Creating the appropriate starter implementation
/// 4. Executing the start command
///
/// Returns appropriate HTTP status codes and JSON responses.
pub async fn start_target_handler(
    target_name: web::Path<String>,
    relay_state: web::Data<RelayState>,
    monitor: web::Data<TargetMonitor>,
) -> impl Responder {
    let target_name = target_name.into_inner();
    eprintln!("[start] Request to start target: {}", target_name);

    // Look up the target in the relay configuration
    let target = match relay_state.targets.get(&target_name) {
        Some(t) => t,
        None => {
            eprintln!("[start] Target '{}' not found in configuration", target_name);
            return HttpResponse::NotFound().json(ErrorResponse {
                status: "error",
                target: target_name.clone(),
                message: format!("Unknown target: '{}'", target_name),
            });
        }
    };

    // Try to create a starter for this target type
    let starter = match create_starter(target, &monitor) {
        Some(s) => s,
        None => {
            let reason = match target {
                RelayTarget::Static(url) =>
                    format!("Static target '{}' does not support remote start", url),
                RelayTarget::StaticManaged { url, .. } =>
                    format!("Static managed target '{}' does not support remote start", url),
                RelayTarget::Mac { shelly_power_mac: None, wol_enabled: false, .. } =>
                    "MAC-based target does not have 'wol_enabled: true' or 'shelly_power_mac' configured for remote start".to_string(),
                _ => "This target type does not support remote start".to_string(),
            };
            eprintln!("[start] Target '{}' is not startable: {}", target_name, reason);
            return HttpResponse::BadRequest().json(ErrorResponse {
                status: "error",
                target: target_name,
                message: reason,
            });
        }
    };

    // Log the starter type
    eprintln!(
        "[start] Target '{}' identified as: {}",
        target_name,
        starter.description()
    );

    // Attempt to start the target
    match starter.start().await {
        Ok(()) => {
            eprintln!("[start] Successfully started target '{}'", target_name);
            HttpResponse::Ok().json(SuccessResponse {
                status: "success",
                target: target_name.clone(),
                starter_type: starter.description().to_string(),
                message: format!("Target '{}' has been started", target_name),
            })
        }
        Err(e) if e.contains("not found on network") => {
            eprintln!("[start] Target '{}' not found on network: {}", target_name, e);
            HttpResponse::ServiceUnavailable().json(DetailedErrorResponse {
                status: "error",
                target: target_name.clone(),
                starter_type: starter.description().to_string(),
                error: "device_offline",
                message: e,
            })
        }
        Err(e) => {
            eprintln!("[start] Failed to start target '{}': {}", target_name, e);
            HttpResponse::InternalServerError().json(DetailedErrorResponse {
                status: "error",
                target: target_name,
                starter_type: starter.description().to_string(),
                error: "start_failed",
                message: e,
            })
        }
    }
}
