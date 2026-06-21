# Mouser — Communication Interface (Wire Protocol) — v2.2

This is the contract that lets independently-built Mouser binaries interoperate.
If two engines implement this document, they form a cluster.

> v2 incorporated Round 1 ([design-review.md](design-review.md)); **v2.1** closes the
> Round 2 gate findings (CBOR profile, enum discriminants, trust-store authority,
> identity hash domain, bulk-connection auth, capability-state, layout revision).

---

## 0. Conventions (normative)

- **Two connections per peer**:
  - **interactive connection** — control stream + pointer-motion datagram plane; its
    congestion window is never filled by bulk data.
  - **bulk connection** — file transfer, clipboard images, state snapshots; app-rate-limited.
  Both are QUIC + TLS 1.3, pinned to the same `device_id`, established together (§5, §6.2).
- **Control encoding = CBOR** (RFC 8949) via serde/`ciborium`. CBOR is self-describing →
  unknown map keys are skipped and added fields tolerated, which makes version skew safe.
- **Datagram encoding = postcard**, fixed layout, for `PointerMotion` only.

### 0.1 CBOR encoding profile (normative)
This profile is mandatory; `mouser-protocol` ships **golden vectors** as the conformance oracle.
- **Structs** encode as **definite-length CBOR maps keyed by the field-name string**
  (serde default). Decoders **MUST ignore unknown keys** and treat absent optional keys as
  `None`/default. Field names are the lowercase identifiers shown in §7.
- **Enums** are **C-like** (no payload) and encode as their **unsigned integer discriminant**
  (Appendix C) — **never** the variant-name string. Implementations MUST use a custom integer
  (de)serializer (serde `try_from = "u16"` / hand-written `Deserialize`) that maps an unrecognized
  discriminant to the enum's `Unknown` sentinel rather than erroring. Do **not** use `serde_repr` —
  its derive errors on unknown values, which would break the §2 forward-compat guarantee.
- **Sets** (e.g. `set<Capability>`) encode as a CBOR **array of integer discriminants, ascending,
  no duplicates**; an **unrecognized member is dropped** from the set (not kept as `Unknown`), so
  set-member enums need no `Unknown` sentinel.
- `bytes` → CBOR byte string; `bytes32` → 32-byte byte string. Integers are unsigned unless the
  field type says `i32`/`i64`. Multi-byte scalars in the postcard datagram are little-endian.
- **Golden vectors**: `mouser-protocol` produces canonical encoded vectors (including an
  unknown-discriminant case proving the map-to-`Unknown` rule) as its **first deliverable**; until
  then §0.1 + Appendix C are the binding contract. Worked example — `Ping{ id: 7 }` (type `0x05`)
  frames as `09 00 00 00 | 05 00 | 00 00 | A1 62 69 64 07`, where `len=9` (LE) covers type+flags+payload
  and the CBOR payload `A1 62 69 64 07` decodes as map{ "id": 7 }.

### 0.2 Control-stream framing
Every control message is `len: u32 (LE) | type: u16 (LE) | flags: u16 (LE) | payload[len-4]`,
where `len` counts `type + flags + payload`. On an unknown `type` a decoder consumes `len` bytes
and continues. `flags` is reserved: senders MUST set 0, receivers MUST ignore unknown bits.

### 0.3 Limits, time, discipline
- **Size caps (per plane)**: a **control-stream** message ≤ 256 KiB (`String` ≤ 4 KiB, any `bytes`
  field ≤ 64 KiB). **Bulk-stream** messages may be larger — `FileChunk.data` ≤ 1 MiB,
  `ClipboardData.data` ≤ 1 MiB per frame (chunk larger payloads like `FileChunk`), `StateSnapshot.full_state`
  ≤ 8 MiB. Each bulk transfer is framed per §0.2 on its own stream (§6.2). Reject oversize before allocating.
- **Time**: timestamps are `u64` ms; **no wire field uses another machine's wall clock for
  comparison**. Lease/liveness use local-monotonic durations (§7.10).
- **Decode** path is panic-free (`deny(unwrap_used, panic, indexing_slicing)`) and fuzzed in CI.

---

## 1. Design goals
Interoperable (byte-exact framing + Appendix C discriminants + CBOR forward-compat); peer-to-peer
(no broker); secure by default (identity-bound cert, mandatory channel-bound pairing, local-authority
permissions); low input latency (motion on a dedicated datagram plane).

---

## 2. Versioning & capability negotiation
- **Protocol version = QUIC ALPN** token (`mouser/1`, …). Each side advertises **all** supported
  tokens (highest first); TLS selects the common maximum. ALPN is the **sole** version source —
  there is **no** version field in `Hello`.
- **Capabilities** exchanged in the authenticated `Hello` as `set<Capability>` (Appendix C);
  behaviour = intersection. mDNS TXT capability hints are advisory/UI-only and untrusted.
- Forward compatibility: unknown CBOR keys ignored; unknown `type` skipped; unknown enum
  discriminant → `Unknown`; new message types additive.

---

## 3. Device identity
- Permanent **Ed25519 keypair** generated on first launch.
- **`device_id` = SHA-256(SubjectPublicKeyInfo)** — the SHA-256 of the DER `SubjectPublicKeyInfo`
  of the Ed25519 identity key, full **32 bytes**, used for all comparison/pinning. A truncated
  base32 rendering is **display-only** and never compared. (`device_id` is identical whether
  computed from the raw identity key's SPKI or from the TLS leaf cert's SPKI, because they are the
  same key — see below.)
- The **TLS leaf certificate's public key IS the Ed25519 identity key** (the v1 "or signed by it"
  option is removed). On **every** connection (pairing and resume) the receiver MUST verify
  `SHA-256(presented_cert_SPKI) == device_id` — via a custom `rustls`
  `ServerCertVerifier`/`ClientCertVerifier` (supported) — **before** processing any `Hello`.
- Trust is keyed solely on `device_id`; name/address are never part of the trust check.

---

## 4. Discovery
- **Service type:** `_mouser._udp.local` (mDNS/DNS-SD).
- **Instance name:** `"<display name> (<short id>)"` (unique even when names collide).
- **TXT** (typed; `txtvers=1`; unknown keys ignored): `id` (base32 no-pad lower of full device_id),
  `name`, `os`, `ver` (display only), `iport`, `bport`, `caps` (advisory csv), `role`.
  TXT is advisory; trust is established in §5.

---

## 5. Connection establishment & pairing
1. **QUIC handshakes** (interactive + bulk), ALPN + TLS 1.3; cert pinning per §3.
2. **`Hello`** on the interactive control stream (§7.1), carrying `channel_sig` (step 4).
3. **First contact (pairing):** the receiver shows the **cert-derived** `device_id`/fingerprint
   (not TXT data) and a **mandatory** 6-digit SAS computed identically on both ends. Let
   `ctx = min(idA,idB) || max(idA,idB)` (the two 32-byte `device_id`s ordered ascending by byte value),
   `k = tls_exporter(label="mouser-sas-v1", context = ctx, length = 32)` on the **interactive** connection,
   `digest = HKDF-SHA256(salt = "mouser-sas-v1", ikm = k, info = "sas", L = 4)`; then
   `SAS = ( be_u32(digest) mod 1_000_000 )` rendered as **6 zero-padded decimal digits**. The user
   compares the two screens and approves. SAS is not optional.
4. **Identity proof:** `channel_sig = Ed25519_sign( identity_key,
   tls_exporter(label="mouser-channel-v1", context = device_id_of_signer, length=64) )` on the
   **interactive** connection (TLS 1.3 exporter, RFC 8446 §7.5 / RFC 5705) — binds the proof to this
   session (defeats relay/replay).
5. **Bulk binding:** the bulk connection sends `BulkHello` (§7.1) carrying the same `device_id` and the
   interactive `session_id` it binds to, with `channel_sig = Ed25519_sign( identity_key,
   tls_exporter(label="mouser-bulk-v1", context = device_id_of_signer || be8(interactive_session_id),
   length = 64) )` over the **bulk** connection's exporter — so `interactive_session_id` is part of the
   signed bytes, binding the two connections. No separate SAS (trust already established).
6. **Resume:** trusted peers skip the prompt; cert pinning + `channel_sig` still verified.

`HelloAck.status ∈ {Accepted, Rejected, Pending}`; first contact returns `Pending` (timeout 120 s),
then a `PairingResult`. **Before trust is established a peer may complete the handshake, exchange
`Hello`, and run pairing (SAS) only** — it exchanges no input, state, clipboard, or files until it
is in the trusted list.

---

## 6. Transport planes
### 6.1 Interactive connection
- **Control stream** — one long-lived high-priority bidi QUIC stream, reliable+ordered, framed per
  §0.2. Carries `Hello`, ownership/focus, key/button/scroll, small CRDT deltas, election, clipboard
  offers/text, capability-state, notifications, liveness.
- **Motion datagram plane** — QUIC DATAGRAM (RFC 9221), `PointerMotion`/`PointerMotionRel` only (§7.6). If the path's
  `max_datagram_frame_size` is insufficient, motion degrades onto the control stream with coalescing.

### 6.2 Bulk connection
Separate QUIC connection authenticated by `BulkHello` (§5 step 5) and bound to the interactive `session_id`.
Each file transfer or large clipboard payload uses **one dedicated bidirectional stream per
`transfer_id`/`hash`**, framed per §0.2. App-level bandwidth cap (default 50 % of estimated path
capacity; hard-yields to interactive RTT spikes) so bulk never starves motion.

---

## 7. Message catalog
Envelope per §0.2; `type` shown as `[NN]`. Enum value sets are in Appendix C. Field names are the
CBOR map keys (§0.1).

### 7.1 Session & liveness
```
[01] Hello       { device_id: bytes32, name: str, os: Os, engine_version: str,
                   capabilities: set<Capability>, role: Role, session_id: u64, channel_sig: bytes }
[02] HelloAck    { status: AckStatus, reason?: str }
[03] PairingResult { accepted: bool, reason?: str }
[04] BulkHello   { device_id: bytes32, interactive_session_id: u64, channel_sig: bytes }
[05] Ping        { id: u64 }
[06] Pong        { id: u64 }                 // echoes Ping.id (same-clock RTT)
[07] Heartbeat   { seq: u64 }                // interval 1s; Disconnected after 3 misses
[08] Goodbye     { reason: GoodbyeReason }   // owner Goodbye triggers handoff first
```
`session_id` is a random per-connection identifier used only to bind the bulk connection to the
interactive one (§5 step 5); it is unrelated to input ownership (which is keyed on `owner_epoch`, §7.4).

### 7.2 Cluster state — CRDT replication
Pinned CRDT **automerge**, `fmt = 1`.
```
[10] StateDelta   { fmt: u16, change: bytes, dep_heads: [bytes32] }
[11] StateRequest { fmt: u16, have_heads: [bytes32] }
[12] StateChanges { fmt: u16, changes: [bytes] }
[13] StateSnapshot{ fmt: u16, full_state: bytes }   // bulk connection, own dedicated stream
```
- The replicated CRDT holds **shared, non-security config only**: `devices`, per-monitor `layout`
  (+ a monotonic `layout_rev`), `aliases`, `input_prefs` (Appendix A).
- **Permissions and the trusted list are NOT in the CRDT** — they are a
  **local, non-replicated policy store** authored only by the local user about peers (§9). This is a
  deliberate departure from the brief's "permissions replicate": enforcement authority must be local.
- `StateDelta` whose `dep_heads` are unmet is buffered until parents arrive. Anti-entropy: send
  `StateRequest` on connect and every 30 s; reply `StateChanges`; dedup by change-hash. Gossip is
  best-effort; `StateRequest` is the correctness backstop.

### 7.3 Layout & identification
```
[20] IdentifyRequest { device_id: bytes32, number: u8, ttl_ms: u32 }
[21] IdentifyClear   { device_id: bytes32 }
```
Layout changes travel only as `StateDelta` (v1 `LayoutUpdate` removed).

### 7.4 Ownership, focus & capability
```
[30] OwnershipTransfer { to: bytes32, owner_epoch: u64, layout_rev: u64, reason: TransferReason } // owner-minted grant
[31] OwnershipAck      { owner_epoch: u64, accepted: bool, reason?: str }
[32] FocusState        { owner: bytes32, owner_epoch: u64, state: FocusKind }
[33] CapabilityState   { device_id: bytes32, capture: CapState, inject: CapState, reason: BlockedReason }
[34] OwnershipRequest  { from: bytes32, reason: TransferReason }  // non-owner asks the current owner to hand off
[35] PointerModeReq    { owner_epoch: u64, mode: PointerMode }    // target → owner: select absolute/relative motion
```
- Ownership is a **single token with monotonic `owner_epoch`**. Who mints `epoch+1` depends on the case:
  during a **normal handoff** only the **current owner** mints (so `OwnershipTransfer` is an owner-minted
  *grant*) — a non-owner that wants ownership sends `OwnershipRequest` (mobile `UiSelect` included) and the
  owner mints + grants. When the **owner is unreachable** (heartbeat-timeout) or on a **local-input or
  panic reclaim**, the reclaiming device **self-mints** `epoch+1` directly (no reachable owner to conflict
  with). So a *reachable* owner is the sole minter; an unreachable one is replaced by self-mint plus the
  simultaneous-reclaim tiebreak below — two valid grants for one epoch cannot otherwise exist.
- **Acceptance rule:** accept an `OwnershipTransfer`/`FocusState` iff its `owner_epoch` is **strictly
  greater** than the locally-known epoch. The lone exception is *simultaneous reclaim* — two devices
  independently self-mint the same `epoch+1` after an owner heartbeat-timeout (below): there the claim
  with the **lower `device_id` wins** and the loser adopts the winner's state at that epoch.
- `LocalReclaim`: a device whose **own** local hardware is used reclaims ownership (replaces v1
  "WindowClick"). On owner heartbeat-timeout the physically-attached device reclaims with `epoch+1`.
- **`layout_rev`** is a monotonic counter stored in the CRDT (LWW by `(layout_rev, editor device_id)`),
  bumped on each layout change. A receiver
  whose local `layout_rev` < `OwnershipTransfer.layout_rev` pulls via `StateRequest` before applying.
- **`CapabilityState`** is broadcast when input capture/injection becomes (un)available — secure
  desktop, lock screen, missing permission, unsupported compositor. On a block the engine
  returns ownership to the source and sets `FocusKind.InputBlocked`.

### 7.5 Input — reliable (control stream)
```
[40] KeyEvent     { usage: u16, down: bool, mods: u16, owner_epoch: u64, ctr: u64 }
[41] PointerButton{ button: u8, down: bool, owner_epoch: u64, ctr: u64 }
[42] Scroll       { dx: i32, dy: i32, unit: ScrollUnit, owner_epoch: u64, ctr: u64 }
```
- `usage` = USB HID Usage Page 0x07 (Appendix B); physical-key semantics. `mods` bitmask Appendix B.
  `button`: 0=left,1=right,2=middle,3=back,4=forward.
- `Scroll.unit` (Appendix C): `Detent120` = `dx/dy` in 1/120-of-a-wheel-notch units (legacy wheel);
  `LogicalPixel` = high-resolution/trackpad pixels. Receiver converts to the platform's native unit.
- **Anti-replay**: `owner_epoch` = the current ownership epoch (§7.4) under which the event is sent;
  `ctr` = a monotonic counter reset whenever `owner_epoch` changes. Receivers reject events whose
  `owner_epoch` is not the current one, or whose `(owner_epoch, ctr)` is not strictly increasing.

### 7.6 Pointer motion — lossy datagram
```
PointerMotion    (datagram, postcard, 1-byte tag 0x01 = absolute):
  { owner_epoch: u64, seq: u32, display_id: u32, x: i32, y: i32 }
PointerMotionRel (datagram, postcard, 1-byte tag 0x02 = relative):  // pointer-locked / relative targets
  { owner_epoch: u64, seq: u32, dx_acc: i64, dy_acc: i64 }          // cumulative deltas since session start
```
- **Tag 0x01 (absolute, default):** `x,y` = integer logical pixels in the target **display's** space
  (`display_id`, Appendix A); origin top-left, y-down; receiver clamps. Loss self-heals (absolute).
- **Tag 0x02 (relative):** used only when the target reports a pointer-locked/relative consumer (games,
  3D). `dx_acc,dy_acc` are **cumulative** deltas since session start, so a dropped packet costs at most
  the motion between two surviving samples (still newest-wins, not per-packet deltas). **Mode selection is
  explicit on the wire:** the sender defaults to absolute and switches to `PointerMotionRel` only after
  receiving `PointerModeReq{mode: Relative}` from the target (sent when the target's foreground app grabs
  pointer-lock), switching back on `PointerModeReq{mode: Absolute}`. The 1-byte datagram tag carries the
  mode each packet so a receiver can always discriminate.
- Both: a change in `owner_epoch` resets `last_seq`; `seq` compared wraparound-safe (RFC 1982); no `ts`;
  unknown tag → drop; apply only if `(owner_epoch, seq)` is newer.
- Authorized only from a trusted peer with `mouse` permission that is the current owner; path-validated
  after migration.

### 7.7 Clipboard
```
[50] ClipboardOffer { entries: [{ format: ClipFormat, hash: bytes32, size: u64 }], origin: bytes32 }
[51] ClipboardPull  { hash: bytes32, format: ClipFormat }
[52] ClipboardData  { hash: bytes32, format: ClipFormat, offset: u64, data: bytes, last: bool }  // bulk; chunked
```
`hash = SHA-256(canonical(format, bytes))`, where `canonical` per `ClipFormat` is: `utf8_text` = UTF-8
with CRLF→LF and no trailing NUL; `html` / `rtf` = bytes as-is; `png` = the raw PNG byte stream;
`uri_list` = UTF-8, LF-separated, no trailing blank line. The puller verifies the *reassembled* data's hash
(drop on mismatch). **Transport by size:** a small payload (text formats within the control-stream cap)
rides the **interactive** control stream as a single `ClipboardData` (`offset = 0`, `last = true`); `png`
or any payload over the control cap rides the **bulk** connection on its own per-`hash` stream as ordered
chunks (`offset` = byte offset, `data` ≤ 1 MiB per chunk, `last = true` on the final chunk; the offer's
`size` bounds the total). A new Offer supersedes outstanding offers from
that origin; locally-applied clipboard is tagged `(origin,hash)` and not re-offered (loop prevention).
Gated by mode (off/text-only/full) + permission. `ClipFormat` values in Appendix C.

### 7.8 File transfer (bulk connection)
```
[60] FileOffer  { transfer_id: u64, files: [{ name: str, size: u64 }] }
[61] FileAccept { transfer_id: u64, resume: [{ file_index: u32, offset: u64 }] }
[62] FileReject { transfer_id: u64, reason: str }
[63] FileChunk  { transfer_id: u64, file_index: u32, offset: u64, data: bytes }
[64] FileAck    { transfer_id: u64, file_index: u32, acked_through: u64 }   // cumulative bytes; window 8 MiB
[65] FileDone   { transfer_id: u64, ok: bool }
```
`name` sanitized on receipt (strip separators, reject `..`, no symlink follow); files land in a
quarantine dir. One dedicated bulk stream per `transfer_id`.

### 7.9 Notifications
```
[70] Notify { kind: NotifyKind, detail: str }
```
Connect/disconnect debounced; `CoordinatorChanged` off by default.

### 7.10 Coordinator election — lease-based
```
[80] CoordinatorLease { holder: bytes32, term: u64, ttl_ms: u32 }   // duration, NOT absolute time
[81] CoordinatorClaim { candidate: bytes32, term: u64 }
[82] CoordinatorYield { from: bytes32, term: u64 }
```
- Local-monotonic only: on receipt set `deadline = monotonic_now + ttl_ms`; renew at `ttl/3`; default
  `ttl_ms = 6000`, capped 30000.
- Term rules: candidate increments `term` on `Claim`; a holder seeing a strictly-higher `term` yields;
  equal `term` → lowest `device_id` (sole deterministic tiebreak).
- The coordinator **serializes nothing in steady state** — state is the CRDT, admission is local
  approval, ownership is the epoch token. It is a cosmetic "who's in charge" label + optional
  unattended-admission fallback; all-ineligible clusters operate with no coordinator.

---

## 8. Pointer-motion stream (latency detail)
**Sender:** event-driven — emit a datagram per input event; coalesce-keep-newest only when the prior
datagram hasn't flushed; ~1000 Hz hard cap (not a cadence); ≤2 ms sender budget; on the interactive
connection only. **Receiver:** apply if `(owner_epoch, seq)` newer; inject absolute logical-pixel position
on `display_id`; clamp; optional ≤1-frame smoothing off by default. **Pointer-lock/relative consumers**
(games): on `PointerModeReq{mode: Relative}` from the target the sender switches to the `PointerMotionRel`
datagram (tag 0x02, cumulative deltas, §7.6); otherwise the absolute `PointerMotion` (tag 0x01) is used.
**Budget:** ≤5 ms wired / ≤15 ms good Wi-Fi, asserted by the harness.

---

## 9. Permission & authority
- **Authoritative permissions and the trusted list are a LOCAL, non-replicated store**, authored only
  by the local user about peers — never on the wire, never peer-writable (resolves the Round 2 trust
  contradiction). A read-only advisory mirror MAY be shown in UI but is never used for authorization.
- **Authorization** = `negotiated_capability ∧ local_permission`; capabilities are advisory, the local
  permission is authoritative. Enforced on receipt, in core, before any platform adapter, on **both**
  the control stream and the datagram plane. Unauthorized messages dropped (rate-limited logging).
- **Trusted-but-malicious mitigations**: per-peer input rate-limit + burst cap; "remote input
  only when unlocked" (default on); visible active-owner indicator + optional first-input confirm;
  peer-initiated ownership is a request, never an unconditional grant.

---

## 10. Abuse & DoS control
QUIC Retry/address validation (anti-amplification); per-source-IP connection + `Hello` rate limits;
cap concurrent untrusted connections and the discovered-device list; penalize repeated trust failures;
size caps (§0.3) + panic-free fuzzed decoders.

---

## 11. Channel summary
| Data | Connection / plane | Reliability |
|------|--------------------|-------------|
| Hello, pairing, election, liveness, capability-state | interactive / control | reliable, ordered |
| Ownership / focus | interactive / control | reliable, epoch-gated |
| Key / button / scroll | interactive / control | reliable, replay-checked |
| CRDT deltas (small) + anti-entropy | interactive / control | reliable + periodic sync |
| **Pointer motion** | **interactive / datagram** | **lossy, newest-wins** |
| Clipboard offers / text | interactive / control | reliable |
| Clipboard images, files, snapshots | bulk (stream per transfer) | reliable, rate-limited |

---

## Appendix A — CRDT document schema (normative)
Automerge root keys (**permissions and trusted list are intentionally absent** — local store, §9):
- `layout_rev: u64` — monotonic layout revision (LWW by `(layout_rev, editor device_id)`).
- `devices: Map<device_id_hex, { name: str, os: str }>`  (the user-chosen alias lives in `aliases`
  below — not duplicated here).
- `layout: Map<device_id_hex, { monitors: List<Monitor> }>`,
  `Monitor { display_id: u32, x: i32, y: i32, w: u32, h: u32, scale_milli: u32, rotation: u16 }`
  in a shared virtual-desktop coordinate space (signed origins). `scale_milli` = scale ×1000 (integer).
- `aliases: Map<device_id_hex, str>` — optional user-chosen display alias per device.
- `input_prefs: { edge_dwell_ms: u32, lock_on_drag: bool, cursor_accel: bool, cmd_ctrl_swap: bool,
  hotkeys: Map<str, str> }` — cluster-wide input preferences (`hotkeys` maps an action name, e.g.
  `"jump:<device_id_hex>"` or `"panic"`, to a platform-neutral chord string).

**Post-merge normalization**: after applying changes each engine deterministically re-derives
the edge-adjacency map from merged `Monitor` rects (overlaps resolved by ascending `device_id`, then
`display_id`) — identical inputs ⇒ identical edge map everywhere.

## Appendix B — Canonical keymap (normative)
Wire keycodes = **USB HID Usage IDs, Usage Page 0x07** (e.g. `0x04`=a, `0x28`=Enter, `0x29`=Esc).
Each platform adapter ships a bidirectional table: macOS HID↔CGKeyCode; Windows HID↔scancode/VK; Linux
HID↔evdev `KEY_*`. `mods` bitmask: bit0 LCtrl,1 LShift,2 LAlt,3 LMeta,4 RCtrl,5 RShift,6 RAlt,7 RMeta.
`Meta` = Cmd/Win/Super, remapped per target OS (optional Cmd↔Ctrl swap preference).

## Appendix C — Wire enums (normative)
All encode as the unsigned integer below via a custom integer (de)serializer (**not** `serde_repr`);
decoders map an unknown value to `Unknown`. For `set<>` members an unknown value is dropped (§0.1).
- `Os`: macos=0, windows=1, linux=2, ios=3, android=4, Unknown=255
- `Capability`: keyboard=0, mouse=1, clipboard=2, file_transfer=3, webcam=4, audio=5,
  coordinator_eligible=6, remote_control_only=7 — set member; unknown discriminants dropped (§0.1), no `Unknown` sentinel
- `Role`: eligible=0, ineligible=1, Unknown=255
- `AckStatus`: accepted=0, rejected=1, pending=2, Unknown=255
- `GoodbyeReason`: shutdown=0, sleep=1, user_quit=2, network_leave=3, Unknown=255
- `TransferReason`: edge_cross=0, hotkey=1, ui_select=2, local_reclaim=3, Unknown=255
- `FocusKind`: active=0, standby=1, disconnected=2, input_blocked=3, Unknown=255
- `CapState`: available=0, permission_missing=1, secure_context=2, unsupported=3, Unknown=255
- `BlockedReason`: none=0, secure_desktop=1, lock_screen=2, secure_input_field=3, permission=4,
  compositor_unsupported=5, Unknown=255
- `ClipFormat`: utf8_text=0, html=1, png=2, uri_list=3, rtf=4, Unknown=255
- `ScrollUnit`: detent120=0, logical_pixel=1, Unknown=255
- `PointerMode`: absolute=0, relative=1, Unknown=255
- `NotifyKind`: device_connected=0, device_disconnected=1, config_changed=2, coordinator_changed=3, Unknown=255
