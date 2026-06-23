//! Inbound trust/pairing flow for accepted interactive connections.

use mouser_core::DeviceId;
use mouser_net::{InteractiveConnection, InteractiveEndpoint};
use mouser_protocol::TYPE_DEVICE_NAME;

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::discovery::PeerRegistry;

use super::ipc_bridge::IpcBridge;

/// Accept inbound connections until one is from a trusted (or just-approved) peer.
/// A trusted peer is returned immediately. An untrusted peer triggers an interactive
/// pairing approval over the UI; with no UI, it is rejected.
pub(super) async fn accept_trusted(
    endpoint: &InteractiveEndpoint,
    store: &DaemonStore,
    registry: &PeerRegistry,
    bridge: Option<&IpcBridge>,
) -> Result<InteractiveConnection, String> {
    loop {
        let conn = endpoint
            .accept_interactive()
            .await
            .map_err(|e| e.to_string())?;
        let Some(peer_id) = conn.peer_device_id() else {
            eprintln!("mouserd: rejected peer without a valid device_id");
            conn.close();
            continue;
        };
        if store.is_peer_trusted(&peer_id).map_err(|e| e.to_string())? {
            return Ok(conn);
        }

        match bridge {
            Some(bridge) if pair_via_ui(store, registry, bridge, &conn, &peer_id).await? => {
                return Ok(conn);
            }
            Some(_) => {}
            None => {
                let peer_text = format_device_id(&peer_id);
                eprintln!(
                    "mouserd: rejected untrusted peer {peer_text}; run \
                     `mouserd trust {peer_text}` to allow control (no UI to approve)"
                );
            }
        }
        conn.close();
    }
}

/// How long an inbound pairing request waits for the user's Approve/Deny before closing.
const PAIRING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Clears the pending pairing prompt when dropped.
struct PairingGuard<'a>(&'a IpcBridge);

impl Drop for PairingGuard<'_> {
    fn drop(&mut self) {
        self.0.clear_pairing();
    }
}

async fn pair_via_ui(
    store: &DaemonStore,
    registry: &PeerRegistry,
    bridge: &IpcBridge,
    conn: &InteractiveConnection,
    peer_id: &DeviceId,
) -> Result<bool, String> {
    let peer_b32 = format_device_id(peer_id);
    let name = recv_device_name(conn)
        .await
        .or_else(|| {
            registry
                .find(peer_id)
                .map(|p| p.name)
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| "A device".to_string());
    eprintln!("mouserd: pairing request from {name} ({peer_b32})");
    bridge.request_pairing(peer_b32.clone(), name);
    let _guard = PairingGuard(bridge);

    let approved = loop {
        match tokio::time::timeout(PAIRING_TIMEOUT, bridge.next_pairing_decision()).await {
            Ok(Some((id, decision))) if id == peer_b32 => break decision,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break false,
        }
    };
    if approved {
        store.trust_peer(*peer_id).map_err(|e| e.to_string())?;
        eprintln!("mouserd: paired with {peer_b32}");
    } else {
        eprintln!("mouserd: pairing with {peer_b32} not approved");
    }
    Ok(approved)
}

async fn recv_device_name(conn: &InteractiveConnection) -> Option<String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
        let (ty, payload) = tokio::time::timeout(remaining, conn.recv_control())
            .await
            .ok()?
            .ok()?;
        if ty == TYPE_DEVICE_NAME {
            let name = String::from_utf8_lossy(&payload).trim().to_string();
            return (!name.is_empty()).then_some(name);
        }
    }
}
