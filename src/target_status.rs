use std::net::IpAddr;
use serde::Serialize;

// ── Status ────────────────────────────────────────────────────────────────────

/// Operational state of a single relay target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetStatus {
    /// Host not reachable: MAC absent from last network scan (MAC-based target),
    /// or the service URL is not responding (static target).
    Offline,

    /// Host is on the network (MAC resolved to an IP) but its HTTP service is
    /// not yet accepting requests. Applies to MAC-based targets only.
    Starting,

    /// Service endpoint is up and responding to HTTP probes.
    Online,
}

// ── TargetInfo ────────────────────────────────────────────────────────────────

/// Snapshot of a target's current status, bundled with its resolved IP (if any).
#[derive(Debug, Clone, Serialize)]
pub struct TargetInfo {
    pub status: TargetStatus,
    /// The IP resolved from the MAC at last probe (MAC-based targets only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
}

