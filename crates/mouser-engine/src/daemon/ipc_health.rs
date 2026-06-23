//! IPC snapshot health diagnostics.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use mouser_ipc::{HealthItemDto, HealthSeverity, PeerDto};

use crate::discovery;

/// How long discovery may find nothing before surfacing the "advertising but no peers"
/// hint, long enough not to flap during the normal startup browse.
const ZERO_PEERS_GRACE: Duration = Duration::from_secs(12);

/// Derive connectivity health items (spec §9) from the current engine state. These are
/// the platform-agnostic checks; richer per-OS probes can append OS-specific findings.
pub(super) fn build_diagnostics(started: Instant, peers: &[PeerDto]) -> Vec<HealthItemDto> {
    let mut items = Vec::new();

    let local_ok = match discovery::local_ipv4() {
        Some(IpAddr::V4(v4)) => !v4.is_link_local() && !v4.is_unspecified(),
        Some(IpAddr::V6(_)) => true,
        None => false,
    };
    if !local_ok {
        items.push(HealthItemDto {
            code: "no_network_address".to_string(),
            severity: HealthSeverity::Error,
            title: "No network connection".to_string(),
            detail: "This computer doesn't have a usable network address - it isn't \
                getting an IP from the router. Connect to Wi-Fi or Ethernet so Mouser can \
                find your other devices."
                .to_string(),
            remediation: Some("open_network_settings".to_string()),
        });
    } else if peers.is_empty() && started.elapsed() > ZERO_PEERS_GRACE {
        items.push(HealthItemDto {
            code: "advertising_zero_peers".to_string(),
            severity: HealthSeverity::Warning,
            title: "No other devices found".to_string(),
            detail: "Mouser is advertising on this network but hasn't discovered any \
                other devices. Make sure another device is running Mouser on the same \
                network, and that a firewall isn't blocking it."
                .to_string(),
            remediation: Some("check_firewall".to_string()),
        });
    }

    items
}
