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
pub(super) fn build_diagnostics(
    started: Instant,
    peers: &[PeerDto],
    connection_error: Option<&str>,
) -> Vec<HealthItemDto> {
    let mut items = Vec::new();

    if let Some(item) = connection_error_diagnostic(connection_error) {
        items.push(item);
    }

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

fn connection_error_diagnostic(error: Option<&str>) -> Option<HealthItemDto> {
    let error = error?;
    let lower = error.to_ascii_lowercase();
    if lower.contains("not trusted") || lower.contains("untrusted peer") {
        return Some(health_item(
            "peer_untrusted",
            "Peer is not paired",
            "Mouser refused this connection because the peer is not trusted on this computer. Pair the peer on both devices before connecting.",
            None,
        ));
    }
    if lower.contains("applicationverificationfailure")
        || lower.contains("application verification failure")
        || lower.contains("peer identity changed")
        || (lower.contains("certificate") && lower.contains("pin"))
    {
        return Some(health_item(
            "peer_identity_changed",
            "Peer identity changed",
            "The peer's certificate no longer matches the trusted identity pin. Confirm this is the same device, then forget and pair it again.",
            None,
        ));
    }
    if lower.contains("not currently discoverable")
        || lower.contains("live discovery registry")
        || lower.contains("no dialable address")
        || lower.contains("timed out dialing")
        || lower.contains("unreachable")
        || lower.contains("refused")
        || lower.contains("udp blocked")
    {
        return Some(health_item(
            "connect_unreachable",
            "Peer is unreachable",
            "Mouser found the peer but could not reach its connection address. Keep both devices awake on the same network and check firewall settings.",
            Some("check_firewall"),
        ));
    }
    None
}

fn health_item(code: &str, title: &str, detail: &str, remediation: Option<&str>) -> HealthItemDto {
    HealthItemDto {
        code: code.to_string(),
        severity: HealthSeverity::Error,
        title: title.to_string(),
        detail: detail.to_string(),
        remediation: remediation.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::connection_error_diagnostic;

    #[test]
    fn classifies_peer_identity_changed() {
        let item = connection_error_diagnostic(Some(
            "connect: invalid peer certificate: ApplicationVerificationFailure",
        ))
        .expect("diagnostic");

        assert_eq!(item.code, "peer_identity_changed");
    }

    #[test]
    fn classifies_connect_unreachable() {
        let item =
            connection_error_diagnostic(Some("connect: timed out dialing 192.168.1.50:49200"))
                .expect("diagnostic");

        assert_eq!(item.code, "connect_unreachable");
        assert_eq!(item.remediation.as_deref(), Some("check_firewall"));
    }
}
