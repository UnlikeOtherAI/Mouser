//! One connected peer session: runtime, clipboard side lane, and IPC file sends.

use std::sync::Arc;

use mouser_core::platform::{Clipboard, InputCapture, InputInjection};
use mouser_core::DeviceId;
use mouser_net::{BulkEndpoint, DeviceIdentity, InteractiveConnection};

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::discovery::PeerRegistry;
use crate::{EngineCore, RuntimeHandle};

use super::clipboard::{self as clipboard_driver, DriverConfig, SettingsProvider};
use super::clipboard_bulk::{BulkClipboardSender, ClipboardBulkRx};
use super::file_transfer;
use super::ipc_bridge::{ConnectRequest, FileSendRequest, IpcBridge};
use super::source_layout;

pub(super) struct SessionContext<'a> {
    pub store: &'a DaemonStore,
    pub registry: &'a PeerRegistry,
    pub bridge: Option<&'a IpcBridge>,
    pub identity: Arc<DeviceIdentity>,
    pub bulk_endpoint: Arc<BulkEndpoint>,
    pub bulk_session_id: u64,
    pub clipboard_bulk_rx: ClipboardBulkRx,
}

pub(super) struct SessionAdapters {
    pub injector: Arc<dyn InputInjection>,
    pub capture: Arc<dyn InputCapture>,
    pub clipboard: Arc<dyn Clipboard>,
}

pub(super) enum SessionEnd {
    Shutdown,
    Disconnected,
    ConnectionLost { reason: String },
    /// The user explicitly asked (over IPC) to connect to a peer while this machine was a
    /// receiver (or connected to a different peer). End this session and re-establish as
    /// the **source** for `peer`, so an explicit Connect always makes the clicking machine
    /// the controller regardless of who auto-dialed first.
    SwitchSource(DeviceId),
}

pub(super) async fn run_session(
    my_id: DeviceId,
    peer: DeviceId,
    can_control: bool,
    conn: InteractiveConnection,
    context: SessionContext<'_>,
    adapters: SessionAdapters,
) -> SessionEnd {
    let session_id = conn.session_id();
    let peer_session_id = conn.peer_session_id();
    let core = if can_control {
        // The cross edge (which side the peer sits on) is a user setting, applied per
        // session.
        let edge = crate::core::Edge::from_setting(&context.store.load_settings().cross_edge);
        EngineCore::new_source(my_id, peer, source_layout(edge))
    } else {
        EngineCore::new_target(my_id, peer)
    };
    let mut runtime =
        RuntimeHandle::start(core, Arc::new(conn), adapters.injector, adapters.capture);
    let peer_os = context
        .registry
        .find(&peer)
        .map(|advert| clipboard_driver::os_from_str(&advert.os))
        .unwrap_or(mouser_protocol::Os::Unknown);
    let settings = context
        .bridge
        .map(|bridge| SettingsProvider::Bridge(bridge.settings_source()))
        .unwrap_or_else(|| SettingsProvider::Fixed(context.store.load_settings()));
    let bulk_sender = BulkClipboardSender::new(
        Arc::clone(&context.bulk_endpoint),
        Arc::clone(&context.identity),
        PeerRegistry::clone(context.registry),
        peer,
        context.bulk_session_id,
    );
    let clipboard_task = runtime.take_control_lane().map(|lane| {
        tokio::spawn(clipboard_driver::run_driver(
            lane,
            adapters.clipboard,
            DriverConfig {
                my_id,
                peer_id: peer,
                peer_os,
                settings,
                bulk_sender: Some(bulk_sender),
                bulk_rx: Some(Arc::clone(&context.clipboard_bulk_rx)),
            },
        ))
    });

    if can_control {
        crate::diag!(
            info,
            "mouserd: passive edge sensing active - local keyboard/mouse stay native; \
             suppressing capture installs only while controlling the peer"
        );
    } else {
        crate::diag!(
            info,
            "mouserd: target ready - injecting input received from the source"
        );
    }

    let mut file_send_open = context.bridge.is_some();
    let end = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break SessionEnd::Shutdown,
            _ = wait_for_disconnect(context.bridge) => {
                crate::diag!(info, "mouserd: disconnect requested over IPC");
                break SessionEnd::Disconnected;
            }
            request = wait_for_connect(context.bridge) => {
                if let Some(request) = request {
                    // An explicit Connect means "I want to control this peer." If we're a
                    // receiver, or pointed at a different peer, switch to being its source.
                    // If we're already its source, there's nothing to do.
                    if !can_control || request.peer_id != peer {
                        break SessionEnd::SwitchSource(request.peer_id);
                    }
                }
            }
            request = wait_for_file_send(context.bridge), if file_send_open => {
                let Some(request) = request else {
                    file_send_open = false;
                    continue;
                };
                if request.peer_id != peer {
                    crate::diag!(info, "mouserd: ignoring file-send request for an inactive peer");
                    continue;
                }
                spawn_file_send(
                    Arc::clone(&context.bulk_endpoint),
                    Arc::clone(&context.identity),
                    PeerRegistry::clone(context.registry),
                    context.bulk_session_id,
                    request,
                );
            }
            _ = runtime.wait_dead() => {
                let transport_reason = runtime
                    .death_reason()
                    .unwrap_or_else(|| "connection closed without a transport reason".to_string());
                let reason = format!(
                    "peer_id={} session_id={session_id} peer_session_id={peer_session_id}: {transport_reason}",
                    format_device_id(&peer)
                );
                break SessionEnd::ConnectionLost { reason };
            }
        }
    };
    if let Some(task) = clipboard_task {
        task.abort();
    }
    runtime.shutdown();
    end
}

fn spawn_file_send(
    endpoint: Arc<BulkEndpoint>,
    identity: Arc<DeviceIdentity>,
    registry: PeerRegistry,
    bulk_session_id: u64,
    request: FileSendRequest,
) {
    tokio::spawn(async move {
        if let Err(e) = file_transfer::send_paths_to_peer(
            endpoint,
            identity,
            registry,
            request.peer_id,
            bulk_session_id,
            request.paths,
        )
        .await
        {
            crate::diag!(info, "mouserd: file send failed: {e}");
        }
    });
}

async fn wait_for_disconnect(bridge: Option<&IpcBridge>) {
    match bridge {
        Some(bridge) => bridge.next_disconnect_request().await,
        None => std::future::pending().await,
    }
}

async fn wait_for_connect(bridge: Option<&IpcBridge>) -> Option<ConnectRequest> {
    match bridge {
        Some(bridge) => bridge.next_connect_request().await,
        None => std::future::pending().await,
    }
}

async fn wait_for_file_send(bridge: Option<&IpcBridge>) -> Option<FileSendRequest> {
    match bridge {
        Some(bridge) => bridge.next_file_send_request().await,
        None => std::future::pending().await,
    }
}
