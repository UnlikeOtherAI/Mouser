//! Daemon clipboard driver over the interactive control stream.

use std::sync::Arc;
use std::time::Duration;

use mouser_clipboard::{
    canonical, content_hash, transport_for, ClipboardEngine, ClipboardSettings, LocalRepr,
    MemContentSource, SyncDirection, Transport, CONTROL_TEXT_CAP,
};
use mouser_core::platform::{ClipFormat as PlatformFormat, Clipboard};
use mouser_core::DeviceId;
use mouser_ipc::SettingsDto;
use mouser_protocol::{
    from_cbor, to_cbor, ClipFormat, ClipboardData, ClipboardEntry, ClipboardOffer, ClipboardPull,
    Os, TYPE_CLIPBOARD_DATA, TYPE_CLIPBOARD_OFFER, TYPE_CLIPBOARD_PULL,
};

use crate::runtime::RuntimeControlLane;

use super::ipc_bridge::SettingsSource;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

const FORMATS: [(ClipFormat, PlatformFormat); 5] = [
    (ClipFormat::Utf8Text, PlatformFormat::Utf8Text),
    (ClipFormat::Html, PlatformFormat::Html),
    (ClipFormat::Rtf, PlatformFormat::Rtf),
    (ClipFormat::UriList, PlatformFormat::UriList),
    (ClipFormat::Png, PlatformFormat::Png),
];

#[derive(Clone)]
pub(super) enum SettingsProvider {
    Bridge(SettingsSource),
    Fixed(SettingsDto),
}

impl SettingsProvider {
    pub(super) fn current(&self) -> ClipboardSettings {
        let dto = match self {
            Self::Bridge(source) => source.settings(),
            Self::Fixed(settings) => settings.clone(),
        };
        settings_from_dto(&dto)
    }
}

pub(super) async fn run_driver(
    lane: RuntimeControlLane,
    clipboard: Arc<dyn Clipboard>,
    my_id: DeviceId,
    peer_id: DeviceId,
    peer_os: Os,
    settings: SettingsProvider,
) {
    let mut lane = lane;
    let mut session = ClipboardSession::new(my_id, peer_id, local_os(), peer_os);
    let mut last_token = clipboard.change_token().ok();
    let mut tick = 0u64;
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick = tick.saturating_add(1);
                session.set_settings(settings.current());
                let expired = session.engine.tick(tick);
                if expired > 0 {
                    eprintln!("mouserd: expired {expired} stalled clipboard pull(s)");
                }
                if let Ok(token) = clipboard.change_token() {
                    if last_token.is_some_and(|prev| prev == token) {
                        continue;
                    }
                    last_token = Some(token);
                    if let Some(frame) = session.local_offer(clipboard.as_ref()) {
                        send_frame(&lane, frame);
                    }
                }
            }
            msg = lane.recv() => {
                let Some(msg) = msg else { break };
                session.set_settings(settings.current());
                for frame in session.on_control(msg.ty, &msg.payload, clipboard.as_ref()) {
                    send_frame(&lane, frame);
                }
            }
        }
    }
}

fn send_frame(lane: &RuntimeControlLane, frame: ControlFrame) {
    if !lane.send(frame.ty, frame.payload) {
        eprintln!("mouserd: clipboard control lane is closed");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlFrame {
    ty: u16,
    payload: Vec<u8>,
}

impl ControlFrame {
    fn encode<T: serde::Serialize>(ty: u16, value: &T) -> Option<Self> {
        match to_cbor(value) {
            Ok(payload) => Some(Self { ty, payload }),
            Err(e) => {
                eprintln!("mouserd: failed to encode clipboard control frame: {e}");
                None
            }
        }
    }
}

struct ClipboardSession {
    engine: ClipboardEngine,
    source: MemContentSource,
    peer_id: DeviceId,
    peer_os: Os,
}

impl ClipboardSession {
    fn new(my_id: DeviceId, peer_id: DeviceId, local_os: Os, peer_os: Os) -> Self {
        Self {
            engine: ClipboardEngine::new(my_id, local_os, ClipboardSettings::default()),
            source: MemContentSource::new(),
            peer_id,
            peer_os,
        }
    }

    fn set_settings(&mut self, settings: ClipboardSettings) {
        self.engine.set_settings(settings);
    }

    fn local_offer(&mut self, clipboard: &dyn Clipboard) -> Option<ControlFrame> {
        let settings = *self.engine.settings();
        let (reps, source) = snapshot_local(clipboard, &settings);
        self.source = source;
        let offer = self.engine.on_local_change(&reps)?;
        let offer = control_only_offer(offer)?;
        ControlFrame::encode(TYPE_CLIPBOARD_OFFER, &offer)
    }

    fn on_control(
        &mut self,
        ty: u16,
        payload: &[u8],
        clipboard: &dyn Clipboard,
    ) -> Vec<ControlFrame> {
        match ty {
            TYPE_CLIPBOARD_OFFER => self.on_offer(payload),
            TYPE_CLIPBOARD_PULL => self.on_pull(payload),
            TYPE_CLIPBOARD_DATA => self.on_data(payload, clipboard),
            _ => Vec::new(),
        }
    }

    fn on_offer(&mut self, payload: &[u8]) -> Vec<ControlFrame> {
        let Ok(offer) = from_cbor::<ClipboardOffer>(payload) else {
            return Vec::new();
        };
        let Some(offer) = control_only_offer(offer) else {
            return Vec::new();
        };
        match self.engine.on_offer(&offer, self.peer_os) {
            Ok(Some(pull)) => ControlFrame::encode(TYPE_CLIPBOARD_PULL, &pull)
                .into_iter()
                .collect(),
            Ok(None) => Vec::new(),
            Err(e) => {
                eprintln!("mouserd: ignored clipboard offer: {e}");
                Vec::new()
            }
        }
    }

    fn on_pull(&mut self, payload: &[u8]) -> Vec<ControlFrame> {
        let Ok(pull) = from_cbor::<ClipboardPull>(payload) else {
            return Vec::new();
        };
        match self.engine.on_pull(&pull, &self.source) {
            Ok(data) => data
                .into_iter()
                .filter(is_control_data)
                .filter_map(|data| ControlFrame::encode(TYPE_CLIPBOARD_DATA, &data))
                .collect(),
            Err(e) => {
                eprintln!("mouserd: ignored clipboard pull: {e}");
                Vec::new()
            }
        }
    }

    fn on_data(&mut self, payload: &[u8], clipboard: &dyn Clipboard) -> Vec<ControlFrame> {
        let Ok(data) = from_cbor::<ClipboardData>(payload) else {
            return Vec::new();
        };
        match self.engine.on_data_from(self.peer_id, &data) {
            Ok(Some(applied)) => {
                if let Some(format) = to_platform(applied.format) {
                    if let Err(e) = clipboard.write(format, &applied.bytes) {
                        eprintln!("mouserd: failed to write clipboard data: {e}");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("mouserd: ignored clipboard data: {e}"),
        }
        Vec::new()
    }
}

fn snapshot_local(
    clipboard: &dyn Clipboard,
    settings: &ClipboardSettings,
) -> (Vec<LocalRepr>, MemContentSource) {
    let mut reps = Vec::new();
    let mut source = MemContentSource::new();
    if !settings.can_offer() {
        return (reps, source);
    }

    for (wire, platform) in FORMATS {
        if !settings.format_enabled(wire) {
            continue;
        }
        let Ok(Some(bytes)) = clipboard.read(platform) else {
            continue;
        };
        let canonical = canonical(wire, &bytes);
        if transport_for(wire, canonical.len()) != Transport::Control {
            continue;
        }
        let hash = content_hash(wire, &bytes);
        source.insert(wire, hash, canonical);
        reps.push(LocalRepr::new(wire, bytes));
    }
    (reps, source)
}

fn control_only_offer(offer: ClipboardOffer) -> Option<ClipboardOffer> {
    let entries = offer
        .entries
        .into_iter()
        .filter(control_entry)
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(ClipboardOffer {
        entries,
        origin: offer.origin,
    })
}

fn control_entry(entry: &ClipboardEntry) -> bool {
    let Ok(size) = usize::try_from(entry.size) else {
        return false;
    };
    transport_for(entry.format, size) == Transport::Control
}

fn is_control_data(data: &ClipboardData) -> bool {
    data.offset == 0
        && data.last
        && data.data.len() <= CONTROL_TEXT_CAP
        && transport_for(data.format, data.data.len()) == Transport::Control
}

fn to_platform(format: ClipFormat) -> Option<PlatformFormat> {
    FORMATS
        .into_iter()
        .find(|(wire, _)| *wire == format)
        .map(|(_, platform)| platform)
}

fn settings_from_dto(dto: &SettingsDto) -> ClipboardSettings {
    ClipboardSettings {
        shared_clipboard: dto.shared_clipboard,
        sync_text: dto.sync_text,
        sync_images: dto.sync_images,
        sync_files: dto.sync_files,
        max_auto_sync_bytes: dto.max_auto_sync_bytes,
        prefer_native_apple: dto.prefer_native_apple,
        direction: direction_from_str(&dto.clipboard_direction),
    }
}

fn direction_from_str(value: &str) -> SyncDirection {
    match value {
        "send_only" => SyncDirection::SendOnly,
        "receive_only" => SyncDirection::ReceiveOnly,
        _ => SyncDirection::Bidirectional,
    }
}

fn local_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    }
}

pub(super) fn os_from_str(value: &str) -> Os {
    match value {
        "macos" => Os::Macos,
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        "ios" => Os::Ios,
        "android" => Os::Android,
        _ => Os::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};

    use super::*;
    use mouser_core::platform::PlatformResult;

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

    #[test]
    fn text_offer_pull_data_applies_to_peer_clipboard() {
        let a_id = [1u8; 32];
        let b_id = [2u8; 32];
        let a_clip = FakeClipboard::default();
        let b_clip = FakeClipboard::default();
        a_clip.set(PlatformFormat::Utf8Text, b"hello from a");

        let mut a = ClipboardSession::new(a_id, b_id, Os::Linux, Os::Linux);
        let mut b = ClipboardSession::new(b_id, a_id, Os::Linux, Os::Linux);
        a.set_settings(ClipboardSettings::default());
        b.set_settings(ClipboardSettings::default());

        let offer = a.local_offer(&a_clip).expect("local offer");
        assert_eq!(offer.ty, TYPE_CLIPBOARD_OFFER);
        let pulls = b.on_control(offer.ty, &offer.payload, &b_clip);
        assert_eq!(pulls.len(), 1);
        assert_eq!(pulls[0].ty, TYPE_CLIPBOARD_PULL);

        let data = a.on_control(pulls[0].ty, &pulls[0].payload, &a_clip);
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].ty, TYPE_CLIPBOARD_DATA);
        let replies = b.on_control(data[0].ty, &data[0].payload, &b_clip);
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

        let mut a = ClipboardSession::new(a_id, b_id, Os::Macos, Os::Macos);
        let mut b = ClipboardSession::new(b_id, a_id, Os::Macos, Os::Macos);
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
}
