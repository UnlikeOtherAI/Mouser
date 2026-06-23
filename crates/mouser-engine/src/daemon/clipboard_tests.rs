use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::*;
use mouser_core::platform::PlatformResult;
use mouser_core::DeviceIdentity;
use mouser_net::{BulkEndpoint, PinPolicy};
use mouser_protocol::{from_cbor, ClipFormat, ClipboardOffer};

#[derive(Default)]
struct FakeClipboard {
    token: Mutex<u64>,
    data: Mutex<Vec<(PlatformFormat, Vec<u8>)>>,
}

impl FakeClipboard {
    fn set(&self, format: PlatformFormat, bytes: &[u8]) {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, existing)) = data.iter_mut().find(|(stored, _)| *stored == format) {
            *existing = bytes.to_vec();
        } else {
            data.push((format, bytes.to_vec()));
        }
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        *token = token.saturating_add(1);
    }

    fn get(&self, format: PlatformFormat) -> Option<Vec<u8>> {
        self.data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(stored, _)| *stored == format)
            .map(|(_, bytes)| bytes.clone())
    }
}

impl Clipboard for FakeClipboard {
    fn change_token(&self) -> PlatformResult<u64> {
        Ok(*self.token.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn read(&self, format: PlatformFormat) -> PlatformResult<Option<Vec<u8>>> {
        Ok(self.get(format))
    }

    fn write(&self, format: PlatformFormat, data: &[u8]) -> PlatformResult<()> {
        self.set(format, data);
        Ok(())
    }
}

fn only_control(outputs: Vec<ClipboardOutput>) -> ControlFrame {
    assert_eq!(outputs.len(), 1);
    match outputs.into_iter().next().expect("one output") {
        ClipboardOutput::Control(frame) => frame,
        ClipboardOutput::Bulk(_) => panic!("expected control output"),
    }
}

#[test]
fn text_offer_pull_data_applies_to_peer_clipboard() {
    let a_id = [1u8; 32];
    let b_id = [2u8; 32];
    let a_clip = FakeClipboard::default();
    let b_clip = FakeClipboard::default();
    a_clip.set(PlatformFormat::Utf8Text, b"hello from a");

    let mut a = ClipboardSession::new(a_id, b_id, Os::Linux, Os::Linux, true);
    let mut b = ClipboardSession::new(b_id, a_id, Os::Linux, Os::Linux, true);
    a.set_settings(ClipboardSettings::default());
    b.set_settings(ClipboardSettings::default());

    let offer = a.local_offer(&a_clip).expect("local offer");
    assert_eq!(offer.ty, TYPE_CLIPBOARD_OFFER);
    let pull = only_control(b.on_control(offer.ty, &offer.payload, &b_clip));
    assert_eq!(pull.ty, TYPE_CLIPBOARD_PULL);

    let data = only_control(a.on_control(pull.ty, &pull.payload, &a_clip));
    assert_eq!(data.ty, TYPE_CLIPBOARD_DATA);
    let replies = b.on_control(data.ty, &data.payload, &b_clip);
    assert!(replies.is_empty());
    assert_eq!(
        b_clip.get(PlatformFormat::Utf8Text),
        Some(b"hello from a".to_vec())
    );
}

#[test]
fn prefer_native_apple_suppresses_pull() {
    let a_id = [1u8; 32];
    let b_id = [2u8; 32];
    let a_clip = FakeClipboard::default();
    let b_clip = FakeClipboard::default();
    a_clip.set(PlatformFormat::Utf8Text, b"hello");

    let mut a = ClipboardSession::new(a_id, b_id, Os::Macos, Os::Macos, true);
    let mut b = ClipboardSession::new(b_id, a_id, Os::Macos, Os::Macos, true);
    let settings = ClipboardSettings::default();
    a.set_settings(settings);
    b.set_settings(settings);

    let offer = a.local_offer(&a_clip).expect("local offer");
    assert!(b.on_control(offer.ty, &offer.payload, &b_clip).is_empty());
}

#[test]
fn settings_direction_disables_send_or_receive() {
    let mut dto = SettingsDto {
        clipboard_direction: "send_only".to_string(),
        ..SettingsDto::default()
    };
    assert_eq!(settings_from_dto(&dto).direction, SyncDirection::SendOnly);
    dto.clipboard_direction = "receive_only".to_string();
    assert_eq!(
        settings_from_dto(&dto).direction,
        SyncDirection::ReceiveOnly
    );
}

#[test]
fn bulk_disabled_does_not_advertise_png() {
    let a_id = [1u8; 32];
    let b_id = [2u8; 32];
    let a_clip = FakeClipboard::default();
    a_clip.set(PlatformFormat::Png, fake_png());

    let mut a = ClipboardSession::new(a_id, b_id, Os::Linux, Os::Linux, false);
    a.set_settings(ClipboardSettings::default());

    assert!(a.local_offer(&a_clip).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_png_payload_syncs_over_bulk_and_applies_with_matching_hash() {
    let a_identity = DeviceIdentity::generate();
    let b_identity = DeviceIdentity::generate();
    let a_id = a_identity.device_id();
    let b_id = b_identity.device_id();
    let a_clip = FakeClipboard::default();
    let b_clip = FakeClipboard::default();
    let png = fake_png().to_vec();
    a_clip.set(PlatformFormat::Png, &png);
    let expected_hash = content_hash(ClipFormat::Png, &png);

    let mut a = ClipboardSession::new(a_id, b_id, Os::Linux, Os::Linux, true);
    let mut b = ClipboardSession::new(b_id, a_id, Os::Linux, Os::Linux, true);
    a.set_settings(ClipboardSettings::default());
    b.set_settings(ClipboardSettings::default());

    let offer = a.local_offer(&a_clip).expect("local offer");
    let decoded_offer: ClipboardOffer = from_cbor(&offer.payload).expect("decode offer");
    assert!(decoded_offer
        .entries
        .iter()
        .any(|entry| entry.format == ClipFormat::Png && entry.hash == expected_hash));
    let pull = only_control(b.on_control(offer.ty, &offer.payload, &b_clip));
    let outputs = a.on_control(pull.ty, &pull.payload, &a_clip);
    let chunks = outputs
        .into_iter()
        .find_map(|output| match output {
            ClipboardOutput::Bulk(chunks) => Some(chunks),
            ClipboardOutput::Control(_) => None,
        })
        .expect("bulk clipboard chunks");
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].last);

    let receiver = BulkEndpoint::bind_server(
        &b_identity,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(a_id),
    )
    .expect("bind receiver");
    let receiver_addr = receiver.local_addr().expect("receiver addr");
    let sender = BulkEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind sender");
    let (clipboard_tx, clipboard_rx) = super::super::clipboard_bulk::channel();
    let accept_task = tokio::spawn(async move {
        let conn = receiver
            .accept_bulk(super::super::file_transfer::BULK_SESSION_ID)
            .await?;
        let peer_id = conn
            .peer_device_id()
            .ok_or_else(|| mouser_net::NetError::Connect("missing peer id".to_string()))?;
        let mut stream = conn.accept_transfer().await?;
        let (ty, payload) = stream.recv_msg().await?;
        assert_eq!(ty, TYPE_CLIPBOARD_DATA);
        let decoded: ClipboardData = from_cbor(&payload).expect("decode first bulk chunk");
        assert!(decoded.last);
        super::super::clipboard_bulk::receive_clipboard_stream(
            stream,
            peer_id,
            payload,
            clipboard_tx,
        )
        .await
        .map_err(mouser_net::NetError::Connect)
    });

    let sent_conn = super::super::clipboard_bulk::send_chunks_to_addr(
        &sender,
        &a_identity,
        b_id,
        receiver_addr,
        chunks,
    )
    .await
    .expect("send bulk clipboard");
    accept_task
        .await
        .expect("accept task")
        .expect("receive stream");

    let inbound = tokio::time::timeout(Duration::from_secs(2), async {
        let mut guard = clipboard_rx.lock().await;
        guard.recv().await
    })
    .await
    .expect("bulk clipboard data arrived")
    .expect("bulk clipboard queue open");
    assert_eq!(inbound.peer_id, a_id);
    b.on_bulk_data(inbound.data, &b_clip);

    let got = b_clip.get(PlatformFormat::Png).expect("png applied");
    assert_eq!(got, png);
    assert_eq!(content_hash(ClipFormat::Png, &got), expected_hash);
    sent_conn.close();
}

fn fake_png() -> &'static [u8] {
    b"\x89PNG\r\n\x1a\nmouser-png-clipboard-payload"
}
