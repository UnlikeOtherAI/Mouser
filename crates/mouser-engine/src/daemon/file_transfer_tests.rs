use super::*;
use crate::daemon::clipboard_bulk;
use mouser_files::sha256;

fn scratch_dir(tag: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("mouser-engine-{tag}-{}-{now}", std::process::id()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_bulk_loopback_sends_file_into_quarantine() {
    let receiver_id = DeviceIdentity::generate();
    let sender_id = DeviceIdentity::generate();
    let receiver_device = receiver_id.device_id();
    let sender_device = sender_id.device_id();

    let receiver = BulkEndpoint::bind_server(
        &receiver_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(sender_device),
    )
    .expect("bind receiver");
    let receiver_addr = receiver.local_addr().expect("receiver addr");
    let sender = BulkEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind sender");

    let quarantine = scratch_dir("quarantine");
    let source_dir = scratch_dir("source");
    std::fs::create_dir_all(&quarantine).expect("create quarantine");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    let source_path = source_dir.join("sample.txt");
    let bytes = b"mouser daemon file transfer\nsmall but real\n".to_vec();
    std::fs::write(&source_path, &bytes).expect("write source");
    let expected_hash = sha256(&bytes);
    let (clipboard_tx, _clipboard_rx) = clipboard_bulk::channel();

    let recv_task = tokio::spawn({
        let quarantine = quarantine.clone();
        async move {
            let conn = receiver.accept_bulk(BULK_SESSION_ID).await.expect("accept");
            serve_bulk_connection(conn, sender_device, quarantine, clipboard_tx).await
        }
    });

    send_paths_to_addr(
        &sender,
        &sender_id,
        receiver_device,
        &[receiver_addr],
        vec![source_path],
    )
    .await
    .expect("send file");

    let landed = quarantine.join("sample.txt");
    for _ in 0..200 {
        if landed.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let got = std::fs::read(&landed).expect("read landed file");
    assert_eq!(got, bytes);
    assert_eq!(sha256(&got), expected_hash);

    sender.close();
    recv_task.abort();
    let _ = std::fs::remove_dir_all(&quarantine);
    let _ = std::fs::remove_dir_all(&source_dir);
}
