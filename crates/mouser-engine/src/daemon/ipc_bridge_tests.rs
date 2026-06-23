use super::*;
use mouser_core::DeviceIdentity;
use mouser_ipc::Client;
use std::time::Duration;

fn temp_path(tag: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mouser-engine-ipc-{tag}-{}-{now}",
        std::process::id()
    ))
}

#[tokio::test]
async fn send_files_command_dispatches_for_active_peer() {
    let peer = DeviceIdentity::generate().device_id();
    let peer_text = crate::daemon_store::format_device_id(&peer);
    let store_dir = temp_path("store");
    let socket = temp_path("socket");
    let shared = Arc::new(Shared {
        store: DaemonStore::new(&store_dir),
        local: DeviceDto {
            id: "local".to_string(),
            name: "Local".to_string(),
            os: OS_KIND.to_string(),
        },
        registry: PeerRegistry::new(),
        connection: Mutex::new(ConnectionDto {
            state: ConnectionStateDto::Connected,
            peer_id: Some(peer_text),
            owner: Some("local".to_string()),
            epoch: None,
            error: None,
        }),
        pairing: Mutex::new(None),
        settings: Mutex::new(SettingsDto::default()),
        started: Instant::now(),
    });
    let server = Server::bind_at(&socket, build_snapshot(&shared))
        .await
        .expect("bind IPC server");
    let publisher = server.publisher();
    let (connect_tx, _connect_rx) = mpsc::unbounded_channel();
    let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();
    let (file_send_tx, mut file_send_rx) = mpsc::unbounded_channel();
    let (decision_tx, _decision_rx) = mpsc::unbounded_channel();
    let command_task = tokio::spawn(command_loop(
        server,
        Arc::clone(&shared),
        publisher,
        connect_tx,
        disconnect_tx,
        file_send_tx,
        decision_tx,
    ));

    let mut client = Client::connect_at(&socket)
        .await
        .expect("connect IPC client");
    let _ = tokio::time::timeout(Duration::from_secs(2), client.next_snapshot())
        .await
        .expect("initial snapshot")
        .expect("snapshot decode");
    client
        .send_command(&Command::SendFiles {
            paths: vec![
                "/tmp/mouser-a.txt".to_string(),
                "/tmp/mouser-b.txt".to_string(),
            ],
        })
        .await
        .expect("send files command");

    let request = tokio::time::timeout(Duration::from_secs(2), file_send_rx.recv())
        .await
        .expect("file send dispatched")
        .expect("file send channel open");
    assert_eq!(request.peer_id, peer);
    assert_eq!(
        request.paths,
        vec![
            PathBuf::from("/tmp/mouser-a.txt"),
            PathBuf::from("/tmp/mouser-b.txt")
        ]
    );

    command_task.abort();
    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_dir_all(store_dir);
}
