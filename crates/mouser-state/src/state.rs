//! [`SharedState`] — the §7.2 replicated cluster-state CRDT.
//!
//! Wraps an automerge [`AutoCommit`] document holding **shared, non-security
//! config only** (spec Appendix A): `devices`, per-device monitor `layout`
//! (plus a monotonic `layout_rev`), `aliases`, and `input_prefs`. Permissions
//! and the trusted list are deliberately **not** here (spec §7.2/§9).

use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, Change, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT,
};

use crate::error::{StateError, StateResult};
use crate::model::{DeviceInfo, InputPrefs, Monitor};

/// Pinned CRDT format version carried by every wire op (`fmt` field, spec §7.2).
pub const STATE_FMT: u16 = 1;

// Root map keys (spec Appendix A).
const K_DEVICES: &str = "devices";
const K_LAYOUT: &str = "layout";
const K_ALIASES: &str = "aliases";
const K_INPUT_PREFS: &str = "input_prefs";
/// Single LWW register encoding `(layout_rev, editor_device_id)` as a string so
/// concurrent edits resolve deterministically by automerge conflict ordering
/// plus our own `(rev, editor)` tiebreak (see [`SharedState::layout_rev`]).
const K_LAYOUT_LWW: &str = "layout_lww";

// Sub-keys.
const K_NAME: &str = "name";
const K_OS: &str = "os";
const K_MONITORS: &str = "monitors";
const K_DISPLAY_ID: &str = "display_id";
const K_X: &str = "x";
const K_Y: &str = "y";
const K_W: &str = "w";
const K_H: &str = "h";
const K_SCALE_MILLI: &str = "scale_milli";
const K_ROTATION: &str = "rotation";
const K_EDGE_DWELL_MS: &str = "edge_dwell_ms";
const K_LOCK_ON_DRAG: &str = "lock_on_drag";
const K_CURSOR_ACCEL: &str = "cursor_accel";
const K_CMD_CTRL_SWAP: &str = "cmd_ctrl_swap";
const K_HOTKEYS: &str = "hotkeys";

/// Fixed actor id for the shared **genesis** change. Every replica builds the
/// identical genesis (same actor, same ops, same content) so the root container
/// objects (`devices`/`layout`/`aliases`/`input_prefs`) carry the **same object
/// ids everywhere**. Without a shared genesis each replica would create its own
/// root maps and a merge would discard one side's entries (LWW on the ROOT key).
const GENESIS_ACTOR: [u8; 16] = *b"mouser-state-fmt";

/// Render a 32-byte `device_id` as lowercase hex — the map-key form used by the
/// CRDT schema (spec Appendix A: `Map<device_id_hex, …>`).
#[must_use]
pub fn device_id_hex(id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        // Two lowercase hex nibbles; `write!` into a String cannot fail.
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    s
}

/// The replicated shared-cluster-state document.
///
/// Cloning is cheap-ish (it forks the automerge document). All mutators
/// auto-commit; call [`SharedState::resolve`] is unnecessary for callers — every
/// merge/apply path resolves the layout LWW register internally.
#[derive(Debug, Clone)]
pub struct SharedState {
    doc: AutoCommit,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    /// Create an empty document with the four root maps initialised and
    /// `layout_rev = 0`.
    ///
    /// The root containers come from a deterministic, byte-identical **genesis**
    /// change (fixed [`GENESIS_ACTOR`]); the returned document is then forked to
    /// a fresh random actor so this replica's later edits stay attributable and
    /// merge cleanly with peers built the same way.
    #[must_use]
    pub fn new() -> Self {
        let mut genesis = AutoCommit::new().with_actor(ActorId::from(GENESIS_ACTOR));
        // ROOT child objects; failures here are impossible on a fresh doc but we
        // stay panic-free and silently no-op on the (unreachable) error.
        let _ = genesis.put_object(ROOT, K_DEVICES, ObjType::Map);
        let _ = genesis.put_object(ROOT, K_LAYOUT, ObjType::Map);
        let _ = genesis.put_object(ROOT, K_ALIASES, ObjType::Map);
        let _ = genesis.put_object(ROOT, K_INPUT_PREFS, ObjType::Map);
        let _ = genesis.put(ROOT, K_LAYOUT_LWW, encode_lww(0, &[0u8; 32]));
        genesis.commit();
        // Fork → fresh random actor for this replica's subsequent local writes,
        // while keeping the shared genesis change as the common ancestor.
        let doc = genesis.fork();
        SharedState { doc }
    }

    /// The actor id automerge uses to attribute this replica's writes, hex-encoded.
    /// Two `SharedState`s must have distinct actors to produce independent changes
    /// (the default constructor assigns a fresh random actor).
    #[must_use]
    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    // ----- devices --------------------------------------------------------

    /// Insert or replace a device's `{name, os}` metadata.
    pub fn set_device(&mut self, id: &[u8; 32], info: &DeviceInfo) -> StateResult<()> {
        let devices = self.map_child(ROOT, K_DEVICES)?;
        let key = device_id_hex(id);
        let entry = self.ensure_map(&devices, &key)?;
        self.doc.put(&entry, K_NAME, info.name.as_str())?;
        self.doc.put(&entry, K_OS, info.os.as_str())?;
        self.doc.commit();
        Ok(())
    }

    /// Read a device's metadata, or `None` if unknown.
    pub fn device(&self, id: &[u8; 32]) -> StateResult<Option<DeviceInfo>> {
        let Some(devices) = self.opt_map_child(ROOT, K_DEVICES)? else {
            return Ok(None);
        };
        let key = device_id_hex(id);
        let Some(entry) = self.opt_map_child(&devices, &key)? else {
            return Ok(None);
        };
        let name = self.str_at(&entry, K_NAME)?.unwrap_or_default();
        let os = self.str_at(&entry, K_OS)?.unwrap_or_default();
        Ok(Some(DeviceInfo { name, os }))
    }

    /// All device ids (hex) currently present, in automerge key order.
    #[must_use]
    pub fn device_ids_hex(&self) -> Vec<String> {
        match self.opt_map_child(ROOT, K_DEVICES) {
            Ok(Some(devices)) => self.doc.keys(&devices).collect(),
            _ => Vec::new(),
        }
    }

    // ----- layout + layout_rev -------------------------------------------

    /// Replace a device's monitor layout and bump the monotonic `layout_rev`.
    ///
    /// The new revision is `max(local_rev, observed) + 1` and is tagged with
    /// `editor` so concurrent edits resolve by `(layout_rev, editor)` LWW
    /// (spec §7.4). `editor` is the device id performing the edit.
    pub fn set_layout(
        &mut self,
        device: &[u8; 32],
        editor: &[u8; 32],
        monitors: &[Monitor],
    ) -> StateResult<()> {
        let layout = self.map_child(ROOT, K_LAYOUT)?;
        let key = device_id_hex(device);
        let entry = self.ensure_map(&layout, &key)?;
        // Fresh monitors list (replace wholesale so a layout edit is atomic).
        let list = self.doc.put_object(&entry, K_MONITORS, ObjType::List)?;
        for (i, m) in monitors.iter().enumerate() {
            let item = self.doc.insert_object(&list, i, ObjType::Map)?;
            self.doc.put(&item, K_DISPLAY_ID, m.display_id as u64)?;
            self.doc.put(&item, K_X, m.x as i64)?;
            self.doc.put(&item, K_Y, m.y as i64)?;
            self.doc.put(&item, K_W, m.w as u64)?;
            self.doc.put(&item, K_H, m.h as u64)?;
            self.doc.put(&item, K_SCALE_MILLI, m.scale_milli as u64)?;
            self.doc.put(&item, K_ROTATION, m.rotation as u64)?;
        }
        let next = self.layout_rev().saturating_add(1);
        self.doc.put(ROOT, K_LAYOUT_LWW, encode_lww(next, editor))?;
        self.doc.commit();
        Ok(())
    }

    /// Read a device's monitor layout (empty vec if the device has none).
    pub fn layout(&self, device: &[u8; 32]) -> StateResult<Vec<Monitor>> {
        let Some(layout) = self.opt_map_child(ROOT, K_LAYOUT)? else {
            return Ok(Vec::new());
        };
        let key = device_id_hex(device);
        let Some(entry) = self.opt_map_child(&layout, &key)? else {
            return Ok(Vec::new());
        };
        let Some(list) = self.opt_list_child(&entry, K_MONITORS)? else {
            return Ok(Vec::new());
        };
        let len = self.doc.length(&list);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let Some(item) = self.opt_list_obj(&list, i)? else {
                continue;
            };
            out.push(Monitor {
                display_id: self.u64_at(&item, K_DISPLAY_ID)?.unwrap_or(0) as u32,
                x: self.i64_at(&item, K_X)?.unwrap_or(0) as i32,
                y: self.i64_at(&item, K_Y)?.unwrap_or(0) as i32,
                w: self.u64_at(&item, K_W)?.unwrap_or(0) as u32,
                h: self.u64_at(&item, K_H)?.unwrap_or(0) as u32,
                scale_milli: self.u64_at(&item, K_SCALE_MILLI)?.unwrap_or(0) as u32,
                rotation: self.u64_at(&item, K_ROTATION)?.unwrap_or(0) as u16,
            });
        }
        Ok(out)
    }

    /// The current monotonic layout revision, resolved by `(rev, editor)` LWW
    /// across any concurrent writers (spec §7.4). Deterministic on all replicas.
    #[must_use]
    pub fn layout_rev(&self) -> u64 {
        self.resolved_lww().0
    }

    /// The editor `device_id` (hex) that authored the winning `layout_rev`.
    #[must_use]
    pub fn layout_editor_hex(&self) -> String {
        device_id_hex(&self.resolved_lww().1)
    }

    /// Resolve the LWW register: gather all concurrent `layout_lww` values and
    /// pick the maximum by `(rev, editor_device_id)`. Because every replica runs
    /// this over the same merged op-set, the result is identical everywhere.
    fn resolved_lww(&self) -> (u64, [u8; 32]) {
        let mut best = (0u64, [0u8; 32]);
        if let Ok(values) = self.doc.get_all(ROOT, K_LAYOUT_LWW) {
            for (value, _) in values {
                if let Some(s) = value.to_str() {
                    if let Some(cand) = decode_lww(s) {
                        if cand > best {
                            best = cand;
                        }
                    }
                }
            }
        }
        best
    }

    // ----- aliases --------------------------------------------------------

    /// Set a device's optional user-chosen display alias.
    pub fn set_alias(&mut self, id: &[u8; 32], alias: &str) -> StateResult<()> {
        let aliases = self.map_child(ROOT, K_ALIASES)?;
        self.doc.put(&aliases, device_id_hex(id).as_str(), alias)?;
        self.doc.commit();
        Ok(())
    }

    /// Read a device's alias, or `None` if unset.
    pub fn alias(&self, id: &[u8; 32]) -> StateResult<Option<String>> {
        let Some(aliases) = self.opt_map_child(ROOT, K_ALIASES)? else {
            return Ok(None);
        };
        self.str_at(&aliases, &device_id_hex(id))
    }

    // ----- input_prefs ----------------------------------------------------

    /// Replace the cluster-wide input preferences.
    pub fn set_input_prefs(&mut self, prefs: &InputPrefs) -> StateResult<()> {
        let obj = self.map_child(ROOT, K_INPUT_PREFS)?;
        self.doc.put(&obj, K_EDGE_DWELL_MS, prefs.edge_dwell_ms as u64)?;
        self.doc.put(&obj, K_LOCK_ON_DRAG, prefs.lock_on_drag)?;
        self.doc.put(&obj, K_CURSOR_ACCEL, prefs.cursor_accel)?;
        self.doc.put(&obj, K_CMD_CTRL_SWAP, prefs.cmd_ctrl_swap)?;
        let hk = self.doc.put_object(&obj, K_HOTKEYS, ObjType::Map)?;
        for (action, chord) in &prefs.hotkeys {
            self.doc.put(&hk, action.as_str(), chord.as_str())?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Read the cluster-wide input preferences ([`InputPrefs::default`] if unset).
    pub fn input_prefs(&self) -> StateResult<InputPrefs> {
        let Some(obj) = self.opt_map_child(ROOT, K_INPUT_PREFS)? else {
            return Ok(InputPrefs::default());
        };
        let mut prefs = InputPrefs {
            edge_dwell_ms: self.u64_at(&obj, K_EDGE_DWELL_MS)?.unwrap_or(0) as u32,
            lock_on_drag: self.bool_at(&obj, K_LOCK_ON_DRAG)?.unwrap_or(false),
            cursor_accel: self.bool_at(&obj, K_CURSOR_ACCEL)?.unwrap_or(false),
            cmd_ctrl_swap: self.bool_at(&obj, K_CMD_CTRL_SWAP)?.unwrap_or(false),
            hotkeys: Vec::new(),
        };
        if let Some(hk) = self.opt_map_child(&obj, K_HOTKEYS)? {
            for action in self.doc.keys(&hk) {
                if let Some(chord) = self.str_at(&hk, &action)? {
                    prefs.hotkeys.push((action, chord));
                }
            }
        }
        Ok(prefs)
    }

    // ----- sync / persistence (spec §7.2 wire ops) ------------------------

    /// Full save for `StateSnapshot.full_state` (spec §7.2 `[13]`).
    pub fn snapshot(&self) -> Vec<u8> {
        // `save` needs `&mut`; clone first to keep `snapshot` ergonomically `&self`.
        let mut doc = self.doc.clone();
        doc.save()
    }

    /// Load a full document from a `StateSnapshot.full_state` payload.
    pub fn load(bytes: &[u8]) -> StateResult<Self> {
        let doc = AutoCommit::load(bytes).map_err(StateError::Automerge)?;
        Ok(SharedState { doc })
    }

    /// Current document heads (spec `StateRequest.have_heads` / `StateDelta.dep_heads`).
    #[must_use]
    pub fn heads(&self) -> Vec<ChangeHash> {
        let mut doc = self.doc.clone();
        doc.get_heads()
    }

    /// All changes this document holds that are **not** transitive dependencies
    /// of `have_heads` — the reply payload for a `StateRequest` (`StateChanges.changes`).
    /// Each entry is one encoded automerge change (`StateDelta.change` bytes).
    #[must_use]
    pub fn changes_since(&self, have_heads: &[ChangeHash]) -> Vec<Vec<u8>> {
        let mut doc = self.doc.clone();
        doc.get_changes(have_heads)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect()
    }

    /// Apply encoded changes received via `StateDelta`/`StateChanges`.
    ///
    /// Automerge buffers any change whose dependencies are not yet present and
    /// applies it automatically once they arrive (it is dependency-order
    /// tolerant), so callers may feed deltas in any order. Decode errors on a
    /// single change abort the batch without mutating the document.
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> StateResult<()> {
        let mut decoded = Vec::with_capacity(changes.len());
        for bytes in changes {
            let change = Change::from_bytes(bytes.clone())
                .map_err(|e| StateError::Decode(e.to_string()))?;
            decoded.push(change);
        }
        self.doc.apply_changes(decoded)?;
        Ok(())
    }

    /// Merge another document into this one (used for snapshot reconciliation).
    pub fn merge(&mut self, other: &SharedState) -> StateResult<()> {
        let mut other = other.doc.clone();
        self.doc.merge(&mut other)?;
        Ok(())
    }

    /// Change hashes still missing for the given heads (callers may request them).
    #[must_use]
    pub fn missing_deps(&self, heads: &[ChangeHash]) -> Vec<ChangeHash> {
        self.doc.get_missing_deps(heads)
    }

    // ----- internal helpers ----------------------------------------------

    fn map_child(&mut self, parent: ObjId, key: &str) -> StateResult<ObjId> {
        match self.doc.get(&parent, key)? {
            Some((Value::Object(ObjType::Map), id)) => Ok(id),
            Some((Value::Object(_), _)) => {
                Err(StateError::Schema(format!("{key} is not a map")))
            }
            _ => Ok(self.doc.put_object(&parent, key, ObjType::Map)?),
        }
    }

    fn ensure_map(&mut self, parent: &ObjId, key: &str) -> StateResult<ObjId> {
        match self.doc.get(parent, key)? {
            Some((Value::Object(ObjType::Map), id)) => Ok(id),
            Some((Value::Object(_), _)) => {
                Err(StateError::Schema(format!("{key} is not a map")))
            }
            _ => Ok(self.doc.put_object(parent, key, ObjType::Map)?),
        }
    }

    fn opt_map_child<O: AsRef<ObjId>>(&self, parent: O, key: &str) -> StateResult<Option<ObjId>> {
        match self.doc.get(parent.as_ref(), key)? {
            Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
            Some((Value::Object(_), _)) => {
                Err(StateError::Schema(format!("{key} is not a map")))
            }
            _ => Ok(None),
        }
    }

    fn opt_list_child(&self, parent: &ObjId, key: &str) -> StateResult<Option<ObjId>> {
        match self.doc.get(parent, key)? {
            Some((Value::Object(ObjType::List), id)) => Ok(Some(id)),
            Some((Value::Object(_), _)) => {
                Err(StateError::Schema(format!("{key} is not a list")))
            }
            _ => Ok(None),
        }
    }

    fn opt_list_obj(&self, list: &ObjId, idx: usize) -> StateResult<Option<ObjId>> {
        match self.doc.get(list, idx)? {
            Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    fn str_at(&self, parent: &ObjId, key: &str) -> StateResult<Option<String>> {
        match self.doc.get(parent, key)? {
            Some((value, _)) => Ok(value.to_str().map(str::to_owned)),
            None => Ok(None),
        }
    }

    fn u64_at(&self, parent: &ObjId, key: &str) -> StateResult<Option<u64>> {
        match self.doc.get(parent, key)? {
            Some((value, _)) => Ok(scalar_u64(&value)),
            None => Ok(None),
        }
    }

    fn i64_at(&self, parent: &ObjId, key: &str) -> StateResult<Option<i64>> {
        match self.doc.get(parent, key)? {
            Some((value, _)) => Ok(value.to_i64()),
            None => Ok(None),
        }
    }

    fn bool_at(&self, parent: &ObjId, key: &str) -> StateResult<Option<bool>> {
        match self.doc.get(parent, key)? {
            Some((value, _)) => Ok(value.to_bool()),
            None => Ok(None),
        }
    }
}

/// Read an unsigned integer regardless of whether automerge stored it as
/// `Uint` or `Int` (a positive `Int` round-trips fine).
fn scalar_u64(value: &Value<'_>) -> Option<u64> {
    if let Some(u) = value.to_u64() {
        return Some(u);
    }
    match value.to_i64() {
        Some(i) if i >= 0 => Some(i as u64),
        _ => None,
    }
}

/// Encode the LWW key `(rev, editor)` as a sortable string: a zero-padded
/// 20-digit decimal revision, a separator, then the 64-char editor hex. Plain
/// lexicographic comparison then matches the `(rev, editor)` ordering exactly.
fn encode_lww(rev: u64, editor: &[u8; 32]) -> ScalarValue {
    ScalarValue::Str(format!("{rev:020}|{}", device_id_hex(editor)).into())
}

/// Inverse of [`encode_lww`]; returns `None` on a malformed register value.
fn decode_lww(s: &str) -> Option<(u64, [u8; 32])> {
    let (rev_str, editor_hex) = s.split_once('|')?;
    let rev: u64 = rev_str.parse().ok()?;
    let editor = hex32(editor_hex)?;
    Some((rev, editor))
}

/// Parse exactly 64 lowercase/uppercase hex chars into a 32-byte array.
fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_nibble(*bytes.get(i * 2)?)?;
        let lo = hex_nibble(*bytes.get(i * 2 + 1)?)?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
