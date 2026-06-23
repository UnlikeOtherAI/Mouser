//! Reconnect supervision for a source/controller session after transport loss.

use std::net::SocketAddr;
use std::time::Duration;

use mouser_core::DeviceId;
use mouser_net::{DeviceIdentity, InteractiveConnection, InteractiveEndpoint, PinPolicy};

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::discovery::{self, PeerRegistry};

use super::ipc_bridge::IpcBridge;

const BASE_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const JITTER_PERCENT: u64 = 20;

pub(super) enum ReconnectEnd {
    Reconnected(Box<InteractiveConnection>),
    Disconnected,
    Shutdown,
}

/// Capped exponential backoff schedule before jitter is applied.
#[must_use]
pub(super) fn reconnect_backoff(attempt: u32) -> Duration {
    let shift = attempt.min(16);
    let factor = 1u128.checked_shl(shift).unwrap_or(u128::MAX);
    let millis = BASE_BACKOFF.as_millis().saturating_mul(factor);
    let capped = millis.min(MAX_BACKOFF.as_millis());
    let capped_u64 = u64::try_from(capped).unwrap_or(u64::MAX);
    Duration::from_millis(capped_u64)
}

fn reconnect_delay(attempt: u32, peer_id: &DeviceId) -> Duration {
    let base = reconnect_backoff(attempt);
    let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let span = JITTER_PERCENT.saturating_mul(2).saturating_add(1);
    let jitter = stable_jitter(peer_id, attempt) % span;
    let pct = 100u64.saturating_sub(JITTER_PERCENT).saturating_add(jitter);
    let jittered = base_ms.saturating_mul(pct) / 100;
    Duration::from_millis(jittered.min(max_backoff_ms()))
}

fn max_backoff_ms() -> u64 {
    u64::try_from(MAX_BACKOFF.as_millis()).unwrap_or(u64::MAX)
}

fn stable_jitter(peer_id: &DeviceId, attempt: u32) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in peer_id {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for byte in attempt.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(super) async fn redial_until_reconnected(
    store: &DaemonStore,
    endpoint: &InteractiveEndpoint,
    me: &DeviceIdentity,
    registry: &PeerRegistry,
    peer_id: DeviceId,
    bridge: Option<&IpcBridge>,
) -> ReconnectEnd {
    let peer_text = format_device_id(&peer_id);
    if let Some(bridge) = bridge {
        bridge.set_connecting(&peer_text);
    }

    let mut attempt = 0u32;
    loop {
        match store.is_peer_trusted(&peer_id) {
            Ok(true) => {}
            Ok(false) => {
                let reason = format!("peer {peer_text} is no longer trusted");
                if let Some(bridge) = bridge {
                    bridge.set_connect_error(&reason);
                }
                eprintln!("mouserd: reconnect stopped: {reason}");
                return ReconnectEnd::Disconnected;
            }
            Err(e) => {
                let reason = format!("trust check failed for {peer_text}: {e}");
                if let Some(bridge) = bridge {
                    bridge.set_connect_error(&reason);
                }
                eprintln!("mouserd: reconnect stopped: {reason}");
                return ReconnectEnd::Disconnected;
            }
        }

        let addrs = match wait_for_peer_addr(registry, &peer_id, bridge).await {
            ResolveEnd::Addrs(addrs) => addrs,
            ResolveEnd::Disconnected => return ReconnectEnd::Disconnected,
            ResolveEnd::Shutdown => return ReconnectEnd::Shutdown,
        };
        eprintln!(
            "mouserd: redialing {peer_text} ({} candidate address(es))",
            addrs.len()
        );
        match endpoint
            .connect_interactive_any(me, &addrs, PinPolicy::Pinned(peer_id))
            .await
        {
            Ok(conn) => return ReconnectEnd::Reconnected(Box::new(conn)),
            Err(e) => {
                let reason = format!("reconnect to {peer_text} failed: {e}");
                if let Some(bridge) = bridge {
                    bridge.set_connect_error(&reason);
                    bridge.set_connecting(&peer_text);
                }
                eprintln!("mouserd: {reason}");
            }
        }

        let delay = reconnect_delay(attempt, &peer_id);
        attempt = attempt.saturating_add(1);
        match wait_delay_or_stop(delay, bridge).await {
            Stop::Continue => {}
            Stop::Disconnected => return ReconnectEnd::Disconnected,
            Stop::Shutdown => return ReconnectEnd::Shutdown,
        }
    }
}

enum ResolveEnd {
    Addrs(Vec<SocketAddr>),
    Disconnected,
    Shutdown,
}

async fn wait_for_peer_addr(
    registry: &PeerRegistry,
    peer_id: &DeviceId,
    bridge: Option<&IpcBridge>,
) -> ResolveEnd {
    let mut changes = registry.subscribe();
    loop {
        let addrs = registry
            .find(peer_id)
            .map(|p| discovery::peer_socket_addrs(&p))
            .unwrap_or_default();
        if !addrs.is_empty() {
            return ResolveEnd::Addrs(addrs);
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return ResolveEnd::Shutdown,
            _ = wait_for_disconnect(bridge) => return ResolveEnd::Disconnected,
            changed = changes.changed() => {
                if changed.is_err() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
}

enum Stop {
    Continue,
    Disconnected,
    Shutdown,
}

async fn wait_delay_or_stop(delay: Duration, bridge: Option<&IpcBridge>) -> Stop {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Stop::Shutdown,
        _ = wait_for_disconnect(bridge) => Stop::Disconnected,
        _ = tokio::time::sleep(delay) => Stop::Continue,
    }
}

async fn wait_for_disconnect(bridge: Option<&IpcBridge>) {
    match bridge {
        Some(bridge) => bridge.next_disconnect_request().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_is_capped_exponential() {
        let mut prev = Duration::ZERO;
        for attempt in 0..20 {
            let delay = reconnect_backoff(attempt);
            assert!(delay >= prev);
            assert!(delay <= MAX_BACKOFF);
            prev = delay;
        }
        assert_eq!(reconnect_backoff(0), Duration::from_millis(250));
        assert_eq!(reconnect_backoff(1), Duration::from_millis(500));
        assert_eq!(reconnect_backoff(7), Duration::from_secs(30));
        assert_eq!(reconnect_backoff(31), Duration::from_secs(30));
    }

    #[test]
    fn reconnect_jitter_stays_within_bounds() {
        let peer = [7u8; 32];
        for attempt in 0..12 {
            let base = reconnect_backoff(attempt);
            let delay = reconnect_delay(attempt, &peer);
            let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
            let low = base_ms.saturating_mul(80) / 100;
            assert!(delay >= Duration::from_millis(low));
            assert!(delay <= MAX_BACKOFF);
        }
    }
}
