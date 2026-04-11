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

/// A single relay target: either a fixed URL or a MAC-address + port pair
/// (the IP is resolved dynamically from the network scanner at request time).
///
/// YAML examples:
///   static:  `ollama: "http://192.168.1.100:11434"`
///   by MAC:  `ollama: { mac: "aa:bb:cc:dd:ee:ff", port: 11434 }`
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RelayTarget {
    /// Fixed URL – used as-is.
    Static(String),
    /// Resolved at request time: look up the current IP for this MAC.
    Mac { mac: String, port: u16 },
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

