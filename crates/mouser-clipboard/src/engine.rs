//! The clipboard sync **state machine** (§7.7) — pure and deterministic.
//!
//! `ClipboardEngine` drives one device's side of the clipboard protocol against its
//! peers. It is I/O-free: local clipboard reads come in as [`LocalRepr`]s, outbound
//! pull-answers come from a [`ClipContentSource`], and applied inbound content is
//! handed back as an [`AppliedClip`] for the caller to write. The engine never touches
//! the OS clipboard, a socket, or the clock.
//!
//! Flows it implements:
//! - **Local change → Offer** ([`ClipboardEngine::on_local_change`]): canonicalize +
//!   hash each enabled representation, emit `ClipboardOffer{entries, origin}`. A
//!   representation the engine just *applied* (`(origin, format, hash)`) is suppressed
//!   once so an applied clip is not bounced back (loop prevention).
//! - **Inbound Offer → eager auto-pull** ([`ClipboardEngine::on_offer`]): pick the
//!   best accepted representation and emit `ClipboardPull` immediately, subject to the
//!   per-format gates, the auto-sync size limit, and the prefer-native-Apple rule.
//! - **Inbound Pull → Data chunks** ([`ClipboardEngine::on_pull`]): produce ordered
//!   `ClipboardData` for a representation we offered (one control-stream message for
//!   small payloads, ≤1 MiB bulk chunks for large ones).
//! - **Inbound Data → reassemble/verify/apply** ([`ClipboardEngine::on_data`]):
//!   accumulate by `(origin, format, hash)`, expose [`Progress`], verify the hash on
//!   `last`, and surface the completed [`AppliedClip`] (tagging the representation so
//!   the local clipboard-change echo is suppressed once).

use mouser_core::DeviceId;
use mouser_protocol::{
    ClipFormat, ClipboardData, ClipboardEntry, ClipboardOffer, ClipboardPull, Os,
};

use crate::canonical::content_hash;
use crate::reassembly::Progress;
use crate::settings::prefer_native_suppresses;
use crate::source::{AppliedClip, ClipContentSource, LocalRepr};
use crate::tracking::{AppliedLog, Pending, PendingPulls, PullKey};
use crate::{ClipboardError, ClipboardSettings, Hash, CONTROL_TEXT_CAP, MAX_DATA_CHUNK};

/// Preference order for choosing the best representation to pull (§7.7): `png` first
/// (an image copy), then the rich text formats, then plain text last. Lower index =
/// more preferred. Used both to rank an inbound offer's entries and (implicitly) so a
/// text-only copy resolves to `utf8_text`.
const PREFERENCE: [ClipFormat; 5] = [
    ClipFormat::Png,
    ClipFormat::Rtf,
    ClipFormat::Html,
    ClipFormat::UriList,
    ClipFormat::Utf8Text,
];

/// Rank of `format` in [`PREFERENCE`] (lower = better); `None` if not a known
/// pullable format.
fn preference_rank(format: ClipFormat) -> Option<usize> {
    PREFERENCE.iter().position(|f| *f == format)
}

/// The clipboard sync state machine for one device. Generic-free; all I/O is injected
/// per call so the engine stays a pure value.
pub struct ClipboardEngine {
    device_id: DeviceId,
    local_os: Os,
    settings: ClipboardSettings,
    /// Recently-applied peer representations used to suppress the local echo.
    applied: AppliedLog,
    /// In-flight inbound pulls keyed by origin, format, and content hash.
    pending: PendingPulls,
    /// Caller-driven logical tick used to stamp pull deadlines.
    now_tick: u64,
}

impl ClipboardEngine {
    /// Build an engine for `device_id` running on `local_os` with `settings`.
    #[must_use]
    pub fn new(device_id: DeviceId, local_os: Os, settings: ClipboardSettings) -> Self {
        Self {
            device_id,
            local_os,
            settings,
            applied: AppliedLog::default(),
            pending: PendingPulls::default(),
            now_tick: 0,
        }
    }

    /// This device's id (the `origin` it stamps on its offers).
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Current settings.
    #[must_use]
    pub fn settings(&self) -> &ClipboardSettings {
        &self.settings
    }

    /// Replace the settings (the UI edited the Clipboard section). Takes effect on the
    /// next message; in-flight pulls are unaffected.
    pub fn set_settings(&mut self, settings: ClipboardSettings) {
        self.settings = settings;
    }

    // --- Local change → Offer -------------------------------------------------------

    /// Build a `ClipboardOffer` from the local clipboard's current representations.
    ///
    /// Each [`LocalRepr`] is canonicalized + hashed (§7.7); a representation whose
    /// format is disabled by the settings is skipped, and the first local echo of a
    /// representation we just applied from a peer is skipped so it is not bounced back.
    /// Returns `None` when the master switch/direction forbids offering or nothing
    /// remains to offer.
    #[must_use]
    pub fn on_local_change(&self, reps: &[LocalRepr]) -> Option<ClipboardOffer> {
        if !self.settings.can_offer() {
            return None;
        }
        let mut entries = Vec::new();
        for rep in reps {
            if !self.settings.format_enabled(rep.format) {
                continue;
            }
            let hash = content_hash(rep.format, &rep.bytes);
            if self.applied.suppress_once(rep.format, &hash) {
                // This is the clipboard-change echo from a peer-applied clip. Suppress
                // it once; a later genuine local copy of the same bytes can be offered.
                continue;
            }
            let size = canonical_size(rep.format, &rep.bytes);
            entries.push(ClipboardEntry {
                format: rep.format,
                hash: hash.to_vec(),
                size,
            });
        }
        if entries.is_empty() {
            return None;
        }
        Some(ClipboardOffer {
            entries,
            origin: self.device_id.to_vec(),
        })
    }

    // --- Inbound Offer → eager auto-pull --------------------------------------------

    /// Handle an inbound `ClipboardOffer`: eagerly choose the best representation we
    /// accept and return the `ClipboardPull` to send (§7.7 immediate sync). Returns
    /// `Ok(None)` — emit nothing — when any gate says not to pull:
    /// - the master switch is off or the direction forbids receiving;
    /// - the prefer-native-Apple rule suppresses this peer pair (`peer_os` Apple +
    ///   local Apple + setting on) so the OS Universal Clipboard carries it;
    /// - the offer came from *this* device (a reflected own-offer);
    /// - no entry is for an enabled format within `max_auto_sync_bytes`.
    ///
    /// Before choosing, the new offer **supersedes outstanding pulls from the same
    /// origin** (§7.7): any in-flight pull from `origin` whose representation the new
    /// offer no longer advertises is dropped, so stale earlier data can no longer
    /// complete and apply.
    ///
    /// On success the engine records the pull as pending so subsequent `ClipboardData`
    /// for that `(origin, format, hash)` is accepted. A fresh offer for an already
    /// pending representation resets the reassembly and re-issues the pull.
    pub fn on_offer(
        &mut self,
        offer: &ClipboardOffer,
        peer_os: Os,
    ) -> Result<Option<ClipboardPull>, ClipboardError> {
        if !self.settings.can_receive() {
            return Ok(None);
        }
        let origin = parse_device_id(&offer.origin)?;
        if origin == self.device_id {
            // Our own offer reflected back to us — never pull from ourselves.
            return Ok(None);
        }
        if prefer_native_suppresses(&self.settings, self.local_os, peer_os) {
            self.pending.remove_origin(origin);
            return Ok(None);
        }
        // A new Offer supersedes outstanding offers from that origin (§7.7): drop any
        // in-flight pull from this origin whose hash is not re-advertised in the new
        // offer, so a stale earlier pull can no longer complete and apply.
        self.supersede_origin(origin, offer);
        let Some(entry) = self.best_entry(offer) else {
            return Ok(None);
        };
        let hash = parse_hash(&entry.hash)?;
        let key = PullKey::new(origin, entry.format, hash);
        self.pending
            .insert(Pending::new(key, entry.size, peer_os, self.now_tick));
        Ok(Some(ClipboardPull {
            hash: hash.to_vec(),
            format: entry.format,
        }))
    }

    /// Pick the most-preferred entry that is enabled and within the auto-sync limit.
    /// Ties (duplicate formats) keep the first seen. Returns `None` if no known entry
    /// passes both policy gates.
    fn best_entry<'a>(&self, offer: &'a ClipboardOffer) -> Option<&'a ClipboardEntry> {
        offer
            .entries
            .iter()
            .filter(|e| self.settings.format_enabled(e.format))
            .filter(|e| self.settings.within_auto_sync_limit(e.size))
            .filter_map(|e| preference_rank(e.format).map(|r| (r, e)))
            .min_by_key(|(r, _)| *r)
            .map(|(_, e)| e)
    }

    /// Drop in-flight pulls from `origin` that the new `offer` no longer advertises
    /// (§7.7 "a new Offer supersedes outstanding offers from that origin"). Matching
    /// is by representation `(origin, format, hash)`, not hash alone.
    fn supersede_origin(&mut self, origin: DeviceId, offer: &ClipboardOffer) {
        let still_offered = offer
            .entries
            .iter()
            .filter_map(|entry| {
                parse_hash(&entry.hash)
                    .ok()
                    .map(|hash| PullKey::new(origin, entry.format, hash))
            })
            .collect::<Vec<_>>();
        self.pending.retain_origin_offer(origin, &still_offered);
    }

    // --- Inbound Pull → Data chunks -------------------------------------------------

    /// Answer an inbound `ClipboardPull` by producing the `ClipboardData` chunks to
    /// send back, drawn from `source` (§7.7). Enforces the settings **on send**: a
    /// disabled format yields nothing. Routing is **format-aware** ([`transport_for`]):
    /// a small *text* payload (≤ [`CONTROL_TEXT_CAP`]) is one message (`offset = 0`,
    /// `last = true`) for the control stream; `png`/any binary, and any payload over the
    /// cap, are split into ≤ [`MAX_DATA_CHUNK`] (1 MiB) ordered chunks for the bulk
    /// plane. Returns an empty vec when the format is disabled or `source` no longer
    /// holds the content (the clipboard moved on).
    pub fn on_pull<S: ClipContentSource>(
        &self,
        pull: &ClipboardPull,
        source: &S,
    ) -> Result<Vec<ClipboardData>, ClipboardError> {
        if !self.settings.can_offer() {
            // If we can't offer, we shouldn't be serving pulls either.
            return Ok(Vec::new());
        }
        if !self.settings.format_enabled(pull.format) {
            return Ok(Vec::new());
        }
        let hash = parse_hash(&pull.hash)?;
        let Some(bytes) = source.canonical_bytes(pull.format, &hash) else {
            return Ok(Vec::new());
        };
        Ok(chunk_data(&hash, pull.format, &bytes))
    }

    // --- Inbound Data → reassemble/verify/apply -------------------------------------

    /// Feed one inbound `ClipboardData` chunk into the matching pending pull (§7.7).
    ///
    /// Returns the completed [`AppliedClip`] once the final chunk arrives and the
    /// reassembled SHA-256 verifies against the offered hash; the applied
    /// `(origin, format, hash)` is recorded so the local echo is suppressed once.
    /// Returns `Ok(None)` while more chunks are expected. A hash mismatch (or any
    /// framing fault) drops the pending payload and surfaces the error so the caller can
    /// clear the "pasting…" indicator. A chunk for an unknown hash is a protocol error.
    ///
    /// This compatibility method resolves by `(format, hash)` and succeeds only when
    /// there is a single matching pending origin. Call [`Self::on_data_from`] when the
    /// caller knows the peer origin.
    ///
    /// The receive gates are **re-checked on apply** (§7.7 "enforced … on receipt …
    /// before any platform clipboard write"): if the master switch, the receive
    /// direction, or this format's per-format gate was turned off mid-stream, the
    /// completed payload is **dropped** (no `AppliedClip`) and the pending slot cleared,
    /// so a setting change always takes effect before the OS clipboard is written.
    pub fn on_data(&mut self, data: &ClipboardData) -> Result<Option<AppliedClip>, ClipboardError> {
        let hash = parse_hash(&data.hash)?;
        let key = self.pending.key_for_data(data.format, hash)?;
        self.on_data_for_key(key, data)
    }

    /// Origin-aware form of [`Self::on_data`]. Runtime callers should prefer this when
    /// dispatching data received on a peer connection, because multiple origins may
    /// legitimately advertise the same `(format, hash)`.
    pub fn on_data_from(
        &mut self,
        origin: DeviceId,
        data: &ClipboardData,
    ) -> Result<Option<AppliedClip>, ClipboardError> {
        let hash = parse_hash(&data.hash)?;
        let key = self
            .pending
            .key_for_origin_data(origin, data.format, hash)?;
        self.on_data_for_key(key, data)
    }

    fn on_data_for_key(
        &mut self,
        key: PullKey,
        data: &ClipboardData,
    ) -> Result<Option<AppliedClip>, ClipboardError> {
        let Some(pending) = self.pending.get_mut(&key) else {
            return Err(ClipboardError::UnknownHash);
        };
        match pending.push(data.offset, &data.data, data.last) {
            Ok(Some(bytes)) => {
                let format = pending.format();
                let origin = pending.origin();
                let peer_os = pending.peer_os();
                self.pending.remove(&key);
                if !self.settings.can_receive()
                    || !self.settings.format_enabled(format)
                    || prefer_native_suppresses(&self.settings, self.local_os, peer_os)
                {
                    // A gate was disabled mid-stream: drop the completed payload without
                    // applying it (and without tagging it applied, since nothing was
                    // written).
                    return Ok(None);
                }
                // Tag the applied content so on_local_change won't bounce back its echo.
                self.applied.record(origin, format, key.hash);
                Ok(Some(AppliedClip { format, bytes }))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                // Drop the pending payload on any fault (mismatch/oversize/gap): the
                // partial bytes must not be applied and the slot is cleared.
                self.pending.remove(&key);
                Err(e)
            }
        }
    }

    /// Progress of the in-flight pull for `hash`, if one exists (§7.7 wait indicator).
    #[must_use]
    pub fn progress(&self, hash: &Hash) -> Option<Progress> {
        self.pending.progress_by_hash(hash)
    }

    /// Whether a pull for `hash` is currently in flight.
    #[must_use]
    pub fn is_pulling(&self, hash: &Hash) -> bool {
        self.pending.contains_hash(hash)
    }

    /// Abort every in-flight pull for `hash`, clearing wait/progress state.
    pub fn abort_pull(&mut self, hash: &Hash) -> bool {
        self.pending.remove_hash(hash)
    }

    /// Abort all in-flight pulls from `origin`, such as after peer disconnect.
    pub fn abort_origin(&mut self, origin: DeviceId) -> usize {
        self.pending.remove_origin(origin)
    }

    /// Advance the caller-driven logical clock and clear stalled pull deadlines.
    ///
    /// The engine remains clock-free: callers pass a monotonically increasing tick
    /// value from their runtime. Returns the number of pending pulls swept.
    pub fn tick(&mut self, now_tick: u64) -> usize {
        self.now_tick = self.now_tick.max(now_tick);
        self.pending.expire_stalled(self.now_tick)
    }

    /// Whether `hash` is in the bounded recently-applied log.
    #[must_use]
    pub fn was_applied(&self, hash: &Hash) -> bool {
        self.applied.contains_hash(hash)
    }

    /// Number of entries currently retained in the bounded applied log.
    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }
}

/// Canonical byte length of a representation (the `size` an offer advertises and the
/// reassembly bounds against).
fn canonical_size(format: ClipFormat, bytes: &[u8]) -> u64 {
    crate::canonical::canonical(format, bytes).len() as u64
}

/// Whether `format` is a text representation eligible for the one-shot control-stream
/// path (§7.7 "text formats within the control-stream cap"): `utf8_text` / `html` /
/// `rtf` / `uri_list` are UTF-8 text; `png` (and any binary / `Unknown`) is not and
/// always rides the bulk plane as chunks.
fn is_text_format(format: ClipFormat) -> bool {
    matches!(
        format,
        ClipFormat::Utf8Text | ClipFormat::Html | ClipFormat::Rtf | ClipFormat::UriList
    )
}

/// The transport plane a representation's `ClipboardData` is destined for (§7.7).
/// Decided by [`transport_for`] from the format and canonical size; the engine produces
/// the chunks while the caller sends them on the matching connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The interactive **control stream**, as a single `ClipboardData` (small text).
    Control,
    /// The **bulk** connection, as ordered ≤ [`MAX_DATA_CHUNK`] chunks (`png`, any
    /// binary, or any payload over [`CONTROL_TEXT_CAP`]).
    Bulk,
}

/// The §7.7 transport-by-size/format decision and single source of truth for routing: a
/// **text** payload at or below [`CONTROL_TEXT_CAP`] rides [`Transport::Control`] as one
/// message; `png` / any non-text payload, and any payload over the cap, ride
/// [`Transport::Bulk`] as ordered chunks. A future net layer calls this to pick the
/// connection; [`chunk_data`] uses it to shape the `ClipboardData`.
#[must_use]
pub fn transport_for(format: ClipFormat, canonical_size: usize) -> Transport {
    if is_text_format(format) && canonical_size <= CONTROL_TEXT_CAP {
        Transport::Control
    } else {
        Transport::Bulk
    }
}

/// Split canonical `bytes` into ordered `ClipboardData` chunks for `hash`/`format`.
///
/// Routing is **format-aware** (§7.7) via [`transport_for`]: only a *text* payload at or
/// below [`CONTROL_TEXT_CAP`] ([`Transport::Control`]) becomes a single control-stream
/// message (`offset = 0`, `last = true`). `png` (and any non-text payload) is **always**
/// [`Transport::Bulk`] and chunked into ≤ [`MAX_DATA_CHUNK`] (1 MiB) ordered chunks even
/// when small. A larger text payload is likewise chunked. An empty bulk payload still
/// emits one terminal chunk.
fn chunk_data(hash: &Hash, format: ClipFormat, bytes: &[u8]) -> Vec<ClipboardData> {
    if transport_for(format, bytes.len()) == Transport::Control {
        return vec![ClipboardData {
            hash: hash.to_vec(),
            format,
            offset: 0,
            data: bytes.to_vec(),
            last: true,
        }];
    }
    if bytes.is_empty() {
        // A non-text empty payload still needs one terminal chunk (the loop below would
        // emit none): `offset = 0`, `last = true`.
        return vec![ClipboardData {
            hash: hash.to_vec(),
            format,
            offset: 0,
            data: Vec::new(),
            last: true,
        }];
    }
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let end = offset.saturating_add(MAX_DATA_CHUNK).min(bytes.len());
        let slice = bytes.get(offset..end).unwrap_or_default();
        let last = end == bytes.len();
        out.push(ClipboardData {
            hash: hash.to_vec(),
            format,
            offset: offset as u64,
            data: slice.to_vec(),
            last,
        });
        offset = end;
    }
    out
}

/// Parse a wire `device_id` (CBOR byte string) into a [`DeviceId`]; a wrong length is
/// a protocol fault.
fn parse_device_id(bytes: &[u8]) -> Result<DeviceId, ClipboardError> {
    DeviceId::try_from(bytes).map_err(|_| ClipboardError::Protocol("origin is not 32 bytes".into()))
}

/// Parse a wire `hash` (CBOR byte string) into a 32-byte [`Hash`]; a wrong length is a
/// protocol fault.
fn parse_hash(bytes: &[u8]) -> Result<Hash, ClipboardError> {
    Hash::try_from(bytes).map_err(|_| ClipboardError::Protocol("hash is not 32 bytes".into()))
}
