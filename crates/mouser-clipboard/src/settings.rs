//! Per-device clipboard settings and the policy gates derived from them (§7.7).
//!
//! These settings are **replicated per device, not cluster-wide**: each member keeps
//! its own copy and the engine enforces them **on send** (don't offer a disabled
//! format) **and on receipt** (don't pull/apply a disabled format), in core, before
//! any platform clipboard write (§9). With the master switch off, no offer is sent and
//! inbound offers are ignored.

use mouser_protocol::{ClipFormat, Os};

/// Which way clipboard content may flow for this device (§7.7 `direction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SyncDirection {
    /// Offer locally-copied content *and* pull/apply inbound content.
    #[default]
    Bidirectional,
    /// Only offer locally-copied content; never pull/apply inbound offers.
    SendOnly,
    /// Only pull/apply inbound content; never offer locally-copied content.
    ReceiveOnly,
}

impl SyncDirection {
    /// Whether this device may **send** (advertise a local `ClipboardOffer`).
    #[must_use]
    pub fn allows_send(self) -> bool {
        matches!(self, Self::Bidirectional | Self::SendOnly)
    }

    /// Whether this device may **receive** (pull + apply an inbound offer).
    #[must_use]
    pub fn allows_receive(self) -> bool {
        matches!(self, Self::Bidirectional | Self::ReceiveOnly)
    }
}

/// The clipboard section of a device's settings (§7.7). All fields are local policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardSettings {
    /// Master on/off. When `false`: no offer is sent and inbound offers are ignored.
    pub shared_clipboard: bool,
    /// Per-format gate: allow `utf8_text` / `html` / `rtf`.
    pub sync_text: bool,
    /// Per-format gate: allow `png` images.
    pub sync_images: bool,
    /// Per-format gate: allow `uri_list` (file references).
    pub sync_files: bool,
    /// Skip eager auto-pull for any representation whose `size` exceeds this many
    /// bytes (`0` = unlimited).
    pub max_auto_sync_bytes: u64,
    /// Prefer the OS Universal Clipboard between two Apple devices (default on): when
    /// both ends are `macos`/`ios`, suppress Mouser's own sync for that peer pair.
    pub prefer_native_apple: bool,
    /// Direction the clipboard may flow for this device.
    pub direction: SyncDirection,
}

impl Default for ClipboardSettings {
    /// Spec defaults: sharing on, all formats on, unlimited size, prefer-native on,
    /// bidirectional.
    fn default() -> Self {
        Self {
            shared_clipboard: true,
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_auto_sync_bytes: 0,
            prefer_native_apple: true,
            direction: SyncDirection::Bidirectional,
        }
    }
}

impl ClipboardSettings {
    /// Whether `format` is allowed by the per-format gates (independent of direction
    /// or the master switch). Maps each format to its gate per §7.7:
    /// text/html/rtf → `sync_text`, png → `sync_images`, uri_list → `sync_files`.
    /// An `Unknown` format is never allowed.
    #[must_use]
    pub fn format_enabled(&self, format: ClipFormat) -> bool {
        match format {
            ClipFormat::Utf8Text | ClipFormat::Html | ClipFormat::Rtf => self.sync_text,
            ClipFormat::Png => self.sync_images,
            ClipFormat::UriList => self.sync_files,
            ClipFormat::Unknown => false,
        }
    }

    /// Whether the local device may advertise an offer at all: master on **and** the
    /// direction permits sending.
    #[must_use]
    pub fn can_offer(&self) -> bool {
        self.shared_clipboard && self.direction.allows_send()
    }

    /// Whether the local device may auto-pull/apply an inbound offer at all: master on
    /// **and** the direction permits receiving.
    #[must_use]
    pub fn can_receive(&self) -> bool {
        self.shared_clipboard && self.direction.allows_receive()
    }

    /// Whether eager auto-pull is permitted for a representation of `size` bytes given
    /// `max_auto_sync_bytes` (`0` = unlimited). The boundary is inclusive: a payload
    /// exactly at the limit is allowed; only one *over* it is skipped.
    #[must_use]
    pub fn within_auto_sync_limit(&self, size: u64) -> bool {
        self.max_auto_sync_bytes == 0 || size <= self.max_auto_sync_bytes
    }
}

/// Whether an OS value denotes an Apple platform (`macos` / `ios`) for the
/// prefer-native rule (§7.7).
#[must_use]
pub fn is_apple(os: Os) -> bool {
    matches!(os, Os::Macos | Os::Ios)
}

/// The **prefer-native** decision for a peer pair (§7.7): when `prefer_native_apple`
/// is on and **both** the local and peer OS are Apple, Mouser suppresses its own
/// clipboard sync so the OS Universal Clipboard (Handoff/Continuity) carries it.
/// Returns `true` when Mouser should *suppress* (emit nothing) for this peer.
///
/// Suppression is **per peer pair**, not global: in a `macos + ios + windows` cluster
/// `macos↔ios` suppresses while `macos↔windows` and `ios↔windows` do not. Computed
/// symmetrically on both ends from the `os` each advertised at handshake (§7.1).
#[must_use]
pub fn prefer_native_suppresses(settings: &ClipboardSettings, local: Os, peer: Os) -> bool {
    settings.prefer_native_apple && is_apple(local) && is_apple(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_gates() {
        assert!(SyncDirection::Bidirectional.allows_send());
        assert!(SyncDirection::Bidirectional.allows_receive());
        assert!(SyncDirection::SendOnly.allows_send());
        assert!(!SyncDirection::SendOnly.allows_receive());
        assert!(!SyncDirection::ReceiveOnly.allows_send());
        assert!(SyncDirection::ReceiveOnly.allows_receive());
    }

    #[test]
    fn format_gates_map_to_the_right_switch() {
        let s = ClipboardSettings {
            sync_text: true,
            sync_images: false,
            sync_files: false,
            ..ClipboardSettings::default()
        };
        assert!(s.format_enabled(ClipFormat::Utf8Text));
        assert!(s.format_enabled(ClipFormat::Html));
        assert!(s.format_enabled(ClipFormat::Rtf));
        assert!(!s.format_enabled(ClipFormat::Png));
        assert!(!s.format_enabled(ClipFormat::UriList));
        assert!(!s.format_enabled(ClipFormat::Unknown));
    }

    #[test]
    fn master_switch_gates_both_directions() {
        let off = ClipboardSettings {
            shared_clipboard: false,
            ..ClipboardSettings::default()
        };
        assert!(!off.can_offer());
        assert!(!off.can_receive());
    }

    #[test]
    fn auto_sync_limit_is_inclusive_and_zero_is_unlimited() {
        let limited = ClipboardSettings {
            max_auto_sync_bytes: 1024,
            ..ClipboardSettings::default()
        };
        assert!(limited.within_auto_sync_limit(1024)); // exactly at limit: allowed
        assert!(!limited.within_auto_sync_limit(1025)); // over: skipped
        let unlimited = ClipboardSettings::default();
        assert!(unlimited.within_auto_sync_limit(u64::MAX));
    }

    #[test]
    fn prefer_native_only_suppresses_apple_to_apple() {
        let s = ClipboardSettings::default(); // prefer_native_apple = true
        assert!(prefer_native_suppresses(&s, Os::Macos, Os::Ios));
        assert!(prefer_native_suppresses(&s, Os::Ios, Os::Macos));
        assert!(prefer_native_suppresses(&s, Os::Macos, Os::Macos));
        // any non-Apple end → Mouser carries it.
        assert!(!prefer_native_suppresses(&s, Os::Macos, Os::Windows));
        assert!(!prefer_native_suppresses(&s, Os::Linux, Os::Ios));
        assert!(!prefer_native_suppresses(&s, Os::Windows, Os::Linux));
    }

    #[test]
    fn prefer_native_off_never_suppresses() {
        let s = ClipboardSettings {
            prefer_native_apple: false,
            ..ClipboardSettings::default()
        };
        assert!(!prefer_native_suppresses(&s, Os::Macos, Os::Ios));
    }
}
