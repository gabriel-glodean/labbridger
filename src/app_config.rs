use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    pub scanner: ScannerSettings,
    #[serde(default)]
    pub relay: RelaySettings,
    #[serde(default)]
    pub users: Vec<UserConfig>,
}

fn default_token_ttl_seconds() -> u64 { 3600 }

#[derive(Debug, Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    /// How long (in seconds) a session token remains valid after it is issued.
    /// Defaults to 3600 (1 hour).
    #[serde(default = "default_token_ttl_seconds")]
    pub token_ttl_seconds: u64,
}

/// A user entry stored in config.yaml.
/// `password_hash` must be a bcrypt hash – generate one with:
///   cargo run --bin hash-password -- yourpassword
#[derive(Debug, Deserialize, Clone)]
pub struct UserConfig {
    pub username: String,
    pub password_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct ScannerSettings {
    /// First three octets of the /24 subnet to scan, e.g. "192.168.1"
    pub network_base: String,
    pub delay_seconds: u64,
}

fn default_probe_path() -> String { "/".to_string() }

/// Determines how a target should be probed for availability.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMethod {
    /// HTTP-based health check (GET request to probe_path).
    Http,
    /// ICMP ping-based check.
    Ping,
}

fn default_probe_method() -> ProbeMethod { ProbeMethod::Http }

fn default_ssh_port() -> u16 { 22 }
fn default_target_os() -> TargetOs { TargetOs::Linux }

/// Operating system of the remote target — determines the default shutdown
/// command when `command` is not explicitly set.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Linux,
    Windows,
}

impl TargetOs {
    /// Returns the default shutdown command for this OS.
    pub fn default_shutdown_command(&self) -> &'static str {
        match self {
            TargetOs::Linux   => "sudo poweroff",
            TargetOs::Windows => "shutdown /s /t 0",
        }
    }
}

/// SSH configuration for remote shutdown of a device.
///
/// Authentication: provide **either** `key_file` (or omit for the system
/// default key) **or** `password`.  When `password` is set the server uses
/// `sshpass` to feed it to OpenSSH (must be installed on the server).
///
/// YAML example (Linux – key-based, default):
/// ```yaml
/// shutdown_ssh:
///   username: "root"
///   key_file: "/root/.ssh/id_ed25519"
///   # os: linux             (default)
///   # port: 22              (default)
///   # command: "sudo poweroff"  (default for linux)
/// ```
///
/// YAML example (Windows – password-based):
/// ```yaml
/// shutdown_ssh:
///   username: "Administrator"
///   password: "s3cret"
///   os: windows
///   # port: 22              (default)
///   # command: "shutdown /s /t 0"  (default for windows)
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct SshShutdownConfig {
    /// Operating system of the remote target. Defaults to "linux".
    /// Determines the default shutdown command when `command` is omitted.
    #[serde(default = "default_target_os")]
    pub os: TargetOs,
    /// SSH port. Defaults to 22.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH username for authentication.
    pub username: String,
    /// Path to the SSH private key file for key-based authentication.
    /// If omitted, the system's default SSH key (~/.ssh/id_rsa etc.) is used.
    /// Ignored when `password` is set.
    #[serde(default)]
    pub key_file: Option<String>,
    /// SSH password for password-based authentication.
    /// When set, the server uses `sshpass` (must be installed) to provide the
    /// password to OpenSSH.  Mutually exclusive with `key_file`.
    #[serde(default)]
    pub password: Option<String>,
    /// Command to execute on the remote host.
    /// Defaults to "sudo poweroff" for linux or "shutdown /s /t 0" for windows.
    #[serde(default)]
    pub command: Option<String>,
}

/// A single relay target: a fixed URL, a fixed URL with a probe path,
/// or a MAC-address + port pair (IP resolved dynamically from the network scanner).
///
/// YAML examples:
///   shorthand static:  `other: "http://192.168.1.50:8000"`
///   static + probe:    `other: { url: "http://192.168.1.50:8000", probe_path: "/health" }`
///   by MAC:            `ollama: { mac: "aa:bb:cc:dd:ee:ff", port: 11434, probe_path: "/api/tags" }`
///   with Shelly:       `ollama: { mac: "aa:bb:cc:dd:ee:ff", port: 11434, shelly_power_mac: "11:22:33:44:55:66" }`
///   ping-based:        `device: { mac: "aa:bb:cc:dd:ee:ff", port: 22, probe_method: "ping" }`
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RelayTarget {
    /// Fixed URL shorthand – probing uses "/" by default.
    Static(String),
    /// MAC-based target: IP resolved from the network scanner at request time.
    /// Only probed after the MAC becomes visible on the network.
    /// Optionally can specify a Shelly smart plug MAC that controls power to this device.
    Mac {
        mac: String,
        port: u16,
        /// HTTP path used by the probing service to check readiness.
        /// Defaults to "/". Only used when probe_method is Http.
        #[serde(default = "default_probe_path")]
        probe_path: String,
        /// Optional: MAC address of a Shelly smart plug that controls power to this device.
        /// If specified, the /start/{target} endpoint will turn on this Shelly plug.
        #[serde(default)]
        shelly_power_mac: Option<String>,
        /// Optional: Enable Wake-on-LAN for this device.
        /// If true, the /start/{target} endpoint will send a WOL magic packet.
        /// Note: wol_enabled takes precedence over shelly_power_mac if both are set.
        #[serde(default)]
        wol_enabled: bool,
        /// Method used to probe this target (http or ping). Defaults to "http".
        #[serde(default = "default_probe_method")]
        probe_method: ProbeMethod,
        /// Optional SSH shutdown configuration. When set, POST /stop/{target}
        /// can send a shutdown command to the device via SSH.
        #[serde(default)]
        shutdown_ssh: Option<SshShutdownConfig>,
        /// Optional REST API shutdown path (e.g. "/api/shutdown"). When set,
        /// POST /stop/{target} sends a POST request to this path on the device.
        #[serde(default)]
        shutdown_api_path: Option<String>,
    },
    /// Static URL with an explicit probe path.
    StaticManaged {
        url: String,
        /// HTTP path used by the probing service to check readiness.
        /// Defaults to "/". Only used when probe_method is Http.
        #[serde(default = "default_probe_path")]
        probe_path: String,
        /// Method used to probe this target (http or ping). Defaults to "http".
        #[serde(default = "default_probe_method")]
        probe_method: ProbeMethod,
    },
}

/// Named relay targets: map of "label" -> RelayTarget
#[derive(Debug, Deserialize, Default)]
pub struct RelaySettings {
    #[serde(default)]
    pub targets: HashMap<String, RelayTarget>,
}

impl Settings {
    pub fn load() -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(config::File::with_name("config"))
            .build()?
            .try_deserialize()
    }
}
