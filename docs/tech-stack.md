# Mouser — Tech Stack — v2

Chosen languages, frameworks, libraries, and why. Complements
[architecture.md](architecture.md) and [communication-interface.md](communication-interface.md).

> **v2** incorporates the Round 1 + Round 2 reviews ([design-review.md](design-review.md)).
> Crate facts below were verified against crates.io.

---

## 1. Core language — Rust

The engine + core are **Rust**. Rationale unchanged from v1: memory safety for a privileged,
network-facing input daemon; fearless concurrency; one core for desktop + mobile (uniffi);
Cargo cross-compilation. Toolchain is **rustup-managed**. Pin a stable channel via `rust-toolchain.toml`.

---

## 2. Async runtime, transport & crypto

| Concern | Choice | Notes |
|---------|--------|-------|
| Async runtime | **tokio** | pairs with quinn |
| Transport | **QUIC via `quinn`** | reliable streams + RFC 9221 datagrams; **two connections per peer** (interactive/bulk) |
| TLS provider | **`rustls` + `rustls-ring`** | pin stable `0.23.x` + explicit provider feature |
| Identity | **`ed25519-dalek` 2.2.x** | stable, not RC; TLS leaf key = identity key |
| Cert pinning | custom `rustls` `ServerCertVerifier`/`ClientCertVerifier` | verify `SHA-256(cert SPKI)==device_id` (supported) |
| Pairing | **self-signed TLS + identity-pin + mandatory SAS over the TLS exporter (RFC 5705) + TOFU** | one concrete flow; **not** SPAKE2/Noise (the `spake2` crate is `0.5.0-pre.0`) |
| Discovery | **`mdns-sd`** | primary; manual/pair-code fallback when multicast blocked |

No MQTT/broker (SPOF). Migration claim framed as NAT-rebind help, not guaranteed roam survival.

---

## 3. Serialization & state

| Concern | Choice | Notes |
|---------|--------|-------|
| Control encoding | **CBOR (`ciborium`)** | self-describing → safe unknown-field/version skew; serde-native, no codegen. (Protobuf/`prost` is the alternative for strict multi-language schema governance.) |
| Datagram encoding | **`postcard`** | fixed-layout `PointerMotion` only |
| Replicated state | **`automerge`** (pinned, format-versioned) | **not** "automerge or yrs" — one CRDT is part of the wire contract |

`bincode` is **removed** (Codex flagged 3.0.0 as a non-functional/placeholder release; we need only
one control codec + postcard). The normative CBOR profile — definite-length maps with string field
keys, integer enum discriminants via a custom (de)serializer mapping unknown→`Unknown` (**not**
`serde_repr`), sets as ascending integer arrays — lives in the wire spec (§0.1, Appendix C); golden
conformance vectors are the first deliverable of `mouser-protocol`. Automerge holds **bounded config only**
(permissions/trusted are a local store, not in the CRDT); liveness/presence stay ephemeral, with
snapshot/compaction to bound history.

---

## 4. OS input capture & injection — capability matrix

Reached only through core traits (`InputCapture`, `InputInjection`). **"Linux" is not one backend** and
"zero-config" doesn't hold for OS-level input — adapters report a capability state at runtime.

| Platform | Backend | Crates | Reality |
|----------|---------|--------|---------|
| macOS | CGEventTap capture + CGEvent/warp inject | `core-graphics`, `core-foundation` | needs **Accessibility + Input Monitoring** (TCC); **Secure Event Input** can suppress capture in password fields; lock screen = local only |
| Windows | WH_*_LL hooks / Raw Input + `SendInput` | `windows` (windows-rs) | **UIPI**: a normal process can't inject into elevated apps; **UAC secure desktop / lock** block it → optional signed `uiAccess` helper, off by default |
| Linux X11 | XTEST | `x11rb` (xtest feature) | works; supported tier |
| Linux Wayland | libei + portal | **`reis`** (libei; *there is no `libei` crate*), **`ashpd`** (xdg `RemoteDesktop`/`InputCapture`) | compositor-dependent; capture is barrier-triggered, may be filtered/paused on lock; **initial targets GNOME/Mutter + KDE/KWin**; others fall back to uinput helper or X11 |
| Linux fallback | uinput ioctl | **`input-linux`** / direct evdev (the `uinput` crate is stale, 2018) | privileged (`/dev/uinput` via udev/polkit); not zero-config |

When input is blocked (secure desktop, lock, missing permission, unsupported compositor), the engine
broadcasts that state and **returns ownership to the source**. First-run onboarding deep-links
to the exact OS settings pane and live-checks grants.

---

## 5. UI shell

| Layer | Choice |
|-------|--------|
| Shell | **Tauri v2** — UI client only; **does not own the daemon** |
| Frontend | React + TypeScript + Vite |
| Styling | Tailwind + custom controls (WCAG 2.2 AA, full keyboard nav incl. layout canvas) |
| Layout canvas | SVG/Canvas, **per-monitor** rectangles |
| Package manager | **pnpm** |

Goal is consistent **layout/IA**, accessible and native-*feeling* per platform — not pixel-cloned native
widgets. Tauri uses each OS WebView (WKWebView/WebView2/WebKitGTK); bundle the WebView2 evergreen
bootstrapper on Windows. Alternatives (Slint pixel-identical/pure-Rust, Flutter, native) recorded as rejected.

---

## 6. Mobile companion

iOS Swift/SwiftUI, Android Kotlin/Compose, sharing `mouser-core` via **uniffi** (pre-1.0 → expose a narrow
**`mouser-ffi`** facade: connect, pair, send motion, send key, observe state — not raw core internals).
The companion is **outbound-only** (it dials engines, never accepts inbound) and **coordinator-ineligible**,
so it needs no listener. iOS requires the **Local Network entitlement** + `NSLocalNetworkUsageDescription`
and a `NSBonjourServices` entry for `_mouser._udp`; input is **foreground-only** (background suspends the
radio). uniffi async surfaces as a callback interface. Portrait: touchpad (motion datagrams) above, native
keyboard (HID key events) below.

---

## 7. Virtual devices (future — separate high-risk subsystem)

Webcam/audio sharing is **not a small toggle**; it gets its own architecture doc covering signing/notarization,
min OS versions, lifecycle, crash isolation, and CI signing. Media uses **separate transport/congestion** from
input (live 1080p contends with motion otherwise) with hardware encode + adaptive bitrate.

| OS | Camera | Audio |
|----|--------|-------|
| macOS | CoreMediaIO Camera Extension (system-extension signing + notarization) | CoreAudio HAL plugin |
| Windows | Media Foundation virtual camera | driver-signed virtual audio |
| Linux | `v4l2loopback` (DKMS / Secure Boot friction) | PipeWire virtual sink/source |

---

## 8. Workspace, build, packaging & CI

Cargo workspace crates (one responsibility each, ≤500 lines/file):
`mouser-protocol` (messages, ALPN, codec rules, **golden vectors**, versioning),
`mouser-core` (identity, trust, permissions, ownership epochs, CRDT schema, election),
`mouser-net` (quinn 2-connection transport, mDNS), `platform-{mac,win,linux}`,
`mouser-engine` (daemon binary), `mouser-ipc` (typed UDS/named-pipe),
`mouser-ffi` (uniffi facade), `mouser-testkit` (fake clock/transport/adapters + N-engine harness),
`apps/desktop` (Tauri UI).

Packaging: `.app`/`.dmg`, `.msi`/`.exe` (WebView2 evergreen), `.deb`/`.AppImage`. **Signing/notarization is a
separate workstream**: macOS Developer ID + notarization, Windows code signing (SmartScreen). CI in
layers: PR checks → nightly unsigned packaging smoke → protected signed release jobs (Apple/Windows secrets).

---

## 9. Quality gates

- **`cargo clippy -- -D warnings`** + **`cargo fmt --check`** blocking.
- Decode paths: `deny(unwrap_used, panic, indexing_slicing)` + **`cargo-fuzz`** corpus on protocol + CRDT-apply.
- **`mouser-testkit`** built first: fake clock, fake transport (drop/reorder/latency), fake platform adapters,
  ≥3 in-process engines; asserts ownership handoff, CRDT convergence, reconnect, stale-message rejection,
  and **latency SLO** (≤5 ms wired / ≤15 ms Wi-Fi).
- Real-device acceptance per platform (macOS native, iOS simulator, Linux over SSH).
- Frontend: ESLint + TS strict + Prettier + **a11y checks** (WCAG 2.2 AA) blocking.
- 500-line file cap.
