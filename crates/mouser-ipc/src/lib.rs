//! mouser-ipc — the local IPC link between the headless `mouserd` engine daemon and the
//! Tauri desktop UI.
//!
//! The daemon owns discovery, trust, and the live peer connection; the UI reflects that
//! state and drives it. This crate is the seam between them: typed DTOs ([`dto`]) over a
//! length-prefixed CBOR ([`codec`]) Unix-domain-socket transport at a well-known
//! [`path`]. The daemon runs the [`Server`] (accept clients, push [`Snapshot`]s on
//! change + on request, forward [`Command`]s); the UI runs the [`Client`] (connect,
//! recv snapshots, send commands).
//!
//! It is panic-free (the workspace clippy lints deny `unwrap`/`panic`/indexing) and
//! `unsafe`-free, like the protocol and core crates.

pub mod client;
pub mod codec;
pub mod dto;
pub mod path;
pub mod server;

pub use client::Client;
pub use codec::{read_message, write_message, IpcError, MAX_FRAME};
pub use dto::{
    Command, ConnectionDto, ConnectionStateDto, DeviceDto, PeerDto, ServerMessage, Snapshot,
};
pub use path::{default_socket_path, SOCKET_FILE};
pub use server::{Publisher, Server};

#[cfg(test)]
mod loopback_tests {
    use super::*;
    use std::time::Duration;

    fn temp_socket_path(tag: &str) -> std::path::PathBuf {
        path::test_socket_path(tag)
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            local: DeviceDto {
                id: "localid".to_string(),
                name: "This Mac".to_string(),
                os: "macos".to_string(),
            },
            peers: vec![PeerDto {
                id: "peerid".to_string(),
                name: "Studio".to_string(),
                os: "linux".to_string(),
                host: "192.168.1.50".to_string(),
                port: 49970,
                trusted: true,
            }],
            connection: ConnectionDto::default(),
        }
    }

    /// In-process loopback: a `Server` and `Client` over a temp socket prove a Snapshot
    /// round-trips (pushed on connect) and a `Connect` command is received by the daemon.
    #[tokio::test]
    async fn snapshot_round_trips_and_connect_command_is_received() {
        let socket = temp_socket_path("loopback");
        let initial = sample_snapshot();
        let mut server = Server::bind_at(&socket, initial.clone())
            .await
            .expect("bind server");

        let mut client = Client::connect_at(&socket).await.expect("connect client");

        // The daemon pushes the current snapshot on connect — it must round-trip exactly.
        let received = tokio::time::timeout(Duration::from_secs(2), client.next_snapshot())
            .await
            .expect("snapshot did not arrive in time")
            .expect("snapshot decode");
        assert_eq!(received, initial);

        // The client sends a Connect command; the daemon must receive it verbatim.
        let want_peer = "peerid".to_string();
        client
            .send_command(&Command::Connect {
                peer_id: want_peer.clone(),
                host: Some("192.168.1.50".to_string()),
                port: Some(49970),
            })
            .await
            .expect("send connect");
        let command = tokio::time::timeout(Duration::from_secs(2), server.recv_command())
            .await
            .expect("command did not arrive in time")
            .expect("server still receiving");
        assert_eq!(
            command,
            Command::Connect {
                peer_id: want_peer,
                host: Some("192.168.1.50".to_string()),
                port: Some(49970),
            }
        );
    }

    /// A snapshot the daemon publishes after connect is pushed to the live client.
    #[tokio::test]
    async fn published_snapshot_is_pushed_to_connected_client() {
        let socket = temp_socket_path("push");
        let server = Server::bind_at(&socket, sample_snapshot())
            .await
            .expect("bind server");
        let mut client = Client::connect_at(&socket).await.expect("connect client");

        // Drain the on-connect snapshot first.
        let _ = tokio::time::timeout(Duration::from_secs(2), client.next_snapshot())
            .await
            .expect("initial snapshot")
            .expect("initial decode");

        // Publish a changed snapshot (now Connected) — the client must see it.
        let mut updated = sample_snapshot();
        updated.connection = ConnectionDto {
            state: ConnectionStateDto::Connected,
            peer_id: Some("peerid".to_string()),
            owner: Some("localid".to_string()),
            epoch: Some(1),
        };
        server.publish(updated.clone());

        let received = tokio::time::timeout(Duration::from_secs(2), client.next_snapshot())
            .await
            .expect("updated snapshot did not arrive")
            .expect("updated decode");
        assert_eq!(received.connection.state, ConnectionStateDto::Connected);
        assert_eq!(received, updated);
    }

    /// A peer can be discovered before the desktop UI connects. The server must retain
    /// that latest snapshot so short-lived polling clients do not see the empty boot
    /// snapshot forever.
    #[tokio::test]
    async fn published_snapshot_without_clients_is_seen_by_later_client() {
        let socket = temp_socket_path("late-client");
        let server = Server::bind_at(&socket, sample_snapshot())
            .await
            .expect("bind server");

        let mut updated = sample_snapshot();
        updated.peers.push(PeerDto {
            id: "latepeer".to_string(),
            name: "Late peer".to_string(),
            os: "macos".to_string(),
            host: "192.168.1.229".to_string(),
            port: 53004,
            trusted: false,
        });
        server.publish(updated.clone());

        let mut client = Client::connect_at(&socket).await.expect("connect client");
        let received = tokio::time::timeout(Duration::from_secs(2), client.next_snapshot())
            .await
            .expect("snapshot did not arrive")
            .expect("snapshot decode");

        assert_eq!(received, updated);
    }
}
