# Mouser — Communication Interface (Wire Protocol) — v2

This is the contract that lets independently-built Mouser binaries interoperate.
If two engines implement this document, they form a cluster.

> **v2** incorporates the Round 1 design review ([design-review.md](design-review.md)).
> Changes from v1 are noted inline as `(R1: Fn)`.

---

## 0. Conventions (normative)

- **Two connections per peer** (R1: F5). Each peer pair maintains:
  - an **interactive connection** — carries the control stream + the pointer-motion
    datagram plane. Its congestion window is never filled by bulk data.
  - a **bulk connection** — carries file transfer, clipboard images, and state
    snapshots. App-level rate-limited so it cannot starve the interactive path.
  Both are QUIC + TLS 1.3, share identity/pinning, and are established together.
- **Control encoding = CBOR** (RFC 8949), via a serde codec (R1: F1). CBOR is
  self-describing: decoders skip unknown map keys and tolerate added fields, which
  is what makes version skew safe. *(Protobuf is the alternative if strict
  multi-language schema governance is later required; CBOR is chosen for serde-native
  forward-compat with no codegen.)*
- **Datagram encoding = postcard**, fixed layout, for the single `PointerMotion`
  message only (tiny, frozen schema).
- **Framing (control stream)** (R1: F2): every message is
  `len: u32 (LE) | type: u16 (LE) | flags: u16 | payload[len-4]`. On an unknown
  `type`, a decoder consumes `len` bytes and continues. `flags` is reserved:
  senders MUST set 0, receivers MUST ignore unknown bits (R1: F58).
- **Enums on the wire** carry **explicit integer discriminants** (listed below) and
  every wire enum reserves `Unknown = 255` (or 65535); decoders map unrecognized
  discriminants to `Unknown` rather than erroring (R1: F1, F36).
- **Integers** are little-endian. **Timestamps** are `u64` **milliseconds**; their
  clock domain is specified per field (R1: F15). **No wire field uses another
  machine's wall clock for comparison** (R1: F7).
- **Size caps** (R1: F27): control message ≤ 256 KiB; any `String` ≤ 4 KiB; any
  `bytes` field ≤ 64 KiB except `StateSnapshot.full_state` ≤ 8 MiB and `FileChunk.data`
  ≤ 1 MiB. Receivers MUST reject oversized frames before allocating.
- **Decode discipline**: the decode path is panic-free and fuzzed in CI
  (`deny(unwrap_used, panic, indexing_slicing)`).

---

## 1. Design goals

- **Interoperable** — byte-exact framing, explicit enum discriminants, CBOR forward-compat.
- **Peer-to-peer** — no broker; state fans out over direct QUIC links + gossip.
- **Secure by default** — cert bound to identity, mandatory channel-bound pairing,
  permissions enforced on receipt.
- **Low input latency** — motion on a lossy datagram plane on a dedicated connection.

---

## 2. Versioning & capability negotiation (R1: F32)

- **Protocol version** is the QUIC **ALPN** token, e.g. `mouser/1`. Each side
  advertises **all** supported tokens (highest-preference first, e.g.
  `["mouser/2","mouser/1"]`); the TLS handshake selects the common maximum. ALPN is
  the **single source of truth** for version — there is no version field in `Hello`.
- **Capabilities** are exchanged in the authenticated `Hello` as an explicit set
  (`keyboard`, `mouse`, `clipboard`, `file_transfer`, `webcam`, `audio`,
  `coordinator_eligible`, `remote_control_only`). Behaviour = intersection.
- mDNS TXT capability hints are **advisory/UI only and untrusted** (R1: F43, X2).
- **Forward compatibility**: unknown CBOR keys/fields ignored; unknown message
  `type` skipped; unknown enum discriminants → `Unknown`. New message types are additive.

---

## 3. Device identity (R1: F8, F37)

- On first launch an engine generates a permanent **Ed25519 keypair**.
- **`device_id` = SHA-256(public_key)**, the full **32 bytes**, used for all
  comparison/pinning. A truncated base32 form is **display-only** and never compared.
- The engine's **TLS leaf certificate public key IS the Ed25519 identity key**
  (R1: F8 — the "or signed by it" option is removed). Therefore on **every**
  connection (pairing and resume) the receiver MUST verify
  `SHA-256(presented_cert_SPKI) == device_id` (via a custom `rustls`
  `ServerCertVerifier`/`ClientCertVerifier`; confirmed supported) **before**
  processing any `Hello`. Trust is keyed solely on `device_id` (R1: F36) — name and
  address are never part of the trust check.

---

## 4. Discovery (R1: F36, F51)

- **Service type:** `_mouser._udp.local` (QUIC/UDP) via mDNS / DNS-SD.
- **Instance name:** `"<display name> (<short id>)"` to guarantee uniqueness when two
  devices share a name.
- **TXT records** (each value's type is fixed; `txtvers=1`; unknown keys ignored):

  | Key | Type | Meaning |
  |-----|------|---------|
  | `txtvers` | int(ascii) | `1` |
  | `id` | base32(no-pad, lower) | full `device_id` |
  | `name` | utf8 | display name |
  | `os` | enum-str | `macos`\|`windows`\|`linux`\|`ios`\|`android` |
  | `ver` | semver | engine version (display only, R1: F-L1) |
  | `iport` | int(ascii) | interactive-connection UDP port |
  | `bport` | int(ascii) | bulk-connection UDP port |
  | `caps` | csv | advisory capability hints |
  | `role` | enum-str | `eligible`\|`ineligible` |

  TXT data is advisory; trust is established in §5.

---

## 5. Connection establishment & pairing (R1: F8, F9)

1. **QUIC handshakes** (interactive + bulk) with ALPN and TLS 1.3; cert pinning per §3.
2. **`Hello` exchange** on the interactive control stream (§7.1).
3. **First contact (pairing):** the receiver shows an approval prompt displaying the
   **cert-derived** `device_id`/fingerprint (not TXT data, R1: H1) and a **mandatory
   Short Authentication String** shown identically on both ends (R1: F9):
   `SAS = base10_6(HKDF-SHA256(ikm = TLS-exporter("mouser-sas-v1", "", 32),
   info = sorted(device_id_A, device_id_B)))` → **6 decimal digits**. The user
   compares the two screens and approves. On approval the peer `device_id` is added
   to the local **trusted list**. SAS is not optional.
4. **Identity proof:** `Hello.channel_sig = Ed25519_sign(identity_key,
   "mouser-hello-v1" || TLS-exporter("mouser-hello",32))` — a signature over the
   **TLS 1.3 exporter** (RFC 5705), binding the proof to *this* session (defeats
   relay/replay, R1: F9). (`nonce_sig`-over-transport-params from v1 is removed.)
5. **Resume:** trusted peers skip the prompt; cert pinning + `channel_sig` still verified.

`HelloAck.status ∈ {Accepted, Rejected, Pending}`; first contact returns `Pending`
while the human decides, followed by a `PairingResult` (§7.1). Pending timeout = 120 s;
on timeout/disconnect the initiator retries on reconnect (pinning prevents double-prompt).

A peer not in the trusted list may be discovered but exchanges nothing.

---

## 6. Transport planes

### 6.1 Interactive connection
- **Control stream** — one long-lived, high-priority bidirectional QUIC stream,
  reliable+ordered, framed per §0. Carries: `Hello`, ownership/focus, key/button/scroll,
  CRDT deltas *(small)*, election, clipboard *offers/text*, notifications, liveness.
- **Motion datagram plane** — QUIC DATAGRAM (RFC 9221) carrying **only**
  `PointerMotion` (§7.6). Lossy, newest-wins. If the peer/path doesn't advertise
  `max_datagram_frame_size` large enough, motion degrades onto the control stream
  with coalescing (R1: F41).

### 6.2 Bulk connection
Separate QUIC connection for `FileChunk`, large `ClipboardData` (images/files), and
`StateSnapshot`. App-level bandwidth cap (default 50% of estimated path capacity, and
hard-yields to interactive RTT spikes) so bulk never starves motion (R1: F5).

---

## 7. Message catalog

Envelope per §0. `type` discriminants are fixed (shown as `[NN]`). Field lists are normative.

### 7.1 Session & liveness
```
[01] Hello { device_id: bytes32, name: str, os: Os, engine_version: str,
             capabilities: set<Capability>, role: Role, channel_sig: bytes }
[02] HelloAck { status: AckStatus(Accepted=0,Rejected=1,Pending=2), reason?: str }
[03] PairingResult { accepted: bool, reason?: str }
[04] Ping { id: u64 }
[05] Pong { id: u64 }                       // echoes Ping.id (same-clock RTT)
[06] Heartbeat { seq: u64 }                 // interval 1s; peer Disconnected after 3 misses (R1: F40)
[07] Goodbye { reason: GoodbyeReason }      // owner Goodbye triggers handoff first (R1: F-L4)
```

### 7.2 Cluster state — CRDT replication (R1: F3, F14, F39)
Pinned CRDT: **automerge**, format `crdt_format_version = 1`. Mismatch ⇒ reject with reason.
```
[10] StateDelta   { fmt: u16, change: bytes, dep_heads: [bytes32] }  // dep_heads = causal parents
[11] StateRequest { fmt: u16, have_heads: [bytes32] }                // automerge change-hash heads
[12] StateChanges { fmt: u16, changes: [bytes] }                     // reply: changes the requester lacks
[13] StateSnapshot{ fmt: u16, full_state: bytes }                    // bulk connection; for fresh join
```
- State = **persistent config only**: layout (per-monitor, Appendix A), aliases, input
  prefs, per-device permissions, trusted list. **Liveness/presence is NOT in the CRDT**
  (R1: F39) — it is ephemeral gossip (Heartbeat/FocusState).
- A `StateDelta` whose `dep_heads` aren't satisfied is buffered until parents arrive.
- **Anti-entropy** (R1: F14): each engine sends `StateRequest` on connect and every 30 s
  to ≥1 peer; the peer replies `StateChanges`. Gossip is best-effort; this is the
  correctness backstop. Dedup by change-hash.

### 7.3 Layout & identification
```
[20] IdentifyRequest { device_id, number: u8, ttl_ms: u32 }   // auto-clears after ttl (R1: F53)
[21] IdentifyClear   { device_id }
```
(Layout changes travel **only** as `StateDelta` — the v1 `LayoutUpdate` is removed, R1: F38.)

### 7.4 Ownership & focus (R1: F6, F17)
```
[30] OwnershipTransfer { to: device_id, owner_epoch: u64, layout_version: u64,
                         reason: TransferReason(EdgeCross=0,Hotkey=1,UiSelect=2,LocalReclaim=3) }
[31] OwnershipAck      { owner_epoch: u64, accepted: bool, reason?: str }
[32] FocusState        { owner: device_id, owner_epoch: u64, state: FocusKind }
```
- Ownership is a **single token with a monotonic `owner_epoch`** (R1: F6). Only the
  current owner mints `epoch+1`. Receivers accept an `OwnershipTransfer`/`FocusState`
  only if `owner_epoch` is strictly greater than locally known; equal-epoch races are
  tie-broken by lower `device_id`. Transfer requires `OwnershipAck`.
- A non-owner wanting ownership (UI/hotkey/local-reclaim) sends `OwnershipTransfer`
  with reason; the current owner mints the next epoch and acks (eliminates two-owner races).
- `LocalReclaim` (R1: F21): a device whose **own** local input hardware is used reclaims
  ownership — this replaces v1's incoherent "WindowClick".
- On owner heartbeat-timeout, the **physically-attached** device reclaims with `epoch+1`.
- `layout_version` pins the transfer to a CRDT layout head; a receiver behind that head
  pulls via `StateRequest` before applying (R1: F17).

### 7.5 Input — reliable (control stream) (R1: F4, F25)
```
[40] KeyEvent     { usage: u16, down: bool, mods: u16, session: u32, ctr: u64 }
[41] PointerButton{ button: u8, down: bool, session: u32, ctr: u64 }
[42] Scroll       { dx: i32, dy: i32, hi_res: bool, session: u32, ctr: u64 }
```
- `usage` = **USB HID Usage ID, Usage Page 0x07** — the canonical cross-OS keycode
  namespace (Appendix B); each platform adapter maps native↔HID (R1: F4). Semantics are
  **physical-key**; the receiver applies its own layout.
- `mods` = fixed bitmask (Appendix B): bit0 LCtrl,1 LShift,2 LAlt,3 LMeta,4 RCtrl,5 RShift,
  6 RAlt,7 RMeta. `Meta` = Cmd/Win/Super, remapped per target OS policy.
- `button`: 0=left,1=right,2=middle,3=back,4=forward.
- **Anti-replay** (R1: F25): `session` is the ownership session id; `ctr` is a per-session
  monotonic counter. Receivers reject non-increasing `(session,ctr)`.

### 7.6 Pointer motion — lossy datagram (R1: F10, F15, F16)
```
PointerMotion (datagram, postcard, tag byte 0x01):
  { session: u32, seq: u32, display_id: u32, x: i32, y: i32 }
```
- `x,y` are **integer logical pixels** in the target **display's** coordinate space
  (R1: F10 — replaces v1 `f32` normalized; deterministic, no round-trip jitter).
  `display_id` selects the monitor (Appendix A geometry). Origin top-left, y-down;
  receiver clamps to actual bounds.
- `session` = ownership session id; on a new session the receiver resets `last_seq`
  (R1: F16). `seq` comparison is wraparound-safe (RFC 1982). **No `ts`** (R1: F15).
- 1-byte version tag prefixes the datagram; unknown tag ⇒ drop (R1: F-H8).
- Receiver applies only if `(session,seq)` is newer; injects the absolute position.
- Authorization (R1: F25): processed only from a trusted peer with `mouse` permission
  that is the current owner; path-validated after migration.

### 7.7 Clipboard (R1: F34)
```
[50] ClipboardOffer { entries: [{ format: ClipFormat, hash: bytes32, size: u64 }], origin: device_id }
[51] ClipboardPull  { hash: bytes32, format: ClipFormat }
[52] ClipboardData  { hash: bytes32, format: ClipFormat, data: bytes }  // bulk conn if large
```
- `hash = SHA-256(format-canonicalized bytes)` per format; puller verifies received data
  hashes to the request (drop on mismatch). A new Offer supersedes prior outstanding offers
  from that origin. Locally-applied clipboard is tagged by `(origin,hash)` and **not
  re-offered** (loop prevention). Gated by mode (off/text-only/full) + permission.

### 7.8 File transfer (bulk connection) (R1: F35)
```
[60] FileOffer  { transfer_id: u64, files: [{ name: str, size: u64 }] }
[61] FileAccept { transfer_id, resume: [{ file_index: u32, offset: u64 }] }
[62] FileReject { transfer_id, reason: str }
[63] FileChunk  { transfer_id, file_index: u32, offset: u64, data: bytes }
[64] FileAck    { transfer_id, file_index: u32, acked_through: u64 }  // cumulative bytes; window 8 MiB
[65] FileDone   { transfer_id, ok: bool }
```
- `name` is sanitized on receipt: strip path separators, reject `..`, no symlink follow;
  files land in a quarantine dir (R1: M1). Window: sender keeps ≤ 8 MiB unacked per file.

### 7.9 Notifications
```
[70] Notify { kind: NotifyKind(DeviceConnected=0,DeviceDisconnected=1,ConfigChanged=2,
              CoordinatorChanged=3), detail: str }
```
Connect/disconnect debounced; `CoordinatorChanged` off by default (election is invisible).

### 7.10 Coordinator election — lease-based (R1: F7, F19, F20, F57)
```
[80] CoordinatorLease { holder: device_id, term: u64, ttl_ms: u32 }   // NOT an absolute time
[81] CoordinatorClaim { candidate: device_id, term: u64 }
[82] CoordinatorYield { from: device_id, term: u64 }
```
- **Local-monotonic only** (R1: F7): on receipt of a Lease, a device sets
  `deadline = monotonic_now + ttl_ms`; lease "live" until then; reset on renewal. Holder
  renews at `ttl/3`. Default `ttl_ms = 6000`. `ttl_ms` is capped at 30 000.
- **Term rules (Raft-style, R1: F19)**: a candidate increments `term` on `Claim`; a holder
  receiving a strictly higher `term` immediately yields; equal `term` resolves by **lowest
  `device_id`** (the sole, deterministic tiebreak — R1: F57; "uptime/etc." removed).
- **Scope (R1: F20)**: the coordinator serializes **nothing in the steady state** — state is
  the CRDT, admission is local approval, ownership is the epoch token. It is a presented
  "who's in charge" label + optional unattended-admission fallback. All-ineligible clusters
  (e.g. two laptops) therefore operate fine with no coordinator (R1: M8).

---

## 8. Pointer-motion stream (latency detail) (R1: F5, F22)

**Sender (active device):**
- **Event-driven** (R1: F22): emit a `PointerMotion` datagram on each input event;
  coalesce (keep newest) **only** when the prior datagram hasn't flushed. Hard rate cap
  ~1000 Hz to protect the network — *not* a fixed cadence. Target ≤ 2 ms sender-side latency.
- Send on the **interactive** connection's datagram plane (never shares a congestion window
  with bulk — R1: F5).

**Receiver:** apply only if `(session,seq)` newer; inject absolute logical-pixel position
on `display_id`; clamp to bounds. Loss self-heals (absolute). Optional ≤1-frame smoothing,
off by default.

**Relative/pointer-lock consumers (games, 3D)** (R1: F23): when the target reports a
pointer-locked app, the sender switches to a cumulative-delta mode (same newest-wins seq);
otherwise pointer-lock is out of scope for v1 and surfaced to the user.

**Budget** (R1: F-L3): target end-to-end motion-to-injection ≤ 5 ms wired / ≤ 15 ms good
Wi-Fi, asserted by the integration harness.

---

## 9. Permission & authority (R1: F25, F31, F43)

- **Authorization** = `negotiated_capability ∧ local_permission`. Capabilities are advisory;
  the **local per-device permission** is authoritative.
- Permissions/trusted-list are **local policy, authored only by the device about its peers**,
  and are **NOT peer-writable CRDT** (R1: F31) — a peer cannot grant itself rights.
- Enforced **on receipt**, in core, before any platform adapter, on **both** the control
  stream and the datagram plane (R1: F25). A peer lacking a permission is dropped (rate-limited
  logging, R1: F54).
- **Trusted-but-malicious peer mitigations** (R1: F26): per-peer input rate limit + burst cap;
  "remote input only when unlocked" (default on); visible indicator + optional confirm on first
  remote ownership; peer-initiated ownership is a *request*, never an unconditional grab.

---

## 10. Abuse & DoS control (R1: F27, F28)

- QUIC **Retry**/address validation enabled (anti-amplification).
- Per-source-IP connection + `Hello` rate limits; cap concurrent untrusted connections;
  cap discovered-device list; penalize repeated trust failures.
- mDNS reaction rate-limited; record flood does not unbound the device list.
- Size caps (§0) + panic-free, fuzzed decoders.

---

## 11. Channel summary

| Data | Connection / plane | Reliability |
|------|--------------------|-------------|
| Hello, pairing, election, liveness | interactive / control | reliable, ordered |
| Ownership / focus | interactive / control | reliable, ordered, epoch-gated |
| Key / button / scroll | interactive / control | reliable, ordered, replay-checked |
| CRDT deltas (small) + anti-entropy | interactive / control | reliable + periodic sync |
| **Pointer motion** | **interactive / datagram** | **lossy, newest-wins** |
| Clipboard offers / text | interactive / control | reliable |
| Clipboard images, files, snapshots | **bulk** | reliable, rate-limited |

---

## Appendix A — CRDT document schema (normative) (R1: F3, F18)

Automerge document root keys:
- `devices: Map<device_id_hex, { name: str, os: str, alias?: str }>`
- `layout: Map<device_id_hex, { monitors: List<Monitor> }>` where
  `Monitor { display_id: u32, x: i32, y: i32, w: u32, h: u32, scale: f32, rotation: u16 }`
  in a shared virtual-desktop coordinate space (signed origins allowed).
- `permissions: Map<device_id_hex, Map<peer_id_hex, { keyboard,mouse,clipboard,
  file_transfer,webcam,audio: bool }>>` (authored only by `device_id_hex` about `peer_id_hex`).
- `input_prefs: Map<...>`; `trusted: Map<device_id_hex, List<peer_id_hex>>`.

**Post-merge normalization** (R1: F18): after applying changes, each engine deterministically
re-derives the edge-adjacency map from merged `Monitor` rects (resolve overlaps by ascending
`device_id`, then `display_id`). Identical inputs ⇒ identical edge map on every engine.

## Appendix B — Canonical keymap (normative) (R1: F4)

Wire keycodes are **USB HID Usage IDs, Usage Page 0x07** (e.g. `0x04`=a, `0x28`=Enter,
`0x29`=Esc). Each platform adapter ships a bidirectional table:
- macOS: HID ↔ CGKeyCode (Carbon virtual keycodes).
- Windows: HID ↔ scancode/VK (via `MapVirtualKey`).
- Linux: HID ↔ evdev `KEY_*` (`linux/input-event-codes.h`).
Modifiers per §7.5 `mods`. `Meta` maps to Cmd (macOS) / Win (Windows) / Super (Linux),
with an optional Cmd↔Ctrl swap preference for cross-OS muscle memory.
