use actix_web::{web, HttpResponse, Responder};
use serde::Serialize;
use std::time::Duration;

use crate::app_config::{RelayTarget, SshShutdownConfig};
use crate::network_scanner::NetworkScanner;
use crate::relay::RelayState;
use crate::shelly::ShellyPlugStoppable;
use crate::target_monitor::TargetMonitor;
use crate::target_stoppable::Stoppable;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ErrorResponse {
    status: &'static str,
    target: String,
    message: String,
}


// ── SshStoppable ─────────────────────────────────────────────────────────────

/// Sends a shutdown command to a remote host via SSH using the system's OpenSSH
/// client.  Supports key-based authentication (`BatchMode=yes`) and
/// password-based authentication (via `sshpass`).
pub struct SshStoppable {
    host: String,
    port: u16,
    username: String,
    key_file: Option<String>,
    password: Option<String>,
    command: String,
}

impl SshStoppable {
    pub fn from_config(host: String, config: &SshShutdownConfig) -> Self {
        let command = config
            .command
            .clone()
            .unwrap_or_else(|| config.os.default_shutdown_command().to_string());
        Self {
            host,
            port: config.port,
            username: config.username.clone(),
            key_file: config.key_file.clone(),
            password: config.password.clone(),
            command,
        }
    }
}

impl Stoppable for SshStoppable {
    async fn stop(&self) -> Result<(), String> {
        eprintln!(
            "[SshStoppable] Sending '{}' to {}@{}:{} (auth: {})",
            self.command, self.username, self.host, self.port,
            if self.password.is_some() { "password" } else { "key" }
        );

        // When a password is configured, wrap the ssh invocation with `sshpass`.
        let mut cmd = if self.password.is_some() {
            let mut c = tokio::process::Command::new("sshpass");
            c.arg("-e");          // read password from $SSHPASS env var
            c.arg("ssh");
            c
        } else {
            tokio::process::Command::new("ssh")
        };

        // Pass the password through the SSHPASS environment variable so it
        // never appears in the process argument list.
        if let Some(ref pw) = self.password {
            cmd.env("SSHPASS", pw);
        }

        cmd.args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ConnectTimeout=5",
        ]);

        if self.password.is_some() {
            // Allow keyboard-interactive / password auth
            cmd.args(["-o", "BatchMode=no"]);
            cmd.args(["-o", "PubkeyAuthentication=no"]);
        } else {
            cmd.args(["-o", "BatchMode=yes"]);
        }

        cmd.args(["-p", &self.port.to_string()]);

        if let Some(ref key_file) = self.key_file {
            if self.password.is_none() {
                cmd.args(["-i", key_file]);
            }
        }

        cmd.arg(format!("{}@{}", self.username, self.host));
        cmd.arg(&self.command);

        let output = cmd.output()
            .await
            .map_err(|e| {
                if self.password.is_some() {
                    format!(
                        "Failed to execute sshpass/ssh command (is sshpass installed?): {}", e
                    )
                } else {
                    format!("Failed to execute ssh command: {}", e)
                }
            })?;

        // Accept success (0) and SSH connection-closed (255) — the remote host
        // may drop the TCP connection as it shuts down.
        if output.status.success() || output.status.code() == Some(255) {
            eprintln!("[SshStoppable] Shutdown command sent successfully to {}", self.host);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "SSH command exited with {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ))
        }
    }
}

// ── RestApiStoppable ─────────────────────────────────────────────────────────

/// Sends an HTTP POST request to a device's shutdown API endpoint.
pub struct RestApiStoppable {
    url: String,
    client: reqwest::Client,
}

impl RestApiStoppable {
    pub fn new(host: &str, port: u16, api_path: &str, client: reqwest::Client) -> Self {
        let path = api_path.trim_start_matches('/');
        Self {
            url: format!("http://{}:{}/{}", host, port, path),
            client,
        }
    }
}

impl Stoppable for RestApiStoppable {
    async fn stop(&self) -> Result<(), String> {
        eprintln!("[RestApiStoppable] POST {}", self.url);

        match self.client.post(&self.url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                eprintln!("[RestApiStoppable] Shutdown request accepted by {}", self.url);
                Ok(())
            }
            Ok(response) => Err(format!(
                "Shutdown API responded with status: {}", response.status()
            )),
            Err(e) => Err(format!(
                "Failed to send shutdown request to {}: {}", self.url, e
            )),
        }
    }
}

// ── Stop strategy ────────────────────────────────────────────────────────────

/// The graceful-shutdown method to attempt *before* an optional Shelly plug
/// power-off.  Determined by pattern-matching the target configuration.
enum GracefulMethod {
    /// Send an SSH command to shut down the device.
    Ssh(SshShutdownConfig),
    /// POST to a REST API shutdown endpoint on the device.
    RestApi(String),
    /// No graceful shutdown — go straight to Shelly plug off (brute force).
    None,
}

// ── run_stop_sequence ────────────────────────────────────────────────────────

/// Executes the stop sequence for a MAC-based target.
///
///   1. Resolve the device IP from the network scanner.
///   2. Attempt the graceful shutdown method (SSH *or* REST API), if any.
///   3. If `plug_off_mac` is `Some`, wait for the device to go offline (up to
///      5 min) then turn off the Shelly plug.
///
/// Errors in any step are logged but do **not** abort the remaining steps.
async fn run_stop_sequence(
    target_name: String,
    target_mac: String,
    target_port: u16,
    graceful: GracefulMethod,
    plug_off_mac: Option<String>,
    scanner: NetworkScanner,
    client: reqwest::Client,
) {
    eprintln!("[stop] Starting stop sequence for '{}'", target_name);

    let device_ip = scanner.get_ip_by_mac(&target_mac).map(|ip| ip.to_string());

    // ── Step 1: graceful shutdown ────────────────────────────────────────
    match (&graceful, &device_ip) {
        (GracefulMethod::Ssh(ssh_config), Some(ip)) => {
            let ssh = SshStoppable::from_config(ip.clone(), ssh_config);
            if let Err(e) = ssh.stop().await {
                eprintln!("[stop] SSH shutdown failed for '{}': {}", target_name, e);
            }
        }
        (GracefulMethod::RestApi(api_path), Some(ip)) => {
            let rest = RestApiStoppable::new(ip, target_port, api_path, client.clone());
            if let Err(e) = rest.stop().await {
                eprintln!("[stop] REST shutdown failed for '{}': {}", target_name, e);
            }
        }
        (GracefulMethod::None, _) => {
            eprintln!("[stop] No graceful shutdown for '{}' — skipping to plug off", target_name);
        }
        (_, None) => {
            eprintln!(
                "[stop] Device '{}' (MAC {}) not on network — skipping graceful shutdown",
                target_name, target_mac
            );
        }
    }

    // ── Step 2 & 3: wait for offline + Shelly plug off ──────────────────
    if let Some(ref plug_mac) = plug_off_mac {
        // Only wait for the device to go offline when a graceful shutdown was
        // actually attempted (i.e. not brute-force) and the device was online.
        if !matches!(graceful, GracefulMethod::None) && device_ip.is_some() {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
            loop {
                if scanner.get_ip_by_mac(&target_mac).is_none() {
                    eprintln!("[stop] Device '{}' is now offline", target_name);
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    eprintln!(
                        "[stop] Timeout waiting for '{}' to go offline — turning off plug anyway",
                        target_name
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }

        let plug = ShellyPlugStoppable::new(plug_mac.clone(), scanner.clone(), client.clone());
        if let Err(e) = plug.stop().await {
            eprintln!("[stop] Failed to turn off Shelly plug for '{}': {}", target_name, e);
        }
    }

    eprintln!("[stop] Stop sequence complete for '{}'", target_name);
}

// ── HTTP handler ─────────────────────────────────────────────────────────────

/// HTTP handler for `POST /stop/{target}`
///
/// Initiates a stop/shutdown sequence for the named target.  The operation is
/// **fire-and-forget**: the endpoint spawns a background task and immediately
/// returns **204 No Content**.
///
/// The handler pattern-matches the target configuration to determine:
///   - **Graceful method** — SSH (highest priority), REST API, or none
///   - **Plug off** — whether to turn off the Shelly smart plug afterward
pub async fn stop_target_handler(
    target_name: web::Path<String>,
    relay_state: web::Data<RelayState>,
    monitor: web::Data<TargetMonitor>,
) -> impl Responder {
    let target_name = target_name.into_inner();
    eprintln!("[stop] Request to stop target: {}", target_name);

    // Look up the target in the relay configuration
    let target = match relay_state.targets.get(&target_name) {
        Some(t) => t,
        None => {
            eprintln!("[stop] Target '{}' not found in configuration", target_name);
            return HttpResponse::NotFound().json(ErrorResponse {
                status: "error",
                target: target_name,
                message: "Unknown target".to_string(),
            });
        }
    };

    match target {
        RelayTarget::Mac {
            mac,
            port,
            shelly_power_mac,
            shutdown_ssh,
            shutdown_api_path,
            shutdown_plug_off,
            ..
        } => {
            // ── Determine graceful method (SSH takes precedence) ──────
            let graceful = match (shutdown_ssh, shutdown_api_path) {
                (Some(ssh), _)    => GracefulMethod::Ssh(ssh.clone()),
                (None, Some(api)) => GracefulMethod::RestApi(api.clone()),
                (None, None)      => GracefulMethod::None,
            };

            // ── Determine plug-off MAC ───────────────────────────────
            let plug_off_mac = match (shutdown_plug_off, shelly_power_mac) {
                (true, Some(plug)) => Some(plug.clone()),
                _                  => None,
            };

            // ── Reject if nothing is configured ──────────────────────
            if matches!(graceful, GracefulMethod::None) && plug_off_mac.is_none() {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    status: "error",
                    target: target_name,
                    message: "Target has no stop methods configured \
                              (need shutdown_ssh, shutdown_api_path, or \
                              shelly_power_mac + shutdown_plug_off)"
                        .to_string(),
                });
            }

            // ── Spawn fire-and-forget background task ────────────────
            let mac = mac.clone();
            let port = *port;
            let scanner = monitor.scanner().clone();
            let client = monitor.client().clone();

            tokio::spawn(async move {
                run_stop_sequence(
                    target_name, mac, port, graceful, plug_off_mac, scanner, client,
                ).await;
            });

            HttpResponse::NoContent().finish() // 204
        }

        RelayTarget::Static(_) | RelayTarget::StaticManaged { .. } => {
            HttpResponse::BadRequest().json(ErrorResponse {
                status: "error",
                target: target_name,
                message: "Static targets do not support remote stop".to_string(),
            })
        }
    }
}

