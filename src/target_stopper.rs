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

// ── CompositeStoppable ───────────────────────────────────────────────────────

/// Composed stoppable that orchestrates a full graceful-shutdown sequence:
///
///   1. **Graceful shutdown** — send an SSH *or* REST command to the device
///      (SSH takes precedence when both are configured).
///   2. **Wait for offline** — poll the network scanner until the device's MAC
///      disappears (up to 5 minutes, checked every 10 s).
///   3. **Shelly plug off** — turn off the smart plug so the device is fully
///      powered down.
///
/// All three steps are optional — only the parts that have configuration will
/// run.  Errors in any step are logged but do **not** abort the remaining steps.
pub struct CompositeStoppable {
    /// Human-readable target name (for log messages).
    target_name: String,
    /// MAC address of the device to stop.
    target_mac: String,
    /// Service port of the target (used to build the REST shutdown URL).
    target_port: u16,
    /// MAC of the Shelly plug that powers the device (if any).
    shelly_mac: Option<String>,
    /// SSH shutdown configuration (if any).
    shutdown_ssh: Option<SshShutdownConfig>,
    /// REST API shutdown path on the device (if any).
    shutdown_api_path: Option<String>,
    /// Network scanner used to resolve MACs → IPs and detect offline state.
    scanner: NetworkScanner,
    /// HTTP client shared with the rest of the application.
    client: reqwest::Client,
}

impl CompositeStoppable {
    pub fn new(
        target_name: String,
        target_mac: String,
        target_port: u16,
        shelly_mac: Option<String>,
        shutdown_ssh: Option<SshShutdownConfig>,
        shutdown_api_path: Option<String>,
        scanner: NetworkScanner,
        client: reqwest::Client,
    ) -> Self {
        Self {
            target_name,
            target_mac,
            target_port,
            shelly_mac,
            shutdown_ssh,
            shutdown_api_path,
            scanner,
            client,
        }
    }
}

impl Stoppable for CompositeStoppable {
    async fn stop(&self) -> Result<(), String> {
        eprintln!("[CompositeStoppable] Starting stop sequence for '{}'", self.target_name);

        // ── Step 1: graceful shutdown (SSH takes precedence over REST) ────
        let device_ip = self.scanner.get_ip_by_mac(&self.target_mac).map(|ip| ip.to_string());

        if let Some(ref ip) = device_ip {
            if let Some(ref ssh_config) = self.shutdown_ssh {
                let ssh = SshStoppable::from_config(ip.clone(), ssh_config);
                if let Err(e) = ssh.stop().await {
                    eprintln!("[CompositeStoppable] SSH shutdown failed for '{}': {}", self.target_name, e);
                }
            } else if let Some(ref api_path) = self.shutdown_api_path {
                let rest = RestApiStoppable::new(ip, self.target_port, api_path, self.client.clone());
                if let Err(e) = rest.stop().await {
                    eprintln!("[CompositeStoppable] REST shutdown failed for '{}': {}", self.target_name, e);
                }
            }
        } else {
            eprintln!(
                "[CompositeStoppable] Device '{}' (MAC {}) not on network — skipping graceful shutdown",
                self.target_name, self.target_mac
            );
        }

        // ── Step 2: wait for the device to go offline ────────────────────
        if self.shelly_mac.is_some() && device_ip.is_some() {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(300);

            loop {
                if self.scanner.get_ip_by_mac(&self.target_mac).is_none() {
                    eprintln!("[CompositeStoppable] Device '{}' is now offline", self.target_name);
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    eprintln!(
                        "[CompositeStoppable] Timeout waiting for '{}' to go offline — turning off plug anyway",
                        self.target_name
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }

        // ── Step 3: turn off the Shelly plug ─────────────────────────────
        if let Some(ref plug_mac) = self.shelly_mac {
            let plug = ShellyPlugStoppable::new(plug_mac.clone(), self.scanner.clone(), self.client.clone());
            if let Err(e) = plug.stop().await {
                eprintln!("[CompositeStoppable] Failed to turn off Shelly plug for '{}': {}", self.target_name, e);
            }
        }

        eprintln!("[CompositeStoppable] Stop sequence complete for '{}'", self.target_name);
        Ok(())
    }
}

// ── HTTP handler ─────────────────────────────────────────────────────────────

/// HTTP handler for `POST /stop/{target}`
///
/// Initiates a stop/shutdown sequence for the named target.  The operation is
/// **fire-and-forget**: the endpoint spawns a background task and immediately
/// returns **204 No Content**.
///
/// Depending on configuration the background task will:
///   - Send a graceful shutdown via SSH or REST API (if configured)
///   - Wait for the device to disappear from the network (if a Shelly plug is
///     also configured)
///   - Turn off the Shelly smart plug
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
            ..
        } => {
            let has_shelly = shelly_power_mac.is_some();
            let has_ssh = shutdown_ssh.is_some();
            let has_rest = shutdown_api_path.is_some();

            if !has_shelly && !has_ssh && !has_rest {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    status: "error",
                    target: target_name,
                    message: "Target has no stop methods configured \
                              (need shelly_power_mac, shutdown_ssh, or shutdown_api_path)"
                        .to_string(),
                });
            }

            // Spawn fire-and-forget background task
            let stopper = CompositeStoppable::new(
                target_name.clone(),
                mac.clone(),
                *port,
                shelly_power_mac.clone(),
                shutdown_ssh.clone(),
                shutdown_api_path.clone(),
                monitor.scanner().clone(),
                monitor.client().clone(),
            );

            tokio::spawn(async move {
                if let Err(e) = stopper.stop().await {
                    eprintln!("[stop] Background stop failed for '{}': {}", target_name, e);
                }
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

