# Mouser — Tech Stack — v2

Chosen languages, frameworks, libraries, and why. Complements
[architecture.md](architecture.md) and [communication-interface.md](communication-interface.md).

> **v2** incorporates [design-review.md](design-review.md); Round 1 fixes tagged `(R1: Fn)`.
> Crate facts below were verified against crates.io.

---

## 1. Core language — Rust

The engine + core are **Rust**. Rationale unchanged from v1: memory safety for a privileged,
network-facing input daemon; fearless concurrency; one core for desktop + mobile (uniffi);
Cargo cross-compilation. Toolchain is **rustup-managed** (R1: needed for iOS/cross targets —
Homebrew rust only ships the host std). Pin a stable channel via `rust-toolchain.toml`.

---

## 2. Async runtime, transport & crypto

| Concern | Choice | Notes |
|---------|--------|-------|
| Async runtime | **tokio** | pairs with quinn |
| Transport | **QUIC via `quinn`** | reliable streams + RFC 9221 datagrams; **two connections per peer** (interactive/bulk, R1: F5) |
| TLS provider | **`rustls` + `rustls-ring`** | pin stable `0.23.x` + explicit provider feature (R1: F48) |
| Identity | **`ed25519-dalek` 2.2.x** | stable, not RC (R1: F48); TLS leaf key = identity key (R1: F8) |
| Cert pinning | custom `rustls` `ServerCertVerifier`/`ClientCertVerifier` | verify `SHA-256(cert SPKI)==device_id` (supported; R1: F8) |
| Pairing | **self-signed TLS + identity-pin + mandatory SAS over the TLS exporter (RFC 5705) + TOFU** | one concrete flow; **not** SPAKE2/Noise (the `spake2` crate is `0.5.0-pre.0`) (R1: F9) |
| Discovery | **`mdns-sd`** | primary; manual/pair-code fallback when multicast blocked (R1: F-L10) |

No MQTT/broker (SPOF). Migration claim framed as NAT-rebind help, not guaranteed roam survival (R1: F49).

---

## 3. Serialization & state (R1: F1, F3)

| Concern | Choice | Notes |
|---------|--------|-------|
| Control encoding | **CBOR (`ciborium`)** | self-describing → safe unknown-field/version skew; serde-native, no codegen. (Protobuf/`prost` is the alternative for strict multi-language schema governance.) |
| Datagram encoding | **`postcard`** | fixed-layout `PointerMotion` only |
| Replicated state | **`automerge`** (pinned, format-versioned) | **not** "automerge or yrs" — one CRDT is part of the wire contract (R1: F3) |

`bincode` is **removed** (Codex flagged 3.0.0 as a non-functional/placeholder release; we need only
one control codec + postcard). Automerge holds **bounded config only**; liveness/presence stay ephemeral,
with snapshot/compaction to bound history (R1: F39).

---

## 4. OS input capture & injection — capability matrix (R1: F13, F24)

Reached only through core traits (`InputCapture`, `InputInjection`). **"Linux" is not one backend** and
"zero-config" doesn't hold for OS-level input — adapters report a capability state at runtime.

| Platform | Backend | Crates | Reality |
|----------|---------|--------|---------|
| macOS | CGEventTap capture + CGEvent/warp inject | `core-graphics`, `core-foundation` | needs **Accessibility + Input Monitoring** (TCC); **Secure Event Input** can suppress capture in password fields; lock screen = local only |
| Windows | WH_*_LL hooks / Raw Input + `SendInput` | `windows` (windows-rs) | **UIPI**: a normal process can't inject into elevated apps; **UAC secure desktop / lock** block it → optional signed `uiAccess` helper, off by default |
| Linux X11 | XTEST | `x11rb` (xtest feature) | works; supported tier |
| Linux Wayland | libei + portal | **`reis`** (libei; *there is no `libei` crate*), **`ashpd`** (xdg `RemoteDesktop`/`InputCapture`) | compositor-dependent; capture is barrier-triggered and may be filtered/paused on lock; **supported only on verified compositors** |
| Linux fallback | uinput ioctl | **`input-linux`** / direct evdev (the `uinput` crate is stale, 2018) | privileged (`/dev/uinput` via udev/polkit); not zero-config |

When input is blocked (secure desktop, lock, missing permission, unsupported compositor), the engine
broadcasts that state and **returns ownership to the source** (R1: F12, F24). First-run onboarding deep-links
to the exact OS settings pane and live-checks grants.

---

## 5. UI shell

| Layer | Choice |
|-------|--------|
| Shell | **Tauri v2** — UI client only; **does not own the daemon** (R1: F11) |
| Frontend | React + TypeScript + Vite |
| Styling | Tailwind + custom controls (WCAG 2.2 AA, full keyboard nav incl. layout canvas — R1: F46/F47) |
| Layout canvas | SVG/Canvas, **per-monitor** rectangles (R1: F10) |
| Package manager | **pnpm** |

Goal is consistent **layout/IA**, accessible and native-*feeling* per platform — not pixel-cloned native
widgets (R1: F47). Tauri uses each OS WebView (WKWebView/WebView2/WebKitGTK); bundle the WebView2 evergreen
bootstrapper on Windows. Alternatives (Slint pixel-identical/pure-Rust, Flutter, native) recorded as rejected.

---

## 6. Mobile companion

iOS Swift/SwiftUI, Android Kotlin/Compose, sharing `mouser-core` via **uniffi** (pre-1.0 → expose a narrow
**`mouser-ffi`** facade: connect, pair, send motion, send key, observe state — not raw core internals, R1: F-X11).
Portrait: touchpad (motion datagrams) above, native keyboard (HID key events) below.

---

## 7. Virtual devices (future — separate high-risk subsystem) (R1: F45)

Webcam/audio sharing is **not a small toggle**; it gets its own architecture doc covering signing/notarization,
min OS versions, lifecycle, crash isolation, and CI signing. Media uses **separate transport/congestion** from
input (live 1080p contends with motion otherwise) with hardware encode + adaptive bitrate.

| OS | Camera | Audio |
|----|--------|-------|
| macOS | CoreMediaIO Camera Extension (system-extension signing + notarization) | CoreAudio HAL plugin |
| Windows | Media Foundation virtual camera | driver-signed virtual audio |
| Linux | `v4l2loopback` (DKMS / Secure Boot friction) | PipeWire virtual sink/source |

---

## 8. Workspace, build, packaging & CI (R1: F11, F33)

Cargo workspace crates (one responsibility each, ≤500 lines/file):
`mouser-protocol` (messages, ALPN, codec rules, **golden vectors**, versioning),
`mouser-core` (identity, trust, permissions, ownership epochs, CRDT schema, election),
`mouser-net` (quinn 2-connection transport, mDNS), `platform-{mac,win,linux}`,
`mouser-engine` (daemon binary), `mouser-ipc` (typed UDS/named-pipe),
`mouser-ffi` (uniffi facade), `mouser-testkit` (fake clock/transport/adapters + N-engine harness),
`apps/desktop` (Tauri UI).

Packaging: `.app`/`.dmg`, `.msi`/`.exe` (WebView2 evergreen), `.deb`/`.AppImage`. **Signing/notarization is a
separate workstream** (R1: F33): macOS Developer ID + notarization, Windows code signing (SmartScreen). CI in
layers: PR checks → nightly unsigned packaging smoke → protected signed release jobs (Apple/Windows secrets).

---

## 9. Quality gates

- **`cargo clippy -- -D warnings`** + **`cargo fmt --check`** blocking.
- Decode paths: `deny(unwrap_used, panic, indexing_slicing)` + **`cargo-fuzz`** corpus on protocol + CRDT-apply (R1: F27).
- **`mouser-testkit`** built first: fake clock, fake transport (drop/reorder/latency), fake platform adapters,
  ≥3 in-process engines; asserts ownership handoff, CRDT convergence, reconnect, stale-message rejection,
  and **latency SLO** (≤5 ms wired / ≤15 ms Wi-Fi, R1: F-L3).
- Real-device acceptance per platform (macOS native, iOS simulator, Linux over SSH).
- Frontend: ESLint + TS strict + Prettier + **a11y checks** (WCAG 2.2 AA, R1: F46) blocking.
- 500-line file cap.
