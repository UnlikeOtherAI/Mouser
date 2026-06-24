//! Daemon clipboard driver over the interactive control stream and bulk plane.

use std::future;
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

use super::clipboard_bulk::{BulkClipboardSender, ClipboardBulkRx, InboundClipboardData};
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

pub(super) struct DriverConfig {
    pub my_id: DeviceId,
    pub peer_id: DeviceId,
    pub peer_os: Os,
    pub settings: SettingsProvider,
    pub bulk_sender: Option<BulkClipboardSender>,
    pub bulk_rx: Option<ClipboardBulkRx>,
}

pub(super) async fn run_driver(
    lane: RuntimeControlLane,
    clipboard: Arc<dyn Clipboard>,
    config: DriverConfig,
) {
    let mut lane = lane;
    let DriverConfig {
        my_id,
        peer_id,
        peer_os,
        settings,
        bulk_sender,
        mut bulk_rx,
    } = config;
    let bulk_enabled = bulk_sender.is_some() && bulk_rx.is_some();
    let mut session = ClipboardSession::new(my_id, peer_id, local_os(), peer_os, bulk_enabled);
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
                    crate::diag!(info, "mouserd: expired {expired} stalled clipboard pull(s)");
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
                for output in session.on_control(msg.ty, &msg.payload, clipboard.as_ref()) {
                    handle_output(&lane, &bulk_sender, output);
                }
            }
            bulk = recv_bulk_data(&bulk_rx) => {
                let Some(inbound) = bulk else {
                    bulk_rx = None;
                    continue;
                };
                if inbound.peer_id != peer_id {
                    continue;
                }
                session.set_settings(settings.current());
                session.on_bulk_data(inbound.data, clipboard.as_ref());
            }
        }
    }
}

async fn recv_bulk_data(rx: &Option<ClipboardBulkRx>) -> Option<InboundClipboardData> {
    let Some(rx) = rx else {
        return future::pending().await;
    };
    let mut guard = rx.lock().await;
    guard.recv().await
}

fn send_frame(lane: &RuntimeControlLane, frame: ControlFrame) {
    if !lane.send(frame.ty, frame.payload) {
        crate::diag!(info, "mouserd: clipboard control lane is closed");
    }
}

fn handle_output(
    lane: &RuntimeControlLane,
    bulk_sender: &Option<BulkClipboardSender>,
    output: ClipboardOutput,
) {
    match output {
        ClipboardOutput::Control(frame) => send_frame(lane, frame),
        ClipboardOutput::Bulk(chunks) => {
            let Some(sender) = bulk_sender.clone() else {
                crate::diag!(
                    info,
                    "mouserd: cannot send bulk clipboard data without a bulk endpoint"
                );
                return;
            };
            tokio::spawn(async move {
                if let Err(e) = sender.send_chunks(chunks).await {
                    crate::diag!(info, "mouserd: bulk clipboard send failed: {e}");
                }
            });
        }
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
                crate::diag!(
                    info,
                    "mouserd: failed to encode clipboard control frame: {e}"
                );
                None
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ClipboardOutput {
    Control(ControlFrame),
    Bulk(Vec<ClipboardData>),
}

struct ClipboardSession {
    engine: ClipboardEngine,
    source: MemContentSource,
    peer_id: DeviceId,
    peer_os: Os,
    bulk_enabled: bool,
}

impl ClipboardSession {
    fn new(
        my_id: DeviceId,
        peer_id: DeviceId,
        local_os: Os,
        peer_os: Os,
        bulk_enabled: bool,
    ) -> Self {
        Self {
            engine: ClipboardEngine::new(my_id, local_os, ClipboardSettings::default()),
            source: MemContentSource::new(),
            peer_id,
            peer_os,
            bulk_enabled,
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
        let offer = supported_offer(offer, self.bulk_enabled)?;
        ControlFrame::encode(TYPE_CLIPBOARD_OFFER, &offer)
    }

    fn on_control(
        &mut self,
        ty: u16,
        payload: &[u8],
        clipboard: &dyn Clipboard,
    ) -> Vec<ClipboardOutput> {
        match ty {
            TYPE_CLIPBOARD_OFFER => self.on_offer(payload),
            TYPE_CLIPBOARD_PULL => self.on_pull(payload),
            TYPE_CLIPBOARD_DATA => self.on_data(payload, clipboard),
            _ => Vec::new(),
        }
    }

    fn on_offer(&mut self, payload: &[u8]) -> Vec<ClipboardOutput> {
        let Ok(offer) = from_cbor::<ClipboardOffer>(payload) else {
            return Vec::new();
        };
        let Some(offer) = supported_offer(offer, self.bulk_enabled) else {
            return Vec::new();
        };
        match self.engine.on_offer(&offer, self.peer_os) {
            Ok(Some(pull)) => ControlFrame::encode(TYPE_CLIPBOARD_PULL, &pull)
                .map(ClipboardOutput::Control)
                .into_iter()
                .collect(),
            Ok(None) => Vec::new(),
            Err(e) => {
                crate::diag!(info, "mouserd: ignored clipboard offer: {e}");
                Vec::new()
            }
        }
    }

    fn on_pull(&mut self, payload: &[u8]) -> Vec<ClipboardOutput> {
        let Ok(pull) = from_cbor::<ClipboardPull>(payload) else {
            return Vec::new();
        };
        match self.engine.on_pull(&pull, &self.source) {
            Ok(data) => route_clipboard_data(data, self.bulk_enabled),
            Err(e) => {
                crate::diag!(info, "mouserd: ignored clipboard pull: {e}");
                Vec::new()
            }
        }
    }

    fn on_data(&mut self, payload: &[u8], clipboard: &dyn Clipboard) -> Vec<ClipboardOutput> {
        let Ok(data) = from_cbor::<ClipboardData>(payload) else {
            return Vec::new();
        };
        if !is_control_data(&data) {
            return Vec::new();
        }
        self.apply_data(&data, clipboard);
        Vec::new()
    }

    fn on_bulk_data(&mut self, data: ClipboardData, clipboard: &dyn Clipboard) {
        self.apply_data(&data, clipboard);
    }

    fn apply_data(&mut self, data: &ClipboardData, clipboard: &dyn Clipboard) {
        match self.engine.on_data_from(self.peer_id, data) {
            Ok(Some(applied)) => {
                if let Some(format) = to_platform(applied.format) {
                    if let Err(e) = clipboard.write(format, &applied.bytes) {
                        crate::diag!(info, "mouserd: failed to write clipboard data: {e}");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => crate::diag!(info, "mouserd: ignored clipboard data: {e}"),
        }
    }
}

fn route_clipboard_data(data: Vec<ClipboardData>, bulk_enabled: bool) -> Vec<ClipboardOutput> {
    let mut outputs = Vec::new();
    let mut bulk = Vec::new();
    for chunk in data {
        if is_control_data(&chunk) {
            if let Some(frame) = ControlFrame::encode(TYPE_CLIPBOARD_DATA, &chunk) {
                outputs.push(ClipboardOutput::Control(frame));
            }
        } else if bulk_enabled {
            bulk.push(chunk);
        }
    }
    if !bulk.is_empty() {
        outputs.push(ClipboardOutput::Bulk(bulk));
    }
    outputs
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
        let hash = content_hash(wire, &bytes);
        source.insert(wire, hash, canonical);
        reps.push(LocalRepr::new(wire, bytes));
    }
    (reps, source)
}

fn supported_offer(offer: ClipboardOffer, bulk_enabled: bool) -> Option<ClipboardOffer> {
    let entries = offer
        .entries
        .into_iter()
        .filter(|entry| supported_entry(entry, bulk_enabled))
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(ClipboardOffer {
        entries,
        origin: offer.origin,
    })
}

fn supported_entry(entry: &ClipboardEntry, bulk_enabled: bool) -> bool {
    bulk_enabled || control_entry(entry)
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
#[path = "clipboard_tests.rs"]
mod tests;
