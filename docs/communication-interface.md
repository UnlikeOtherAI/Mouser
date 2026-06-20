# Mouser — Communication Interface (Wire Protocol)

This is the contract that lets independently-built Mouser binaries — a Linux
build and a Windows build compiled separately on different machines —
interoperate. If two engines implement this document, they form a cluster.

It defines: identity, discovery, connection establishment, the two transport
planes, the message catalog, and the low-latency pointer-motion stream.

Companion docs: [architecture.md](architecture.md) (why) and
[tech-stack.md](tech-stack.md) (with what).

---

## 1. Design goals

- **Interoperable.** Fully specified framing + versioning; unknown fields and
  message types are ignored, not fatal.
- **Peer-to-peer.** No broker. State fans out over direct peer links + gossip.
  There is no MQTT and no central message bus.
- **Secure by default.** Identity-pinned TLS; explicit first-contact approval;
  permissions enforced on receipt.
- **Low input latency.** Pointer motion is a lossy, newest-wins datagram stream,
  separate from the reliable control stream.

---

## 2. Versioning & capability negotiation

- **Protocol version** is carried in the QUIC ALPN token: `mouser/1`. Engines
  offer the highest version they support; the connection uses the highest in
  common. A peer that supports only `mouser/1` and one that also supports
  `mouser/2` negotiate `mouser/1`.
- **Capabilities** are exchanged in `Hello` (§5.1) as an explicit set
  (`keyboard`, `mouse`, `clipboard`, `file-transfer`, `webcam`, `audio`,
  `coordinator-eligible`, `remote-control-only`). Each side filters behaviour to
  the intersection.
- **Forward compatibility:** decoders MUST ignore unknown message-type tags and
  unknown trailing fields rather than error. New message types are additive.

---

## 3. Device identity

- On first launch an engine generates a permanent **Ed25519 keypair**.
- **`device_id` = base32(SHA-256(public_key))**, truncated to 16 bytes for
  display. It is stable across reboots, IP changes, and DHCP renewals.
- The engine presents a **self-signed TLS certificate** whose key is this
  identity key (or is signed by it). Peers **pin** `device_id` after pairing; a
  changed key for a known name/address is rejected, not silently trusted.

---

## 4. Discovery

- **Service type:** `_mouser._udp.local` via mDNS / DNS-SD (QUIC is over UDP).
- **Instance name:** the human device name (e.g. `Mac Studio`).
- **TXT records:**

  | Key | Meaning |
  |-----|---------|
  | `id` | `device_id` (base32) |
  | `name` | display name |
  | `os` | `macos` \| `windows` \| `linux` \| `ios` \| `android` |
  | `ver` | engine version (semver) |
  | `proto` | max protocol version (e.g. `1`) |
  | `port` | QUIC UDP port |
  | `caps` | comma-separated capability flags |
  | `role` | `eligible` \| `ineligible` (coordinator) |
  | `fp` | short cert fingerprint (for the approval prompt) |

- Engines advertise continuously and browse continuously. Appearance/
  disappearance of records drives the live device list. Discovery only *finds*
  peers — trust is established in §5.

---

## 5. Connection establishment

1. **QUIC handshake** with ALPN `mouser/1` and TLS 1.3.
2. **`Hello` exchange** (§5.1) on the control plane: identity, name, OS, version,
   capabilities, role, and a nonce signed by the identity key (proves key
   ownership).
3. **Pairing / approval** (first contact only): the receiving engine shows an
   approval prompt with name, OS, address, and fingerprint. Optionally a
   **short authentication string** (SAS) derived from the handshake is shown on
   both ends for the user to compare. On approval the peer's `device_id` is added
   to the **trusted list**; future connections are automatic.
4. **Resume:** trusted peers skip the prompt; identity pinning still applies.

A rejected or unknown peer may discover the device but cannot exchange input,
state, clipboard, or files.

### 5.1 `Hello`
```
Hello {
  device_id:      bytes32,
  name:           string,
  os:             enum,
  engine_version: string,   // semver
  proto_version:  u16,
  capabilities:   set<Capability>,
  role:           enum { Eligible, Ineligible },
  nonce_sig:      bytes,     // signature over the QUIC transport params + nonce
}
```

---

## 6. Transport planes

Both planes share **one QUIC connection** per peer.

### 6.1 Control plane — reliable, ordered
QUIC bidirectional streams. Every message except pointer motion travels here.
Reliable and ordered; correctness over latency.

### 6.2 Input-motion plane — unreliable, unordered
QUIC **DATAGRAM** frames (RFC 9221). Carries **only** `PointerMotion` (§7.6).
Lossy and newest-wins by design (§8).

---

## 7. Message catalog

Every control-plane message is an envelope: `{ type: u16, flags: u16, payload }`,
where `payload` is `postcard`-encoded. Decoders ignore unknown `type` values.
Types are grouped below; field lists are the normative interface.

### 7.1 Session & liveness
```
Hello            (§5.1)
HelloAck         { accepted: bool, reason?: string }
Ping             { ts: u64 }
Pong             { ts: u64 }            // echoes Ping.ts for RTT
Heartbeat        { ts: u64, seq: u64 }  // periodic; absence ⇒ peer Disconnected
Goodbye          { reason: enum }       // graceful leave
```

### 7.2 Cluster state (CRDT replication)
```
StateDelta       { crdt_change: bytes }     // automerge/yrs change blob
StateRequest     { have_heads: [bytes] }    // sync: request missing changes
StateSnapshot    { full_state: bytes }      // for a freshly joined engine
```
State covers: device list, screen arrangement/layout, aliases, input
preferences, and per-device permissions. Every engine holds a full replica;
deltas gossip to all peers.

### 7.3 Layout & identification
```
LayoutUpdate     { arrangement: ... }        // also expressible as a StateDelta
IdentifyRequest  { device_id, number: u8 }   // show the big centered overlay
IdentifyClear    { device_id }
```

### 7.4 Ownership & focus
```
OwnershipTransfer { to: device_id, reason: enum { EdgeCross, WindowClick, Hotkey } }
FocusState        { owner: device_id, state: enum { Active, Standby, Disconnected } }
```

### 7.5 Input — reliable (control plane)
```
KeyEvent      { code: u32, down: bool, modifiers: u32, ts: u64 }
PointerButton { button: u8, down: bool, ts: u64 }
Scroll        { dx: i32, dy: i32, ts: u64 }
```
Keys, buttons, and scroll are **never** sent as datagrams — dropping a keystroke
or a click is unacceptable.

### 7.6 Pointer motion — lossy (datagram plane)
```
PointerMotion { seq: u32, ts: u64, x: f32, y: f32 }   // DATAGRAM only
```
- `x`,`y` are the **absolute pointer position normalized to the target screen**
  (0.0–1.0 per axis), resolved against the target's geometry in cluster state.
- `seq` is monotonic per active session; `ts` is the sender's monotonic clock.
- See §8 for receiver semantics.

### 7.7 Clipboard
```
ClipboardOffer { formats: [enum { Text, Image, Files }], hash: bytes, size: u64 }
ClipboardData  { format: enum, data: bytes }   // pulled after an accepted offer
```
Gated by the user's mode (off / text-only / full) and per-device clipboard
permission.

### 7.8 File transfer
```
FileOffer  { transfer_id, name: string, size: u64, count: u32 }
FileAccept { transfer_id }
FileChunk  { transfer_id, offset: u64, data: bytes }
FileAck    { transfer_id, received: u64 }
FileDone   { transfer_id, ok: bool }
```
Runs on its own reliable stream so a large transfer never blocks input.

### 7.9 Notifications
```
Notify { kind: enum { DeviceConnected, DeviceDisconnected, ConfigChanged,
                      CoordinatorChanged }, detail: string }
```

### 7.10 Coordinator election (lease-based)
```
CoordinatorLease { holder: device_id, term: u64, expires_at: u64 }
CoordinatorClaim { candidate: device_id, term: u64 }   // on lease expiry
```
Only `Eligible` engines participate. On lease expiry the eligible set picks the
next holder by a stable tiebreak (e.g. lowest `device_id`). The coordinator is
**not** a data dependency — state is in the CRDT.

---

## 8. The low-latency pointer-motion stream (detail)

This is the "super-fast stream of mouse coordinates, separate from the control
channel" requirement, specified.

**Sender (active device):**
- Sample the local pointer; **coalesce** to the send cadence (cap at the target's
  refresh / pointer poll rate). Never queue a backlog of stale positions.
- Emit one `PointerMotion` DATAGRAM per tick with the current **absolute,
  normalized** position, an incrementing `seq`, and a timestamp.

**Receiver (target device):**
- Apply a datagram **only if `seq` is newer** than the last applied; discard
  stale/out-of-order ones.
- Inject the absolute position via the platform `InputInjection` adapter.

**Why this shape:**
- **Newest-wins, drop the rest.** A retransmitted old position is useless; only
  the latest matters. Datagrams avoid head-of-line blocking and retransmit
  stalls that a reliable stream would impose exactly during a network hiccup.
- **Loss self-heals.** Because each datagram is an *absolute* position (not a
  delta), a dropped packet causes at most one frame of staleness; the next packet
  is already correct. Relative deltas would accumulate permanent drift on loss.
- **Actions stay reliable.** Buttons, scroll, and keys go on the control plane
  (§7.5), so no click or keystroke is ever lost even though motion is lossy.

**Why one QUIC connection, not raw UDP:** motion datagrams inherit the
connection's TLS encryption, path validation, congestion signal, NAT keep-alive,
and **connection migration** (a Wi-Fi roam or IP change does not require a new
handshake). A separate raw-UDP socket would re-implement all of that. Raw UDP is
a documented fallback only if datagram overhead is ever shown to matter on target
hardware.

**Why not MQTT for any of this:** an MQTT broker is a single point of failure and
an extra hop — disqualified by the peer-to-peer, no-SPOF, zero-config principles.
State fan-out uses direct peer links + gossip instead.

---

## 9. Permission enforcement

Capabilities negotiated in `Hello` and per-device permission toggles in cluster
state are enforced **on receipt**, in the core, before any platform adapter is
called. A peer without `keyboard` permission that sends `KeyEvent` is dropped (and
may be logged), regardless of what its UI showed. This keeps enforcement on the
trusted side of the boundary.

---

## 10. Summary of channels

| Data | Plane | Reliability |
|------|-------|-------------|
| Handshake, Hello, pairing | Control | Reliable |
| Cluster state / CRDT deltas | Control (gossip) | Reliable |
| Ownership / focus | Control | Reliable, ordered |
| Key / button / scroll | Control | Reliable, ordered |
| **Pointer motion** | **Datagram** | **Lossy, newest-wins** |
| Clipboard | Control | Reliable |
| File transfer | Control (own stream) | Reliable |
| Notifications, election, heartbeats | Control | Reliable |
