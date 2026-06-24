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
        if !conn.is_channel_verified() {
            crate::diag!(
                info,
                "mouserd: rejected peer before §5 channel verification completed"
            );
            conn.close();
            continue;
        }
        let Some(peer_id) = conn.peer_device_id() else {
            crate::diag!(info, "mouserd: rejected peer without a valid device_id");
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
                crate::diag!(
                    info,
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
    let sas = conn.sas().map_err(|e| e.to_string())?;
    let name = pairing_name(conn, registry, peer_id).await;
    crate::diag!(
        info,
        "mouserd: pairing request from {name} ({peer_b32}) with SAS {sas}"
    );
    bridge.request_pairing(peer_b32.clone(), name, sas);
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
        crate::diag!(info, "mouserd: paired with {peer_b32}");
    } else {
        crate::diag!(info, "mouserd: pairing with {peer_b32} not approved");
    }
    Ok(approved)
}

async fn pairing_name(
    conn: &InteractiveConnection,
    registry: &PeerRegistry,
    peer_id: &DeviceId,
) -> String {
    if let Some(name) = conn.peer_name() {
        return name.to_string();
    }
    recv_device_name(conn)
        .await
        .or_else(|| {
            registry
                .find(peer_id)
                .map(|p| p.name)
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| "A device".to_string())
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use mouser_core::DeviceIdentity;
    use mouser_net::{InteractiveEndpoint, PinPolicy};
    use mouser_protocol::{
        to_cbor, ClipboardOffer, FileEntry, FileOffer, KeyEvent, TYPE_CLIPBOARD_OFFER,
        TYPE_FILE_OFFER, TYPE_KEY_EVENT,
    };

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unapproved_peer_never_reaches_runtime_gate() {
        let server_id = DeviceIdentity::generate();
        let client_id = DeviceIdentity::generate();
        let client_device_id = client_id.device_id();
        let store_dir = temp_path("pairing-gate");
        let store = DaemonStore::new(&store_dir);
        let registry = PeerRegistry::new();
        let server = InteractiveEndpoint::bind_server(
            &server_id,
            mouser_net::loopback_addr(),
            PinPolicy::TrustOnFirstUse,
        )
        .expect("bind server");
        let server_addr = server.local_addr().expect("server addr");

        let accept = tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                accept_trusted(&server, &store, &registry, None),
            )
            .await;
            let trusted = store
                .is_peer_trusted(&client_device_id)
                .expect("trust lookup");
            (result, trusted)
        });

        let client =
            InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");
        let conn = client
            .connect_interactive(
                &client_id,
                server_addr,
                PinPolicy::Pinned(server_id.device_id()),
            )
            .await
            .expect("client connect");

        let _ = conn.send_control(TYPE_KEY_EVENT, &key_payload()).await;
        let _ = conn
            .send_control(TYPE_CLIPBOARD_OFFER, &clipboard_payload())
            .await;
        let _ = conn.send_control(TYPE_FILE_OFFER, &file_payload()).await;

        let (result, trusted) = accept.await.expect("accept task");
        assert!(
            result.is_err(),
            "without SAS approval no active InteractiveConnection may reach runtime"
        );
        assert!(!trusted, "unapproved peer must not enter trusted-peers");
        conn.close();
        let _ = std::fs::remove_dir_all(store_dir);
    }

    fn key_payload() -> Vec<u8> {
        to_cbor(&KeyEvent {
            usage: 0x04,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr: 1,
        })
        .expect("key payload")
    }

    fn clipboard_payload() -> Vec<u8> {
        to_cbor(&ClipboardOffer {
            entries: Vec::new(),
            origin: vec![0xAA; 32],
        })
        .expect("clipboard payload")
    }

    fn file_payload() -> Vec<u8> {
        to_cbor(&FileOffer {
            transfer_id: 7,
            files: vec![FileEntry {
                name: "blocked.txt".to_string(),
                size: 1,
                sha256: None,
            }],
        })
        .expect("file payload")
    }

    fn temp_path(tag: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mouser-engine-{tag}-{}-{now}", std::process::id()))
    }
}
