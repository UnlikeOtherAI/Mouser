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
//!   representation the engine just *applied* (`(origin, hash)`) is suppressed so an
//!   applied clip is never re-offered (loop prevention).
//! - **Inbound Offer → eager auto-pull** ([`ClipboardEngine::on_offer`]): pick the
//!   best accepted representation and emit `ClipboardPull` immediately, subject to the
//!   per-format gates, the auto-sync size limit, and the prefer-native-Apple rule.
//! - **Inbound Pull → Data chunks** ([`ClipboardEngine::on_pull`]): produce ordered
//!   `ClipboardData` for a representation we offered (one control-stream message for
//!   small payloads, ≤1 MiB bulk chunks for large ones).
//! - **Inbound Data → reassemble/verify/apply** ([`ClipboardEngine::on_data`]):
//!   accumulate by hash, expose [`Progress`], verify the hash on `last`, and surface
//!   the completed [`AppliedClip`] (tagging `(origin, hash)` so it is not re-offered).

use std::collections::HashMap;

use mouser_core::DeviceId;
use mouser_protocol::{
    ClipFormat, ClipboardData, ClipboardEntry, ClipboardOffer, ClipboardPull, Os,
};

use crate::canonical::content_hash;
use crate::reassembly::{Progress, Reassembly};
use crate::settings::prefer_native_suppresses;
use crate::source::{AppliedClip, ClipContentSource, LocalRepr};
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

/// One in-flight inbound pull. Stored in [`ClipboardEngine::pending`] keyed by content
/// hash; `origin` is retained so a later offer from the same origin can supersede a
/// stale pull (§7.7) and so the applied content can be tagged `(origin, hash)`.
struct Pending {
    origin: DeviceId,
    reasm: Reassembly,
}

/// The clipboard sync state machine for one device. Generic-free; all I/O is injected
/// per call so the engine stays a pure value.
pub struct ClipboardEngine {
    device_id: DeviceId,
    local_os: Os,
    settings: ClipboardSettings,
    /// `(origin, hash)` pairs this device has applied from a peer — never re-offered
    /// (loop prevention, §7.7).
    applied: Vec<(DeviceId, Hash)>,
    /// In-flight inbound pulls keyed by content hash.
    pending: HashMap<Hash, Pending>,
}

impl ClipboardEngine {
    /// Build an engine for `device_id` running on `local_os` with `settings`.
    #[must_use]
    pub fn new(device_id: DeviceId, local_os: Os, settings: ClipboardSettings) -> Self {
        Self {
            device_id,
            local_os,
            settings,
            applied: Vec::new(),
            pending: HashMap::new(),
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
    /// format is disabled by the settings is skipped, and one whose `(origin == self,
    /// hash)`… —more precisely any `(any_origin, hash)` we just *applied*— is skipped
    /// so an applied clip is never bounced back (loop prevention). Returns `None` when
    /// the master switch/direction forbids offering or nothing remains to offer.
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
            if self.was_applied(&hash) {
                // This exact content arrived from a peer and we applied it; re-offering
                // it would loop it back to the origin. Skip every representation of it.
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
    /// - no entry is for an enabled format;
    /// - the chosen entry exceeds `max_auto_sync_bytes`.
    ///
    /// Before choosing, the new offer **supersedes outstanding pulls from the same
    /// origin** (§7.7): any in-flight pull from `origin` whose hash the new offer no
    /// longer advertises is dropped, so a stale earlier pull can no longer complete and
    /// apply.
    ///
    /// On success the engine records the pull as pending so subsequent `ClipboardData`
    /// for that hash is accepted by [`ClipboardEngine::on_data`].
    pub fn on_offer(
        &mut self,
        offer: &ClipboardOffer,
        peer_os: Os,
    ) -> Result<Option<ClipboardPull>, ClipboardError> {
        if !self.settings.can_receive() {
            return Ok(None);
        }
        if prefer_native_suppresses(&self.settings, self.local_os, peer_os) {
            return Ok(None);
        }
        let origin = parse_device_id(&offer.origin)?;
        if origin == self.device_id {
            // Our own offer reflected back to us — never pull from ourselves.
            return Ok(None);
        }
        // A new Offer supersedes outstanding offers from that origin (§7.7): drop any
        // in-flight pull from this origin whose hash is not re-advertised in the new
        // offer, so a stale earlier pull can no longer complete and apply.
        self.supersede_origin(origin, offer);
        let Some(entry) = self.best_entry(offer) else {
            return Ok(None);
        };
        if !self.settings.within_auto_sync_limit(entry.size) {
            return Ok(None);
        }
        let hash = parse_hash(&entry.hash)?;
        // A pull already in flight for this hash needn't be re-issued.
        if self.pending.contains_key(&hash) {
            return Ok(None);
        }
        self.pending.insert(
            hash,
            Pending {
                origin,
                reasm: Reassembly::new(entry.format, hash, entry.size),
            },
        );
        Ok(Some(ClipboardPull {
            hash: hash.to_vec(),
            format: entry.format,
        }))
    }

    /// Pick the most-preferred *enabled* entry from `offer` (§7.7 preference order).
    /// Ties (duplicate formats) keep the first seen. Returns `None` if no entry is for
    /// an enabled, known format.
    fn best_entry<'a>(&self, offer: &'a ClipboardOffer) -> Option<&'a ClipboardEntry> {
        offer
            .entries
            .iter()
            .filter(|e| self.settings.format_enabled(e.format))
            .filter_map(|e| preference_rank(e.format).map(|r| (r, e)))
            .min_by_key(|(r, _)| *r)
            .map(|(_, e)| e)
    }

    /// Drop in-flight pulls from `origin` that the new `offer` no longer advertises
    /// (§7.7 "a new Offer supersedes outstanding offers from that origin"). A pull whose
    /// hash *is* re-advertised in the new offer is kept (the content is still on offer,
    /// so its in-flight data is still valid). Pulls from other origins are untouched.
    fn supersede_origin(&mut self, origin: DeviceId, offer: &ClipboardOffer) {
        let still_offered: Vec<Hash> = offer
            .entries
            .iter()
            .filter_map(|e| parse_hash(&e.hash).ok())
            .collect();
        self.pending
            .retain(|hash, p| p.origin != origin || still_offered.contains(hash));
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
    /// reassembled SHA-256 verifies against the offered hash; the `(origin, hash)` is
    /// recorded so the applied content is never re-offered (loop prevention). Returns
    /// `Ok(None)` while more chunks are expected. A hash mismatch (or any framing
    /// fault) drops the pending payload and surfaces the error so the caller can clear
    /// the "pasting…" indicator. A chunk for an unknown hash (no matching pull) is a
    /// protocol error.
    ///
    /// The receive gates are **re-checked on apply** (§7.7 "enforced … on receipt …
    /// before any platform clipboard write"): if the master switch, the receive
    /// direction, or this format's per-format gate was turned off mid-stream, the
    /// completed payload is **dropped** (no `AppliedClip`) and the pending slot cleared,
    /// so a setting change always takes effect before the OS clipboard is written.
    pub fn on_data(&mut self, data: &ClipboardData) -> Result<Option<AppliedClip>, ClipboardError> {
        let hash = parse_hash(&data.hash)?;
        let pending = self
            .pending
            .get_mut(&hash)
            .ok_or(ClipboardError::UnknownHash)?;
        if data.format != pending.reasm.format() {
            // The data's format must match what we pulled for this hash.
            self.pending.remove(&hash);
            return Err(ClipboardError::Protocol("data format mismatch".into()));
        }
        match pending.reasm.push(data.offset, &data.data, data.last) {
            Ok(Some(bytes)) => {
                let format = pending.reasm.format();
                let origin = pending.origin;
                self.pending.remove(&hash);
                if !self.settings.can_receive() || !self.settings.format_enabled(format) {
                    // A gate was disabled mid-stream: drop the completed payload without
                    // applying it (and without tagging it applied, since nothing was
                    // written).
                    return Ok(None);
                }
                // Tag the applied content so on_local_change won't bounce it back.
                self.applied.push((origin, hash));
                Ok(Some(AppliedClip { format, bytes }))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                // Drop the pending payload on any fault (mismatch/oversize/gap): the
                // partial bytes must not be applied and the slot is cleared.
                self.pending.remove(&hash);
                Err(e)
            }
        }
    }

    /// Progress of the in-flight pull for `hash`, if one exists (§7.7 wait indicator).
    #[must_use]
    pub fn progress(&self, hash: &Hash) -> Option<Progress> {
        self.pending.get(hash).map(|p| p.reasm.progress())
    }

    /// Whether a pull for `hash` is currently in flight.
    #[must_use]
    pub fn is_pulling(&self, hash: &Hash) -> bool {
        self.pending.contains_key(hash)
    }

    /// Whether `(any origin, hash)` has been applied locally (loop-prevention probe).
    #[must_use]
    pub fn was_applied(&self, hash: &Hash) -> bool {
        self.applied.iter().any(|(_, h)| h == hash)
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
