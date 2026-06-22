//! Wire enums (Appendix C). Scalar enums encode as their unsigned integer
//! discriminant via a custom `serde` (de)serializer that maps an unrecognized
//! value to `Unknown` (never erroring). `serde_repr` is intentionally avoided: its
//! derive errors on unknown discriminants, which would break the §2 forward-compat
//! guarantee.
//!
//! `Capability` is special: it is a **set member** with no `Unknown` sentinel — an
//! unrecognized member is *dropped* from the set rather than retained. See
//! [`CapabilitySet`].

use serde::Deserialize;
use std::collections::BTreeSet;

macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $val:literal),+ $(,)? } unknown = $unk:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
        #[serde(into = "u16")]
        #[repr(u16)]
        pub enum $name {
            $($variant = $val,)+
            /// Unrecognized discriminant (forward-compatibility sentinel).
            Unknown = $unk,
        }

        impl From<$name> for u16 {
            fn from(value: $name) -> Self {
                value as u16
            }
        }

        // Map any wire integer to a variant, saturating unrecognized values — and
        // critically any value outside `u16` (negative, or ≥65536) — to `Unknown`.
        // Takes `i128` so it never narrows-then-errors (H7); the §2 forward-compat
        // guarantee holds for *any* CBOR integer magnitude or sign.
        impl From<i128> for $name {
            fn from(value: i128) -> Self {
                match value {
                    $($val => $name::$variant,)+
                    _ => $name::Unknown,
                }
            }
        }

        // Custom deserialize (H7): read a full CBOR item, then map. `serde(try_from =
        // "u16")` is intentionally avoided — it forces the deserializer to produce a
        // `u16` first, so a CBOR int ≥65536 or a negative int hard-errors *before* our
        // mapping runs. Reading `ciborium::Value` and widening to `i128` instead means
        // an out-of-range or unrecognized discriminant decodes to `Unknown`, never an
        // error.
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ok($crate::enums::wire_int(deserializer)?.into())
            }
        }
    };
}

/// Deserialize a wire enum's integer discriminant as `i128`, tolerating any width or
/// sign: an out-of-range or negative *integer* widens here and is mapped to `Unknown`
/// by `From<i128>` (§2 forward-compatibility). A **non-integer** CBOR item is malformed
/// — §0.1 requires an integer discriminant — so it is rejected as a decode error rather
/// than silently becoming `Unknown`.
fn wire_int<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
    let value = ciborium::value::Value::deserialize(deserializer)?;
    value.as_integer().map(i128::from).ok_or_else(|| {
        <D::Error as serde::de::Error>::custom("enum discriminant must be an integer")
    })
}

wire_enum!(
    /// Operating system (Appendix C).
    Os { Macos = 0, Windows = 1, Linux = 2, Ios = 3, Android = 4 } unknown = 255
);
wire_enum!(
    /// Coordinator eligibility role (Appendix C).
    Role { Eligible = 0, Ineligible = 1 } unknown = 255
);
wire_enum!(
    /// `HelloAck` status (Appendix C).
    AckStatus { Accepted = 0, Rejected = 1, Pending = 2 } unknown = 255
);
wire_enum!(
    /// Reason a peer is leaving (Appendix C).
    GoodbyeReason { Shutdown = 0, Sleep = 1, UserQuit = 2, NetworkLeave = 3 } unknown = 255
);
wire_enum!(
    /// Why ownership is being transferred (Appendix C).
    TransferReason { EdgeCross = 0, Hotkey = 1, UiSelect = 2, LocalReclaim = 3 } unknown = 255
);
wire_enum!(
    /// Focus state of a device (Appendix C).
    FocusKind { Active = 0, Standby = 1, Disconnected = 2, InputBlocked = 3 } unknown = 255
);
wire_enum!(
    /// Capture/injection capability state (Appendix C).
    CapState { Available = 0, PermissionMissing = 1, SecureContext = 2, Unsupported = 3 } unknown = 255
);
wire_enum!(
    /// Why input is currently blocked (Appendix C).
    BlockedReason {
        None = 0, SecureDesktop = 1, LockScreen = 2, SecureInputField = 3,
        Permission = 4, CompositorUnsupported = 5
    } unknown = 255
);
wire_enum!(
    /// Clipboard payload format (Appendix C).
    ClipFormat { Utf8Text = 0, Html = 1, Png = 2, UriList = 3, Rtf = 4 } unknown = 255
);
wire_enum!(
    /// Scroll delta unit (Appendix C).
    ScrollUnit { Detent120 = 0, LogicalPixel = 1 } unknown = 255
);
wire_enum!(
    /// Pointer-motion mode requested by the target (Appendix C).
    PointerMode { Absolute = 0, Relative = 1 } unknown = 255
);
wire_enum!(
    /// Notification kind (Appendix C).
    NotifyKind {
        DeviceConnected = 0, DeviceDisconnected = 1, ConfigChanged = 2, CoordinatorChanged = 3
    } unknown = 255
);

/// A negotiated capability (Appendix C). Unlike the scalar enums this has **no**
/// `Unknown` sentinel: it only ever appears inside a [`CapabilitySet`], where an
/// unrecognized member is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum Capability {
    Keyboard = 0,
    Mouse = 1,
    Clipboard = 2,
    FileTransfer = 3,
    Webcam = 4,
    Audio = 5,
    CoordinatorEligible = 6,
    RemoteControlOnly = 7,
}

impl Capability {
    /// Map a wire discriminant to a capability, or `None` if unrecognized.
    pub fn from_u16(value: u16) -> Option<Self> {
        Some(match value {
            0 => Self::Keyboard,
            1 => Self::Mouse,
            2 => Self::Clipboard,
            3 => Self::FileTransfer,
            4 => Self::Webcam,
            5 => Self::Audio,
            6 => Self::CoordinatorEligible,
            7 => Self::RemoteControlOnly,
            _ => return None,
        })
    }
}

impl From<Capability> for u16 {
    fn from(value: Capability) -> Self {
        value as u16
    }
}

/// `set<Capability>` (§0.1): a CBOR array of integer discriminants, ascending and
/// de-duplicated, where an unrecognized member is **dropped** on decode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilitySet(pub BTreeSet<Capability>);

impl serde::Serialize for CapabilitySet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for capability in &self.0 {
            seq.serialize_element(&u16::from(*capability))?;
        }
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for CapabilitySet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // §0.1/§2: an unrecognized member is **dropped**, never an error. Decode each
        // element as a wide `i128` (like the scalar enums, H7) so a discriminant outside
        // `u16` or a negative one maps to "unknown → drop" instead of failing the whole
        // set; only a non-integer member is malformed and errors.
        let raw = Vec::<ciborium::value::Value>::deserialize(deserializer)?;
        let mut set = BTreeSet::new();
        for value in raw {
            // A non-integer member is malformed (§0.1 requires integer discriminants);
            // an out-of-`u16`/negative/unknown-but-in-range integer is dropped.
            let n = value.as_integer().map(i128::from).ok_or_else(|| {
                <D::Error as serde::de::Error>::custom("capability discriminant must be an integer")
            })?;
            if let Ok(u) = u16::try_from(n) {
                if let Some(cap) = Capability::from_u16(u) {
                    set.insert(cap);
                }
            }
        }
        Ok(CapabilitySet(set))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{from_cbor, to_cbor};

    #[test]
    fn known_discriminant_roundtrips() {
        let enc = to_cbor(&AckStatus::Pending).expect("encode");
        assert_eq!(enc, [0x02], "AckStatus::Pending encodes as CBOR uint 2");
        let back: AckStatus = from_cbor(&enc).expect("decode");
        assert_eq!(back, AckStatus::Pending);
    }

    #[test]
    fn in_range_unknown_discriminant_maps_not_errors() {
        // CBOR uint 99 (0x18 0x63): unrecognized but within u16 → Unknown.
        let v: AckStatus = from_cbor(&[0x18, 0x63]).expect("forward-compat decode");
        assert_eq!(v, AckStatus::Unknown);
    }

    #[test]
    fn discriminant_300_maps_to_unknown() {
        // CBOR uint 300 = 0x19 0x01 0x2C. Previously `try_from="u16"` would have
        // erred; now it decodes to Unknown (H7).
        let v: AckStatus = from_cbor(&[0x19, 0x01, 0x2C]).expect("forward-compat decode");
        assert_eq!(v, AckStatus::Unknown);
    }

    #[test]
    fn discriminant_above_u16_maps_to_unknown() {
        // CBOR uint 65536 = 0x1A 0x00 0x01 0x00 0x00 — out of u16 range, must NOT
        // error (H7); decodes to Unknown.
        let v: AckStatus =
            from_cbor(&[0x1A, 0x00, 0x01, 0x00, 0x00]).expect("forward-compat decode");
        assert_eq!(v, AckStatus::Unknown);
        // And a much larger u64 likewise.
        let big = to_cbor(&u64::from(u32::MAX)).expect("encode");
        let v2: AckStatus = from_cbor(&big).expect("forward-compat decode");
        assert_eq!(v2, AckStatus::Unknown);
    }

    #[test]
    fn negative_discriminant_maps_to_unknown() {
        // CBOR negative int -1 = 0x20 — must map to Unknown, never error (H7).
        let v: AckStatus = from_cbor(&[0x20]).expect("forward-compat decode");
        assert_eq!(v, AckStatus::Unknown);
    }

    #[test]
    fn non_integer_is_rejected() {
        // A non-integer CBOR item (text "x" = 0x61 0x78) is not an integer discriminant
        // (§0.1) — it is malformed, so decode errors. This is distinct from an unknown
        // *integer* discriminant, which maps to Unknown (tested above).
        let r: Result<AckStatus, _> = from_cbor(&[0x61, 0x78]);
        assert!(r.is_err(), "non-integer enum value must be a decode error");
    }
}
