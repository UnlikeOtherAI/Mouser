# Mouser — Design Review (Round 1)

A 10-agent adversarial review of the design docs (`brief.md`, `architecture.md`,
`tech-stack.md`, `communication-interface.md`). No implementation exists yet —
this reviews the design before code.

**Panel:** 5 Claude agents (architecture/distributed-systems, protocol interop,
performance/latency, security, usability) + 5 Codex agents (tech-stack/deps,
cross-platform input feasibility, RFC/factual-accuracy audit, implementability,
perf cross-check + A/V feasibility).

**Verification:** Every finding accepted below was cross-checked against the
actual doc text, and factual/external claims were independently verified
(RFC 9221/9000/9002, rustls/quinn docs, and `cargo search` against crates.io for
disputed crates). Convergence count = number of independent reviewers who raised
it. Severities are re-graded here, not inherited.

Legend: `[Cn]` Claude reviewer, `[Xn]` Codex reviewer. `★` = factual claim
independently verified.

---

## A. Blocking decisions (make these before writing code)

These are design forks the docs currently leave open or get wrong. Each maps to
a CRITICAL finding below.

1. **Wire encoding for the control plane.** postcard cannot deliver the stated
   forward-compat rule. → adopt a tagged/length-delimited codec (protobuf or
   CBOR) for control; keep postcard only for the fixed `PointerMotion` datagram. (F1, F2)
2. **One CRDT library, pinned + versioned, with a defined document schema and
   sync algorithm.** "automerge *or* yrs" cannot be a wire contract. (F3)
3. **Canonical cross-OS keycode/modifier/button namespace** (USB HID usage IDs). (F4)
4. **Two QUIC connections per peer** (interactive vs bulk) — not one. (F5)
5. **Ownership as an explicit single-token lease with an epoch** (not fire-and-forget). (F6)
6. **Coordinator lease on local-monotonic TTL**, not cross-machine wall-clock. (F7)
7. **Bind the TLS cert to `device_id`** and verify it every connection. (F8)
8. **Mandatory, fully-specified pairing** (channel-bound SAS over the TLS exporter). (F9)
9. **Per-monitor geometry model** with `display_id` in handoffs — not one
   normalized rectangle per device. (F10)
10. **Process model:** a headless `mouser-engine` daemon + a Tauri UI *client*
    over IPC. Tauri is not the daemon. (F11)
11. **Honest platform-capability matrix.** "Zero configuration" and uniform
    "Linux/Wayland" support are not deliverable as written. (F12, F13)

---

## B. CRITICAL

### F1. postcard cannot deliver the forward-compatibility rule — interop breaks on any version skew
Converged ×4 [C-protocol, X1, X3, X4] ★
`communication-interface.md` §2 (lines 38-39) mandates "decoders MUST ignore
unknown message-type tags and unknown trailing fields," but §7 (line 127) encodes
payloads with `postcard`, a **non-self-describing** format: no field tags, no
length per field, and enums encode by declaration-order index (an unknown enum
variant *errors*, it cannot be skipped). The headline interop promise is
contradicted by the chosen codec. Verified against postcard docs (X3 web-checked)
and confirmed by inspection.
**Fix:** Use a tagged/evolvable codec for the control plane — **protobuf (prost)**
(recommended) or CBOR. Assign **explicit numeric discriminants** to every wire
enum. Keep postcard *only* for the frozen-size `PointerMotion` datagram.

### F2. Message envelope has no length prefix; stream framing is undefined → two implementations can't parse each other
Converged ×2 [C-protocol, X4]
`communication-interface.md` §7 (line 126): envelope is `{type:u16, flags:u16,
payload}` with no length, so an unknown `type` can't be skipped ("ignore unknown
type" is unimplementable), and the doc never says whether messages are one-per-
stream or framed on a long-lived stream.
**Fix:** Define framing normatively: `len:u32 | type:u16 | flags:u16 | payload[len-4]`
on a long-lived control stream; on unknown `type`, consume `len` and continue.
Define the stream inventory (control, per-transfer, CRDT) and lifecycle.

### F3. CRDT library is unpinned and its schema/sync is undefined — "automerge or yrs" can't be a wire contract
Converged ×3 [C-protocol H3, X1, C-arch L3]
`communication-interface.md` §7.2 carries opaque `crdt_change`/`full_state` blobs;
`tech-stack.md` §3 says "automerge (or yrs)". Automerge and yrs have incompatible
change formats and sync algorithms (`have_heads` = automerge change-hashes vs yrs
state-vector). The CRDT document schema (key paths, how a screen rect is
represented) — the real state contract — is absent.
**Fix:** Pin **one** CRDT lib + a `crdt_format_version`; reject on mismatch.
Specify the document schema as a normative appendix, the late-join flow, and a
periodic `StateRequest`-based anti-entropy backstop with change-hash dedup (see F14).

### F4. Cross-OS keycode/modifier/button namespace is undefined — shared keyboard types wrong keys across OSes
Converged ×2 [C-protocol C5, C-arch L5]
`communication-interface.md` §7.5: `KeyEvent.code: u32`, `modifiers: u32`,
`PointerButton.button: u8` with no namespace. macOS virtual keycodes, Windows
VK/scancodes, and Linux evdev codes are mutually incompatible; this is the core
feature.
**Fix:** Mandate a canonical wire keymap — **USB HID usage IDs** (or W3C UI Events
`code`) — with per-platform map tables; define the modifier bitmask (incl.
left/right and Cmd↔Win↔Super policy) and button numbering. Decide physical-key vs
logical-char semantics (recommend physical).

### F5. Both planes on one QUIC connection — bulk transfer throttles/drops pointer motion; "never blocks input" is false
Converged ×2 [C-perf C1 (RFC 9221 §5, RFC 9002, quinn #2156), X5 (RFC 9000/9221)] ★
`communication-interface.md` §7.8 (line 197) claims a file transfer on its own
reliable stream "never blocks input." QUIC streams avoid *head-of-line* blocking
but **share one congestion controller + pacer + UDP socket**; DATAGRAM frames are
congestion-controlled (RFC 9221 §5). A large transfer fills the cwnd and motion
datagrams get paced behind it or dropped (quinn #2156 shows ~20% datagram loss
under load). Verified against the RFCs and quinn's tracker.
**Fix:** **Two QUIC connections per peer** — interactive (control + motion) and
bulk (files/clipboard images/snapshots). App-level bandwidth cap on bulk; drop
stale motion before enqueue. Delete the "never blocks input" claim.

### F6. Input ownership is unmodeled distributed state — it can be held by zero or two devices at once
Converged ×2 [C-arch C1, X4]
The product's #1 invariant ("only one machine owns input") is two fire-and-forget
messages (`OwnershipTransfer`, `FocusState`, §7.4) with no epoch, token, ack, or
tie-break, and ownership is **not** in the CRDT state list (`architecture.md`
§4.4). Concurrent edge-cross + window-click → two owners; target crash mid-handoff
→ zero owners; a delayed `FocusState` overwrites a newer one.
**Fix:** Model ownership as a single token with `owner_epoch: u64`. Only the
current owner mints epoch N+1; receivers accept only strictly-higher epochs,
tie-broken by `device_id`. Add transfer ack/reject and an explicit
"physically-attached device reclaims on owner heartbeat-timeout" rule.

### F7. Coordinator lease `expires_at` is a cross-machine wall-clock → split-brain (dual or zero coordinators)
Converged ×3 [C-arch C3, X4, X5]
`communication-interface.md` §7.10: `CoordinatorLease { expires_at: u64 }`
evaluated against each peer's own clock. LAN clocks drift seconds-to-minutes (no
NTP guarantee); a fast clock elects a second coordinator while the real one is
alive; a slow clock honors a dead one; a peer can pin itself forever.
**Fix:** Put a **duration** on the wire: `CoordinatorLease { holder, term, ttl_ms }`;
each receiver computes `deadline = local_monotonic_now + ttl_ms`, reset on renewal
(renew at ~ttl/3). Never compare timestamps minted on different machines. Bound
max `ttl_ms`. (Same disease in every `ts` field — see F15.)

### F8. The TLS certificate is never bound to `device_id` — pinning is defeated; first-contact MITM is trivial
Converged ×2 [C-security C1, X1/X3 pairing] ★ (rustls custom verifier confirmed feasible)
`communication-interface.md` §3 (lines 48-50): cert "whose key is this identity
key (**or is signed by it**)" — and never states the receiver recomputes
`SHA-256(cert_pubkey)` and checks it equals `device_id`. With self-signed TLS,
that check is the *only* thing making the connection trustworthy. An on-path
attacker (ARP/mDNS spoof) terminates TLS with any cert.
**Fix:** Mandate **cert public key == Ed25519 identity key**; on every connection
(pair *and* resume) verify `base32(SHA-256(cert SPKI)) == device_id` before
processing `Hello`. Drop the "or signed by it" option (or fully specify a chain to
the identity key).

### F9. Pairing is unspecified and optional — `nonce_sig` isn't channel-bound and SAS has no derivation → MITM/relay
Converged ×2 [C-security C2+C3, X1] 
`communication-interface.md` §5.1: `nonce_sig` is "over the QUIC transport params
+ nonce" — a self-chosen nonce, not bound to the live TLS session, so a relayed
signed `Hello` verifies. §5 step 3: SAS is "*optionally*… derived from the
handshake" with no algorithm/encoding/length. `tech-stack.md` §2 hedges "SPAKE2 or
Noise" (different protocols; `spake2` crate is `0.5.0-pre.0` ★). The actual
pairing exchange is absent from the wire spec.
**Fix:** Pick one flow. Recommended (simplest): self-signed TLS + identity-pin
(F8) + **mandatory** SAS = `HKDF(TLS-exporter || sorted(idA,idB))` rendered as
6 digits, compared on both screens. Replace `nonce_sig` with a signature over the
**TLS 1.3 exporter** (RFC 5705). Add any pairing messages to §7.

### F10. Geometry model is one normalized rectangle per device — multi-monitor/DPI/scale/rotation make the cursor land wrong
Converged ×4 [C-perf H2/M1, C-usability H3, X2 HIGH, X5 HIGH] ★
`communication-interface.md` §7.6: `x,y` "normalized to the target screen"
discards monitor identity, per-display DPI/scale, rotation, virtual-desktop origin
(Windows allows negative coords; macOS y-up; Wayland logical/discontinuous zones)
and which edge segment maps to which neighbor. The layout canvas is per-*device*,
not per-*monitor*.
**Fix:** Model layout **per monitor**. Handoff carries source display+edge+coord,
target `display_id`, coordinate space, scale, rotation. Define origin (top-left),
clamping, logical-vs-physical pixels. Prefer **integer logical pixels** (or
fixed-point) over `f32` for deterministic decoding (f32 precision itself is fine —
the contract is the problem).

### F11. Tauri is positioned as the daemon owner — it isn't one; the engine must be a separate process
Converged ×2 [X1 HIGH, X4 CRITICAL]
`architecture.md` §3 correctly splits engine/UI, but `tech-stack.md` §5/§8 say the
Tauri shell "links `mouser-core` directly" and owns autostart/"service
registration." Tauri is window/tray-centric; a fault-tolerant input daemon must
outlive the UI, and a Windows Session-0 service cannot touch the user input desk.
**Fix:** Three artifacts: **`mouser-engine`** (per-user headless daemon: net +
input + state), an optional privileged helper, and **`mouser-ui`** (Tauri tray/
settings client over IPC). Autostart launches the engine; the UI attaches.

### F12. "Zero configuration" is false — macOS TCC, Windows UAC/secure-desktop, and Wayland all need explicit grants
Converged ×3 [C-usability C1/C2, X2 HIGH×3, X1] ★
`brief.md` "Zero Configuration" promises install→launch→done, but macOS needs
Accessibility **and** Input Monitoring (+ restart), and Secure Event Input can
suppress capture even after; Windows UAC secure desktop / lock screen block
`SendInput` and aren't injectable from a normal process (UIPI); Wayland injection
is compositor/portal-gated. None of this is acknowledged.
**Fix:** Reframe as "**zero *network* configuration**; one-time OS permission grant
per machine." Add a first-run permission onboarding flow (deep-link to the exact
settings pane, live granted/not-yet checklist, auto-detect). Broadcast
`Locked/SecureInput/SecureDesktop/InputBlocked` status and **return ownership to
the source** in those states.

### F13. "Linux" / "Wayland" support is overclaimed and names a nonexistent crate
Converged ×3 [X1 HIGH, X2 CRITICAL, X4 CRITICAL] ★ (crates.io verified)
`tech-stack.md` §4 lists `libei` + `input` + `uinput`. Verified: **`libei` is not
a crates.io crate** (binding is **`reis = 0.7.0`**); **`input = 0.10.0`** is
libinput bindings (capture, not emulation); **`uinput = 0.1.3`** is stale (2018)
and needs privileged `/dev/uinput`. Wayland injection is compositor-mediated
(InputCapture is barrier-triggered, may be filtered/paused on lock); wlroots
portals still document only screenshot/screencast.
**Fix:** Replace with a backend capability matrix: **X11/XTEST** (`x11rb` + xtest)
= supported; **Wayland** via `reis` (libei) + `ashpd`/xdg `RemoteDesktop` =
supported *only on verified compositors*; **`input-linux`/`uinput` ioctl** =
optional privileged fallback (udev/polkit). UX states "Wayland support depends on
your desktop environment." Remove `libei`/`uinput`/`bincode` crate names; add
`reis`/`ashpd`/`input-linux`. (`bincode = 3.0.0` flagged by X1 as unmaintained —
regardless, standardize on postcard; drop bincode.)

---

## C. HIGH

### F14. CRDT gossip has no triggerable anti-entropy or causal-ordering rule → state diverges under churn
[C-arch H1, C-protocol H3] §7.2. Deltas "gossip to all" with no dedup, no seen-set,
no causal-dependency buffering, and `StateRequest` has no defined trigger. **Fix:**
periodic + on-reconnect `StateRequest` anti-entropy; dedup by change-hash; buffer
out-of-causal-order deltas until parents arrive.

### F15. `ts` units/epoch/clock-domain undefined and misused for cross-machine reasoning
[C-perf C2, C-protocol H6, X5] §7.1/7.5/7.6. `ts` is a per-process monotonic clock
(arbitrary epoch) yet implied useful to the receiver; it's 8 dead bytes on every
datagram. **Fix:** define canonical unit (ms) + per-field clock domain; mark input
`ts` opaque/sender-local; **drop `ts` from `PointerMotion`** (use `seq`).

### F16. `PointerMotion.seq: u32` — session reset & wrap undefined → cursor freezes after handoff or wrap
[C-protocol H7, C-arch H3, X5] §7.6/§8. Receiver "apply if seq newer" stalls when a
new session restarts `seq` low (stored last_seq high) or on u32 wrap. **Fix:** add
`session_id`/epoch (reset `last_seq` on change); wraparound-safe (RFC 1982)
comparison, or `u64`.

### F17. Ownership/motion not ordered against layout CRDT deltas → transfers use stale geometry
[C-arch H2] §7.4 vs §7.2/7.6. Layout replicates eventually-consistently; ownership/
motion assume current layout. **Fix:** stamp `OwnershipTransfer`/`PointerMotion`
with a `layout_version`; receiver queues/pulls if behind. Address edge-cross by
neighbor `device_id` from the sender's layout, not raw geometry.

### F18. 2D screen arrangement isn't a safe CRDT merge — concurrent drags converge to a broken adjacency graph
[C-arch H7] `architecture.md` §4.4. CRDT converges bytes, not the semantic invariant
(non-overlapping, connected). **Fix:** deterministic **post-merge normalization**
(snap/resolve overlaps by device_id) re-derived identically on every engine.

### F19. Election lacks term semantics, quorum, and partition-heal → dual coordinators after a split
[C-arch C4] §7.10. `term` exists but increment/compare/step-down rules don't;
quorumless lease → both partitions elect. **Fix:** Raft-style term rules (candidate
increments; higher term wins; equal→device_id), defined partition-heal (higher
(term,id) wins, loser re-syncs). Decide whether coordinator serializes anything
critical (F20).

### F20. Coordinator's job is undefined and contradicts the CRDT
[C-arch C2, X1] §4.5. "Conflict tie-breaking" is what a CRDT already does;
"admission" is local approval. **Fix:** decide (A) coordinator serializes ownership
(then it's load-bearing — define the gap behavior) or (B) it serializes nothing —
then largely delete the election machinery. Lean B.

### F21. `WindowClick` ownership transfer is physically impossible with one shared cursor
[X2 CRITICAL, C-usability H1, X4] `brief.md` "Window Interaction"; §7.4. You can't
click a remote window before the cursor is there. **Fix:** redefine as
`LocalInputReclaim` (user touches that machine's own kbd/mouse → it grabs
ownership); keep cross-machine transfer to edge-cross/hotkey/UI selection.

### F22. Send cadence "coalesce to refresh rate" adds up to a full frame of latency
[C-perf H3, X5] §8. ~16.7 ms quantization at 60 Hz contradicts "near-zero." **Fix:**
event-driven send; coalesce only under send-buffer pressure (keep newest); cap rate
~1000 Hz to protect the net, not as default cadence. State a budget (≤~2 ms
sender-side; target ≤5–15 ms end-to-end LAN).

### F23. Absolute-only coordinates break relative/pointer-lock consumers (games, 3D, RTS)
[C-perf H1] §7.6. Pointer-locked apps consume deltas and ignore absolute warps.
**Fix:** dual-mode motion (absolute default; cumulative-delta mode when the target
is pointer-locked or "gaming passthrough"), or explicitly scope pointer-lock out
of v1.

### F24. Windows UAC/secure-desktop + UIPI block injection; macOS Secure Event Input suppresses capture
[X2 HIGH×2, X1, C-usability C2] ★ (MS/Apple docs). **Fix:** detect and surface
"blocked by elevated/secure window"; auto-return ownership; optional signed
`uiAccess`/elevated helper as a documented, opt-in mode — don't promise control of
elevated apps by default.

### F25. No anti-replay on key/button/scroll; datagram authz boundary unstated
[C-security H2, H3] §7.5/§8/§9. QUIC AEAD protects in-session, but app-level replay
(via relay/reconnect) and "process motion only from trusted+permitted+owner peer"
aren't specified; §9 covers control only. **Fix:** per-session monotonic counter +
session-id; reject non-increasing; gate `PointerMotion` on trust+`mouse`
permission+current owner; require QUIC path validation on migration.

### F26. A trusted-but-malicious/compromised peer is an unthrottled keystroke-injection device
[C-security H4] §7.5, §10. Once granted `keyboard`, a peer can type anything at full
rate with no brakes — the in-scope "approved device turns malicious" threat is
unmitigated. **Fix:** per-peer input rate-limit/flood cap; "remote input only when
unlocked" toggle (default on); visible indicator + optional confirm on first remote
ownership; peer-initiated `OwnershipTransfer` is a *request*, not an unconditional grab.

### F27. Untrusted decode (postcard + CRDT) is a pre-trust DoS surface (OOM/panic)
[C-security H5] §7. Length-prefixed `bytes`/`String` fields (incl. `Hello`, parsed
before trust) allocate attacker-controlled sizes; automerge decode is a second large
parser. **Fix:** hard per-message/per-field size caps rejected before allocation;
no-panic decode discipline (`deny(unwrap/panic/indexing_slicing)` on the decode
path — matches strict-clippy policy); `cargo-fuzz` the decoders in CI.

### F28. No connection-admission / DoS control (mDNS spam, QUIC half-open floods)
[C-security H6] §4/§5. **Fix:** enable QUIC Retry/address validation
(anti-amplification); per-source-IP connection + `Hello` rate limits; cap the
discovered-device list; penalize repeated trust failures.

### F29. Pairing is per-pair → N² approvals across a cluster (contradicts zero-config)
[C-usability C4] §5, `architecture.md` §4.5. 4 devices ⇒ up to 6 mutual approvals;
also contradicts the coordinator-admits model. **Fix:** cluster-level trust —
approve a device into the cluster once; replicate the trusted entry (with the
security caveat in F31). Keep per-pair identity pinning. State the approval count
(target: 1 per new device).

### F30. Cursor-recovery / panic path is missing (target asleep/crashed/secure-desktop)
[C-usability C3] `architecture.md` §5. Only direct-peer engine-death is handled;
sleep/hang/secure-desktop leave the user with no input during the heartbeat window.
**Fix:** global **panic hotkey** that unconditionally reclaims local ownership;
handoff timeout (snap back if target doesn't ack within N ms); don't transfer to a
sleeping display.

### F31. Permission/trust authority is unspecified — if it lives in the peer-writable CRDT, a peer can escalate itself
[C-security M2/M4] §4.4/§9. Per-device permissions in the CRDT could be written by
the peer they govern. **Fix:** permission/trust state is **local policy authored only
by the device about its peers**, never peer-writable; capabilities are advisory,
authorization is always the local grant. Add revoke + legitimate-key-change re-pair flow.

### F32. ALPN vs `Hello.proto_version` is a downgrade vector; ALPN offer-set semantics unstated
[C-protocol H1, M9] §2/§5.1. Two version fields that can disagree; "offer the highest"
(singular) breaks ALPN intersection. **Fix:** ALPN is the single source of truth
(remove/validate `proto_version`); advertise *all* supported ALPN tokens.

### F33. Testing harness is named but not designed; CI signing/notarization is a separate workstream
[X4 HIGH×2] `tech-stack.md` §8/§9. **Fix:** build **`mouser-testkit`** first (fake
clock, fake transport with drop/reorder/latency, fake platform adapters, 3 in-process
engines, CRDT-convergence + ownership-handoff + reconnect assertions, latency
histograms). Split CI: PR checks / nightly unsigned packaging / protected signed
release (Apple notarization, Windows signing for SmartScreen).

---

## D. MEDIUM (condensed)

- **F34 Clipboard** [C-protocol H4]: pull message missing; `hash` algorithm/per-format
  binding undefined; no supersede rule; **no echo-loop prevention**. Add
  `ClipboardPull{hash,format}`, per-format `{format,hash,size}`, SHA-256 over
  canonicalized bytes, source-tagging to suppress re-offer.
- **F35 File transfer** [C-protocol H5]: multi-file unimplementable (`count` but no
  `file_index`); backpressure/`FileAck` semantics undefined; no reject/resume; no
  path-sanitization. Add `file_index`, cumulative-byte ack + window, `FileReject`,
  quarantine dir + reject `..`/symlinks.
- **F36 mDNS** [C-protocol H5/M1, C-arch H5]: instance-name collisions (two
  "MacBook Pro"); trust phrased around spoofable "name/address"; TXT values untyped;
  no `txtvers`. Key trust on **`device_id` only**; unique instance labels; type each
  TXT value; add `txtvers=1`.
- **F37 `device_id` width** [C-protocol M2, C-arch H4]: 16-byte "display" vs `bytes32`
  in `Hello`. State full 32-byte SHA-256 is canonical/pinned; 16 is display-only.
- **F38 LayoutUpdate** [C-protocol L3, X4]: duplicates CRDT path. Remove; route layout
  via `StateDelta` only.
- **F39 Liveness in CRDT** [X1]: device list/heartbeat/disconnected don't belong in
  long-lived CRDT history. Keep persistent config in CRDT; presence as ephemeral gossip;
  add snapshot/compaction.
- **F40 Numeric defaults** [C-arch M1, C-protocol M7]: heartbeat interval/timeout, lease
  ttl/renew unspecified → mismatched liveness. Specify defaults (e.g. heartbeat 1 s,
  dead after 3; lease ttl 6 s, renew 2 s).
- **F41 Datagram availability** [C-arch M2]: handle peers/paths without datagram support
  (read `max_datagram_frame_size`); fallback path. (Payload size itself is fine.)
- **F42 Scroll on reliable plane** [C-perf L2, X5]: hi-res/inertial scroll backlogs.
  Coalesce cumulative scroll; consider datagram for continuous scroll, reliable for notches.
- **F43 Capability vs permission** [C-security M4]: state caps are advisory; authorization
  is the local permission (see F31).
- **F44 Engine↔UI IPC** [C-security L1]: access-control the UDS/named pipe (0600 /
  SO_PEERCRED / pipe ACL).
- **F45 Webcam/audio = separate high-risk subsystem** [X5 HIGH, C-security M3]: macOS
  Camera Extension signing/notarization, Windows MF camera + driver-signed virtual audio,
  Linux v4l2loopback/PipeWire (Secure Boot/DKMS); live-camera bandwidth contends with input.
  Own architecture doc; separate transport/congestion; not a "future toggle."
- **F46 Accessibility absent** [C-usability M6]: layout canvas is mouse-only; color-only
  state; no screen-reader/keyboard-nav; non-native controls break a11y. Add a11y to quality gates.
- **F47 Identical-UI tradeoffs** [C-usability H5]: justify vs native-feel/a11y cost or relax to
  "consistent IA, native-feeling controls"; require WCAG 2.2 AA on custom controls.
- **F48 Deps to pin** [X1]: `rustls` provider (`rustls-ring` vs `aws-lc-rs`) + stable
  versions; `ed25519-dalek 2.2.x` (not RC); `uniffi` pre-1.0 → narrow `mouser-ffi` facade.
- **F49 QUIC migration overclaimed** [X1, X5]: frame migration as NAT-rebind help, not
  guaranteed roam survival; identity-pinned mDNS rediscovery is the required recovery path.

---

## E. LOW

- **F50** base32 alphabet for `device_id` unspecified (RFC 4648 no-pad, fixed case). [C-protocol L5]
- **F51** DNS-SD service type should be vendor-scoped/registered. [C-arch L1, X3]
- **F52** `Pong` matches `Ping` by `ts` only — add a request id. [C-protocol L4]
- **F53** Identify overlay needs a TTL + fullscreen-game behavior; show name+color not just a number. [C-usability M1]
- **F54** Rate-limit security logging (`§9` "may be logged" = log-flood). [C-security L4]
- **F55** Multi-user "Install for All Users": identity is per-**machine**, define user-switch. [C-usability L4]
- **F56** Drop "Server" from user-facing terms (reintroduces the server mental model). [C-usability L1]
- **F57** Tiebreak: `lowest device_id` **only** (drop "highest uptime, etc." — non-deterministic/grindable). [C-arch M7, C-security M5]
- **F58** `flags: u16` envelope field undefined → "reserved, MUST be 0, ignore on read." [C-protocol M3]

---

## F. What the panel agreed is RIGHT (keep)

- **Rust core** for a privileged, network-facing input daemon — unanimous. [X1]
- **QUIC/quinn two-plane** concept (reliable control + lossy datagram motion) — correct fit;
  datagrams = RFC 9221, supported by quinn. ★ The fixes are *connection split* (F5) and framing,
  not the concept.
- **Absolute, newest-wins motion** with self-healing loss — sound primary representation
  (needs the coordinate-space contract F10 + relative mode F23).
- **Identity-pinned self-signed TLS** — feasible (rustls custom verifier ✓); needs the
  binding (F8) and pairing (F9) made explicit.
- **CRDT for config + lease election** — appropriate *if* scoped to bounded config and the
  ownership/clock/term gaps (F6/F7/F19) are fixed.
- **Permission enforcement on the receiving (trusted) side** — correctly located (§9);
  extend to the datagram plane and fix authority (F25/F31).
- **Engine/UI process split** (architecture.md §3) — right; just make tech-stack match (F11).

---

## G. Recommended de-risking build sequence (from X4, endorsed)

1. `mouser-protocol` — message types, ALPN, codec rules + **golden vectors**, versioning.
2. `mouser-testkit` — fake clock/transport (drop/reorder/latency)/discovery/input adapters; 3-engine scenario runner.
3. Pure `mouser-core` — identity, trust list, permission enforcement, ownership epochs, CRDT layout schema, lease election.
4. Prove core headlessly — 3 virtual engines: ownership transfer, coordinator loss, CRDT convergence, loss/reorder, stale-message rejection.
5. Platform input **spikes** before the app — Linux (X11 then Wayland/portal), macOS (TCC), Windows (hooks/UIPI); each reports capability state.
6. Minimal **CLI two-machine bridge** — manual peer, hard-coded layout, QUIC control + motion datagrams; no mDNS/Tauri/files.
7. Add mDNS discovery + pairing.
8. Add local IPC + Tauri UI client (pairing prompts, device list, layout canvas, identify).
9. Add clipboard text; defer images/files/drag-drop.
10. Packaging in layers: unsigned dev → nightly smoke → signed/notarized release.

Highest-risk areas to prototype first: **Linux Wayland parity, macOS permission/notarization,
Windows input/UIPI, and making OS-level injection testable.**
