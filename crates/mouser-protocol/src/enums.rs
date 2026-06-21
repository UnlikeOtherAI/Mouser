//! Wire enums (Appendix C). Scalar enums encode as their unsigned integer
//! discriminant via a custom `serde` (de)serializer that maps an unrecognized
//! value to `Unknown` (never erroring). `serde_repr` is intentionally avoided: its
//! derive errors on unknown discriminants, which would break the §2 forward-compat
//! guarantee.
//!
//! `Capability` is special: it is a **set member** with no `Unknown` sentinel — an
//! unrecognized member is *dropped* from the set rather than retained. See
//! [`CapabilitySet`].

use std::collections::BTreeSet;

macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $val:literal),+ $(,)? } unknown = $unk:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(into = "u16", try_from = "u16")]
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

        // `serde(try_from = "u16")` uses the std blanket `TryFrom` derived from this
        // `From<u16>` (Error = Infallible), mapping unknown discriminants to `Unknown`
        // without ever erroring — the §2 forward-compatibility guarantee.
        impl From<u16> for $name {
            fn from(value: u16) -> Self {
                match value {
                    $($val => $name::$variant,)+
                    _ => $name::Unknown,
                }
            }
        }
    };
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
        let raw = Vec::<u16>::deserialize(deserializer)?;
        Ok(CapabilitySet(
            raw.into_iter().filter_map(Capability::from_u16).collect(),
        ))
    }
}
