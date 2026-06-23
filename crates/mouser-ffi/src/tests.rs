use super::*;
use std::time::Duration;

#[test]
fn new_client_is_disconnected_and_reports_device_id() {
    let client = MobileClient::new();
    assert!(!client.is_connected());
    let id = client.device_id();
    assert!(decode_device_id(&id).is_some(), "device_id is valid base32");
}

#[test]
fn connect_rejects_malformed_peer_id() {
    let client = MobileClient::new();
    let err = client
        .connect(
            "127.0.0.1".into(),
            1,
            "not-base32 !!!".into(),
            "Phone".into(),
        )
        .expect_err("malformed id rejected");
    assert!(matches!(err, MobileError::InvalidPeerId));
    assert!(!client.is_connected());
}

#[test]
fn input_senders_are_noops_while_disconnected() {
    let client = MobileClient::new();
    client.send_pointer_moved(0, 10, 10);
    client.send_button(0, true);
    client.send_key(0x04, true, 0);
    client.send_scroll(0, -3);
    client.disconnect();
    assert!(!client.is_connected());
}

/// End-to-end over a real QUIC loopback connection: a target engine accepts, the
/// `MobileClient` connects as source and sends a key; the target injects it. Proves
/// the FFI drives the whole capture->forward->inject pipeline over the actual
/// transport (mirrors `mouser-engine/tests/loopback.rs`).
#[test]
fn loopback_connect_and_send_forwards_to_target() {
    use mouser_engine::EngineCore;
    use mouser_net::{DeviceIdentity, InteractiveEndpoint, PinPolicy};

    #[derive(Debug, PartialEq, Eq)]
    enum Recorded {
        Key { usage: u16, down: bool },
    }
    struct RecordingInjector {
        tx: tokio::sync::mpsc::UnboundedSender<Recorded>,
    }
    impl InputInjection for RecordingInjector {
        fn move_cursor(&self, _d: u32, _x: i32, _y: i32) -> PlatformResult<()> {
            Ok(())
        }
        fn move_cursor_relative(&self, _dx: i32, _dy: i32) -> PlatformResult<()> {
            Ok(())
        }
        fn button(&self, _b: u8, _down: bool) -> PlatformResult<()> {
            Ok(())
        }
        fn key(&self, usage: u16, down: bool, _mods: u16) -> PlatformResult<()> {
            let _ = self.tx.send(Recorded::Key { usage, down });
            Ok(())
        }
        fn scroll(&self, _dx: i32, _dy: i32, _u: ScrollUnit) -> PlatformResult<()> {
            Ok(())
        }
    }

    let target_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("target runtime");

    let phone = MobileClient::new();
    let phone_id = decode_device_id(&phone.device_id()).expect("phone id");

    let target_identity = DeviceIdentity::generate();
    let target_device = target_identity.device_id();
    let target_id_b32 = target_identity.device_id_base32();

    let (server, server_addr) = target_rt.block_on(async {
        let server = InteractiveEndpoint::bind_server(
            &target_identity,
            mouser_net::loopback_addr(),
            PinPolicy::Pinned(phone_id),
        )
        .expect("bind server");
        let addr = server.local_addr().expect("server addr");
        (server, addr)
    });

    let (rec_tx, mut rec_rx) = tokio::sync::mpsc::unbounded_channel::<Recorded>();
    let accept = target_rt.spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        let conn = Arc::new(conn);
        let _target = RuntimeHandle::start(
            EngineCore::new_target(target_device, phone_id),
            conn,
            Arc::new(RecordingInjector { tx: rec_tx }),
            Arc::new(NoopCapture),
        );
        std::future::pending::<()>().await;
    });

    phone
        .connect(
            server_addr.ip().to_string(),
            server_addr.port(),
            target_id_b32,
            "Test Phone".into(),
        )
        .expect("phone connects");
    assert!(phone.is_connected());

    phone.send_key(0x04, true, 0);

    let got = target_rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(5), rec_rx.recv()).await });
    assert_eq!(
        got.ok().flatten(),
        Some(Recorded::Key {
            usage: 0x04,
            down: true
        }),
        "the forwarded key was injected on the target over QUIC",
    );

    phone.disconnect();
    accept.abort();
}
