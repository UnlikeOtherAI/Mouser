//! Wire enums (Appendix C). Each encodes as its unsigned integer discriminant via a
//! custom `serde` (de)serializer that maps an unrecognized value to `Unknown` instead
//! of erroring. `serde_repr` is intentionally avoided: its derive errors on unknown
//! discriminants, which would break the §2 forward-compatibility guarantee.

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
        // `From<u16>` (Error = Infallible), which maps unknown discriminants to
        // `Unknown` without ever erroring — the §2 forward-compatibility guarantee.
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
