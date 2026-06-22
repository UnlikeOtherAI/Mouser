//! Plain typed views of the replicated CRDT values (spec Appendix A).
//!
//! These structs are the ergonomic surface callers read/write; the CRDT itself
//! stores them as automerge maps/lists. They carry no automerge state and are
//! cheap to clone.

/// A device's shared, non-security metadata: `devices[id] = { name, os }`
/// (spec Appendix A). The user-chosen alias is **not** here — it lives in the
/// separate `aliases` map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Human-readable device name advertised at handshake.
    pub name: String,
    /// Operating-system token (`macos`, `windows`, `linux`, `ios`, `android`).
    /// Stored as the lowercase string used in the discovery TXT record.
    pub os: String,
}

/// One monitor rectangle in the shared virtual-desktop coordinate space
/// (spec Appendix A `Monitor`). Origins are signed; sizes are unsigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Platform display identifier, unique within the owning device.
    pub display_id: u32,
    /// X origin in the shared virtual-desktop space (signed).
    pub x: i32,
    /// Y origin in the shared virtual-desktop space (signed).
    pub y: i32,
    /// Width in logical pixels.
    pub w: u32,
    /// Height in logical pixels.
    pub h: u32,
    /// Display scale ×1000 as an integer (e.g. `2000` = 2.0×).
    pub scale_milli: u32,
    /// Rotation in degrees (0/90/180/270).
    pub rotation: u16,
}

/// Cluster-wide input preferences (spec Appendix A `input_prefs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputPrefs {
    /// Dwell time at a screen edge before a cross is triggered, in ms.
    pub edge_dwell_ms: u32,
    /// Lock the pointer to the active device while a drag is in progress.
    pub lock_on_drag: bool,
    /// Apply pointer acceleration to injected motion.
    pub cursor_accel: bool,
    /// Swap Cmd and Ctrl when injecting toward a different-family OS.
    pub cmd_ctrl_swap: bool,
    /// Action-name → platform-neutral chord string (e.g. `"panic"` → `"Ctrl+Alt+P"`).
    pub hotkeys: Vec<(String, String)>,
}
