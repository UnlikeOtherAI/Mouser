//! Direct (explicit-address) daemon modes: `probe` (handshake-only transport check)
//! and `connect <host:port> <peer-id>` (trusted controller dial, no mDNS).

use std::net::SocketAddr;
use std::sync::Arc;

use mouser_core::platform::{Clipboard, InputCapture, InputInjection};
use mouser_core::DeviceId;
use mouser_net::{InteractiveEndpoint, PinPolicy};
use mouser_protocol::Os;

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::{EngineCore, RuntimeHandle};

use super::clipboard::{run_driver, DriverConfig, SettingsProvider};
use super::source_layout;

/// Connect to an explicit peer (TrustOnFirstUse) and report the handshake, then
/// exit - a safe transport check that never captures or injects input.
pub(super) async fn probe(store: &DaemonStore, addr: SocketAddr) -> Result<(), String> {
    let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
    let endpoint = InteractiveEndpoint::bind_client(mouser_net::client_bind_for(addr))
        .map_err(|e| e.to_string())?;
    eprintln!("mouserd: probing {addr}...");
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        endpoint.connect_interactive(&me, addr, PinPolicy::TrustOnFirstUse),
    )
    .await
    .map_err(|_| format!("timed out connecting to {addr} (no Mouser peer there, or UDP blocked)"))?
    .map_err(|e| e.to_string())?;
    let alpn = conn
        .negotiated_alpn()
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    let peer = conn.peer_device_id();
    let peer_text = peer
        .as_ref()
        .map(format_device_id)
        .unwrap_or_else(|| "<missing>".to_string());
    eprintln!(
        "mouserd: PROBE OK - handshake with {addr} completed; ALPN={alpn:?}; \
         peer_device_id={peer_text}"
    );
    if let Some(peer_id) = peer {
        eprintln!(
            "mouserd: to trust this peer on this machine, run: mouserd trust {}",
            format_device_id(&peer_id)
        );
    }
    conn.shutdown().await;
    Ok(())
}

/// Source mode against an explicit peer address (direct dial, no mDNS): this host
/// becomes the controller (captures + forwards across the right edge).
pub(super) async fn serve_direct(
    store: &DaemonStore,
    addr: SocketAddr,
    expected_peer: DeviceId,
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    clipboard: Arc<dyn Clipboard>,
) -> Result<(), String> {
    if !store
        .is_peer_trusted(&expected_peer)
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "peer {} is not trusted on this machine; run `mouserd trust {}` first",
            format_device_id(&expected_peer),
            format_device_id(&expected_peer)
        ));
    }

    let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
    let my_id = me.device_id();
    eprintln!("mouserd: device_id {}", me.device_id_base32());
    let endpoint = InteractiveEndpoint::bind_client(mouser_net::client_bind_for(addr))
        .map_err(|e| e.to_string())?;
    eprintln!("mouserd: dialing {addr} directly...");
    let conn = endpoint
        .connect_interactive(&me, addr, PinPolicy::Pinned(expected_peer))
        .await
        .map_err(|e| e.to_string())?;
    let peer = conn
        .peer_device_id()
        .ok_or("peer did not present a device_id")?;
    eprintln!("mouserd: connected directly; this machine can control the peer");

    let core = EngineCore::new_source(my_id, peer, source_layout());
    let mut runtime = RuntimeHandle::start(core, Arc::new(conn), injector, capture);
    let clipboard_task = runtime.take_control_lane().map(|lane| {
        tokio::spawn(run_driver(
            lane,
            clipboard,
            DriverConfig {
                my_id,
                peer_id: peer,
                peer_os: Os::Unknown,
                settings: SettingsProvider::Fixed(store.load_settings()),
                bulk_sender: None,
                bulk_rx: None,
            },
        ))
    });
    eprintln!(
        "mouserd: passive edge sensing active - local keyboard/mouse stay native; \
         suppressing capture installs only while controlling the peer"
    );

    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    if let Some(task) = clipboard_task {
        task.abort();
    }
    runtime.shutdown();
    Ok(())
}
