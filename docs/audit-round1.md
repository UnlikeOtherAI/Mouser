# Mouser — Code Audit (Round 1)

A 24-agent audit of the implemented codebase: **12 topics × (1 Opus + 1 Codex)**, each pair independent.
Every finding below is **convergent (both sources) or independently verified against the code**; severities
re-graded here. `×N` = independent reviewers who raised it.

Audited state: `main` = `mouser-protocol`, `mouser-core`, `mouser-net`, `platform-mac`, `platform-linux`
(+ `mouser-ffi`/`mouser-testkit` stubs); `apps/ios`, `apps/desktop` (Tauri), `platform-win` on branches.
Baseline: workspace builds, `clippy -D warnings` clean, 65 tests pass.

**Headline:** the *primitives* are well-built (pure ownership/election state machines with strong tests,
correct framing/CBOR/datagram codecs, sound cert-pinning + mutual TLS, panic-free decoders). The risk is
almost entirely at the **integration boundary that doesn't exist yet** (`mouser-engine`) plus a cluster of
real defects in the transport.

---

## CRITICAL (gate the next milestone)

### A1. No runtime: `mouser-engine` doesn't exist → no liveness, reconnect, auth-gate, or anti-replay
×6 (reliability, security, concurrency, architecture). The pure state machines (`ownership`, `election`)
have **zero production callers**; there is no heartbeat loop, no timeout detection, no reconnect/mDNS-redial
supervisor, no receive-side authorization, no anti-replay tracking. **Today none of sleep/wake, Wi-Fi-drop,
or mid-session-shutdown are survivable.** Also the protocol messages the engine needs aren't defined yet
(`Heartbeat`/`Pong`/`Goodbye`, `OwnershipTransfer`/`Ack`/`FocusState`/`CapabilityState`,
`CoordinatorLease`/`Claim`/`Yield`, `StateDelta`/`Request`/`Changes`/`Snapshot`).
→ Build `mouser-engine` + `mouser-ipc` + the missing `mouser-protocol` messages. This is the next build phase.

### A2. Acceptor-sends-first deadlocks the control stream
`mouser-net/src/transport.rs` (lazy `open_bi`/`accept_bi` tied to first I/O direction). quinn requires the
opener to write before the peer can `accept_bi`; if the acceptor's first action is `send_control`, both sides
block forever. Loopback test only exercises initiator-sends-first, so it's uncaught. ×1 (concurrency, verified
against quinn source). → Establish the control stream **eagerly and symmetrically** at connect/accept
(initiator opens + writes a priming frame; acceptor accepts).

### A3. `recv_control` is not cancel-safe → permanent control-channel corruption
`mouser-net/src/transport.rs:215-245` holds the recv lock across two `read_exact`s (quinn-documented
NOT cancel-safe). Any `select!`/timeout around it (exactly how the engine will multiplex) drops partial bytes
→ every subsequent frame misparses; length-prefixed framing has no resync. ×1 (concurrency). → Buffer into a
persistent per-stream `Vec` with cancel-safe `read()`, or a dedicated reader task forwarding complete frames.

### A4. Datagram coalescing not implemented → cursor backlog/rubber-banding under load
`mouser-net/src/transport.rs` `send_motion` calls `send_datagram` with quinn's default **1 MiB** drop-oldest
buffer → tens of thousands of stale positions can queue before the newest. Violates spec §8 "coalesce
keep-newest." ×1 (performance, verified vs quinn-proto). → App-level single-slot keep-newest sender + tiny
`datagram_send_buffer_size`.

### A5. Duplicate `device_id`/SPKI derivation in `mouser-core` *and* `mouser-net`
×5 (architecture, naming, bugs, protocol, codex-arch). Two independent code paths compute the security root
that pinning depends on; `mouser-net` does **not** depend on `mouser-core`. Byte-identical today, no shared
test — silent pin-divergence risk forever. → `mouser-net` depends on `mouser-core` for `DeviceId`/identity;
keep only the TLS-cert builder + `device_id_from_cert` (calling the core derivation).

### A6. Decoders accept trailing garbage (wire malleability)
`mouser-protocol` `codec.rs` (`ciborium::from_reader`) and `datagram.rs` (`postcard::from_bytes`) ignore
bytes after the first item → two distinct frames decode to the same message; breaks the "byte-exact /
golden-vector oracle" interop contract. ×1 (edge-cases, verified end-to-end). → Strict decode: reject if any
input remains (`Cursor` position check / `postcard::take_from_bytes` remainder must be empty).

---

## HIGH

- **H1. No QUIC keep-alive / idle timeout** — no `TransportConfig`; quinn default `max_idle_timeout=30s`,
  `keep_alive=None` → idle interactive connections silently die. ×2 (reliability, concurrency). → set
  `keep_alive_interval` (~5s) + bounded idle timeout.
- **H2. Platform crates don't implement the core `InputInjection`/`InputCapture` traits** — the trait layer has
  zero implementors; spike free-functions diverge in shape (`display_id`/`ScrollUnit` dropped). ×2 (architecture,
  codex). → add adapter structs implementing the traits; spikes become private bodies.
- **H3. Capture can't suppress local input** — `InputSink::on_event -> ()` + mac tap `ListenOnly`/`Keep` only;
  no way to swallow local input while forwarding to a remote owner (core requirement). ×1 (codex-arch). →
  `CaptureDecision::{PassThrough, Suppress}` + capability-state when suppression unavailable.
- **H4. No pre-trust boundary** — `InteractiveConnection` exposes `send_control`/`send_motion` immediately;
  Hello/SAS/`channel_sig` stubbed. ×3 (security, codex-arch). → connection typestates: `Unpaired`
  (Hello/pairing only) → `Trusted` after core approval.
- **H5. No anti-replay / authorization on received input** — `recv_*` hand decoded messages back with no
  trusted-list / permission / `owner_epoch`-`seq` high-water check (spec §9). ×1 (security). → core receive
  gate: trusted ∧ capability ∧ permission ∧ current-owner; reject non-increasing `(epoch,seq)`.
- **H6. quinn connections never gracefully drained** — no `Drop`/`wait_idle`; dropped conns leave peers to
  idle-timeout. ×2 (resources, concurrency). → `shutdown()` that `close()`+`wait_idle().await`; best-effort
  `Drop`.
- **H7. Enum unknown-mapping errors for discriminants ≥ 65536** — `try_from="u16"` hard-errors instead of →
  `Unknown` (spec forward-compat). ×1 (edge-cases). → deserialize via `u64`/`Value`, saturate to `Unknown`.
- **H8. `recv_motion` conflates a corrupt datagram with a fatal error** — one bad UDP packet → `Err` a caller
  may treat as dead connection. ×1 (edge-cases). → drop-and-continue on `DatagramError`.
- **H9. Golden vectors incomplete + `mouser-testkit`/fuzzing absent** — no datagram/enum golden bytes, no
  struct unknown-field test; testkit is a 1-line stub; no `cargo-fuzz`. ×2 (protocol, testing). → datagram +
  per-enum golden vectors; testkit (fake clock + drop/reorder/latency transport + N-engine harness); fuzz
  targets for `decode_frame`/`from_cbor`/`decode_datagram`/`device_id_from_cert`.
- **H10. Tauri branch CI will fail on Linux** — adds webkit2gtk/gtk `*-sys` deps but CI installs no
  `libwebkit2gtk-4.1-dev …`; no Node/pnpm/lint step. ×1 (cross-platform, verified dep tree). → add the apt
  step + pnpm lint/build job on the tauri branch.
- **H11. Keymap gaps & no `mods` translation** — mac keymap missing F1–F12 / nav cluster / keypad (Windows has
  them); **no Linux HID→evdev keymap at all**; no `mods` bitmask→modifier translation in any adapter. ×2
  (cross-platform, bugs). → complete the per-OS tables + a shared parity test; add `mods` mapping (Cmd↔Ctrl).
- **H12. iOS missing Local-Network entitlement + Bonjour keys** — its own tech-stack §6 mandates them; absent
  from Info.plist/project.yml → no mDNS/LAN on iOS 14+. ×1 (cross-platform). → add `NSLocalNetworkUsageDescription`,
  `NSBonjourServices=["_mouser._udp"]`, multicast entitlement if needed.
- **H13. WebView2 install mode missing** — `tauri.conf.json` lacks `windows.webviewInstallMode:
  downloadBootstrapper` (mandated by windows-build.md/§8). ×1 (cross-platform). → add it.

---

## MEDIUM

- **M1. mac inject ignores `display_id`** → multi-monitor routing wrong if wired directly. (codex-arch, perf) →
  translate `(display_id,x,y)` via full display enumeration.
- **M2. Hot-path allocations** — per-message `Vec` in framing/codec/transport; fresh `CGEventSource` per mac
  injection; per-event `Vec` in linux `move_rel`; datagram encode double-alloc. (performance) → reusable
  buffers / cached source / stack arrays.
- **M3. `left_click` can leave the button logically stuck** on partial failure (down posted, up creation
  fails). (resources, bugs) → build/post both before returning.
- **M4. Election robustness** — a bare `CoordinatorClaim` is adopted as a full lease; equal-term same-holder
  lease is replayable; any peer can inflate `seen_term` via a forged-self lease. Cosmetic today (no driver),
  fix with the engine + §9 auth. (edge-cases, bugs)
- **M5. Panic-free deny is not workspace-wide** — only the two decode crates set it; `[workspace.lints]` only
  forbids unsafe. (resources) → hoist `unwrap_used`/`panic`/`indexing_slicing` to workspace clippy lints.
- **M6. Core missing `trust`/`permissions`/`cluster_state` modules** the spec assigns to it → risk they land
  ad-hoc in engine/net. (architecture, codex) → add pure core modules now (ties to A1/H5).
- **M7. `Tray` trait in core contradicts headless-engine + Tauri-owns-tray** model. (codex-arch) → move tray to
  UI over `mouser-ipc`.
- **M8. Discovery uses raw strings + silently defaults bad ports to 0.** (codex, concurrency) → typed advisory
  fields, reject malformed required values.
- **M9. CI never builds iOS (no xcodebuild) or runs JS lint** → Swift/frontend regress silently. (cross-platform)
- **M10. Indefinite-length CBOR maps accepted** despite §0.1 "definite-length"; widens accepted set. (edge-cases)
- **M11. CapabilitySet decodes an unbounded `Vec<u16>` before filtering.** (security) → cap length.

---

## LOW (selected)
- `datagram.rs` module doc still says "fixed layout, little-endian" — code is correct postcard varint; doc
  misleads a second implementer. (×4) → fix comment.
- `mouser-protocol/src/lib.rs` cites spec "v2.3" (now v2.5). 
- Unused `libc`/`core-foundation` deps in `platform-mac`. (naming)
- `Role` name collides (wire eligibility vs transport Initiator/Acceptor). (naming) → rename transport one.
- Duplicate base32 impl (folds into A5). `mac example` raw index. `platform-mac` whole-crate cfg vs
  `UNSUPPORTED` stub inconsistency. Hand-rolled `O_NONBLOCK` named `libc_nonblock`.

---

## Validated as CORRECT (no action)
Framing length math + golden Ping vector; CBOR enum/set encoding (unknown→Unknown, drop-unknown set);
ownership epoch machine (strictly-greater + reclaim-gated tiebreak — no two-owner path); election term/lease/
renew (ttl/3-elapsed) + partition-heal; DER/SPKI parser (panic-free) and `SHA-256(cert SPKI)==device_id`;
cert pinning both directions with TLS signature still verified + mandatory client auth; mac keymap values
sampled correct; no `unsafe` outside the (documented) platform crates; no production `unwrap`/`panic`.

---

## Prioritized fix plan
1. **`mouser-net` hardening** (A2, A3, A4, A5, H1, H6, H8, A6-net): identity-dep on core, eager symmetric
   control stream, cancel-safe recv, keep-alive TransportConfig, graceful drain, drop-on-corrupt motion,
   datagram keep-newest sender, strict decode.
2. **`mouser-protocol` hardening** (A6, H7, H9): strict decode, u16-overflow→Unknown, datagram+enum golden
   vectors + struct unknown-field test, fix datagram doc + v2.5 label.
3. **`mouser-engine` + `mouser-ipc` + missing messages** (A1, H4, H5, M6): the runtime — heartbeat/keep-alive,
   reconnect supervisor, ownership/election drivers, receive-side auth + anti-replay, Goodbye-on-sleep,
   ack-timeout snap-back, bulk connection + StateSnapshot.
4. **Platform↔core trait wiring** (H2, H3, H11, M1): adapter impls, capture-suppression, display_id mapping,
   complete keymaps + mods.
5. **`mouser-testkit` + fuzz** (H9): fake clock/transport + N-engine harness; cargo-fuzz targets.
6. **Cross-platform/CI** (H10, H12, H13, M9): tauri Linux deps + lint job, iOS entitlements, WebView2 mode,
   iOS/JS CI legs.
7. **Cleanups** (M2, M3, M5, M7, M8, LOWs).
