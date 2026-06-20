# Mouser — Tech Stack

This document records the chosen languages, frameworks, and libraries, and why.
It complements [architecture.md](architecture.md) (structure) and
[communication-interface.md](communication-interface.md) (wire protocol).

---

## 1. Core language — Rust

The shared engine is written in **Rust**.

**Why Rust over a shared C++ core:**

1. **Threat model.** The engine runs with the highest effective privilege on the
   machine (it injects keystrokes and mouse events) and parses data arriving over
   the network from peer devices. A memory-safety bug in a packet parser is a
   remote code-execution path on a host that can then control the keyboard. Rust
   eliminates that entire bug class at compile time. For a greenfield codebase
   with no legacy C++ to preserve, paying the C++ memory-safety tax is hard to
   justify.
2. **Concurrency.** Discovery + per-peer transport + election + state replication
   is deeply concurrent. Rust's compile-time data-race freedom plus `tokio` is a
   stronger base than hand-rolled C++ threading for a self-healing system.
3. **One core, all targets.** `uniffi` generates Swift and Kotlin bindings from
   the same Rust core for the mobile companion; `cbindgen` exposes a C ABI where
   needed. C++ → Swift/Kotlin requires hand-written shims and JNI.
4. **Build & dependencies.** Cargo workspaces + cross-compilation beat CMake
   across five targets.

C++ would only win if the team were C++-only or if we forked Synergy/Barrier's
code wholesale (we will reference their protocol/input concepts, not reuse the
code). The one genuinely C++/ObjC-leaning area — virtual camera/audio plugins —
is an isolated, separately-loaded module that talks to the core over IPC and does
not pull the core to C++.

---

## 2. Async runtime & transport

| Concern | Choice | Notes |
|---------|--------|-------|
| Async runtime | **tokio** | De-facto standard; pairs with quinn. |
| Transport | **QUIC via `quinn`** | Reliable streams *and* unreliable datagrams (RFC 9221) on one encrypted connection; connection migration survives Wi-Fi roams. |
| TLS / crypto | **`rustls`** + **`ring`** | TLS 1.3 for QUIC; certs pinned to device identity. |
| Pairing handshake | **SPAKE2** or **Noise** (`snow`) | Short-authentication-string confirmation on first contact. |
| Device identity | **Ed25519** (`ed25519-dalek`) | Permanent keypair; device ID derived from the public key. |
| Discovery | **mDNS / DNS-SD** (`mdns-sd`) | Continuous advertise + browse on the LAN. |

The two-plane design (reliable control plane, lossy input-motion datagrams) is
specified in [architecture.md §6](architecture.md) and
[communication-interface.md](communication-interface.md). **No MQTT/broker** — it
would be a single point of failure.

---

## 3. Serialization & state

| Concern | Choice | Notes |
|---------|--------|-------|
| Wire encoding (hot path) | **`postcard`** (or `bincode`) | Compact, fast, `serde`-based; ideal for tiny motion/input frames. |
| Wire encoding (control) | `serde` + compact codec | Versioned, forward-compatible message envelope. |
| Replicated cluster state | **CRDT** — `automerge` (or `yrs`) | Conflict-free; every engine holds a full replica; concurrent layout edits converge. |

Protocol versioning and capability negotiation are mandatory so independently-
built binaries interoperate — see the protocol spec.

---

## 4. OS input capture & injection

Reached only through core traits (`InputCapture`, `InputInjection`), implemented
per platform:

| OS | Capture / Injection | Crates |
|----|--------------------|--------|
| macOS | CGEventTap, CoreGraphics warp/inject | `core-graphics`, `core-foundation` |
| Windows | low-level hooks + `SendInput` / Raw Input | `windows` (official Microsoft `windows-rs`) |
| Linux (Wayland) | `libei` (emulated input) + `uinput` | `input`, `evdev`, libei bindings |
| Linux (X11) | XTEST / event injection | `x11rb` |

Notes: macOS requires Accessibility + Input Monitoring permissions. Wayland
input injection is constrained by the compositor (a protocol-level limitation,
not a language one); `libei`/portals are the forward path, with `uinput` as a
lower-level fallback.

---

## 5. UI shell — identical settings screen across macOS/Win/Linux

**Tauri v2** with a single web frontend:

| Layer | Choice |
|-------|--------|
| Shell | **Tauri v2** (Rust backend = same core; tray + autostart plugins) |
| Frontend | **React + TypeScript + Vite** |
| Styling | **Tailwind CSS** + **shadcn/ui**, bundled UI font |
| Layout canvas | SVG/Canvas drag-arrange of device rectangles |
| Package manager | **pnpm** |

**Why Tauri:** the requirement is an *identically laid-out* settings screen on
all three desktop OSes. A single web frontend renders the same DOM/CSS
everywhere; using custom-styled controls (not native form widgets) and a bundled
font makes the layout identical by construction. Tauri's backend is Rust, so the
shell links `mouser-core` directly, and bundles are small.

**Alternatives considered:**
- **Slint** (Rust-native declarative UI) — renders its own pixels, so it is
  pixel-identical and pure-Rust; viable, but a smaller component/design ecosystem
  than web for a polished settings UI.
- **Flutter desktop** — pixel-identical via Skia, but Dart adds a second language
  and a clunkier bridge to a Rust core.
- **Native per-OS (SwiftUI / WinUI / GTK)** — best OS-native feel, but directly
  violates the "identical layout" requirement and triples UI work. Rejected.

Caveat: Tauri uses each platform's system WebView (WKWebView / WebView2 /
WebKitGTK), so there can be sub-pixel font-rendering differences. For *layout*
identity (the stated requirement) this is a non-issue; if pixel-perfect rendering
is ever required, Slint is the fallback. On Windows, bundle the WebView2
evergreen bootstrapper so target machines need no manual runtime install.

---

## 6. Mobile companion

| Concern | Choice |
|---------|--------|
| iOS | Swift + SwiftUI |
| Android | Kotlin + Jetpack Compose |
| Shared logic | `mouser-core` via **uniffi** (generated Swift + Kotlin bindings) |
| Layout | Portrait: touchpad above, native OS keyboard below |

The companion is a protocol peer (remote-control capability, coordinator-
ineligible), reusing the one networking/protocol implementation. Tauri v2 mobile
is a possible alternative shell, but native is preferred for touchpad/keyboard
feel.

---

## 7. Virtual devices (future)

Webcam and audio sharing expose a real device from one machine as a virtual
device on another. These are native, separately-loaded modules talking to the
core over local IPC:

| OS | Camera | Audio |
|----|--------|-------|
| macOS | CoreMediaIO DAL / Camera Extension | CoreAudio / audio HAL plugin |
| Windows | Media Foundation virtual camera / DirectShow filter | virtual audio device |
| Linux | `v4l2loopback` | PipeWire / virtual sink-source |

---

## 8. Build, packaging & CI

| Concern | Choice |
|---------|--------|
| Workspace | Cargo workspace (`mouser-core`, `platform-*`, `mouser-ffi`, Tauri app) |
| Frontend build | Vite + pnpm |
| Installers | Tauri bundler: `.app`/`.dmg` (macOS), `.msi`/`.exe` (Windows, WebView2 evergreen), `.deb`/`.AppImage` (Linux) |
| Autostart / service | Tauri autostart plugin + OS service registration (launch on login, start minimized, optional all-users install) |
| CI | GitHub Actions matrix: macOS, Windows, Linux build + test + lint |

This is what lets Linux and Windows builds be produced on, and run on, separate
machines while remaining interoperable — the wire protocol is the contract.

---

## 9. Quality gates

Per project policy, linting is **strict and blocking**:

- **`cargo clippy -- -D warnings`** and **`cargo fmt --check`** must pass; builds
  do not pass without all lints passing.
- **`cargo test`** (headless) for the core and protocol; integration harness for
  multi-engine scenarios.
- Frontend: ESLint + TypeScript strict mode + Prettier, blocking.
- Source files kept to a 500-line cap; over the limit is a refactor signal, not a
  dumping ground.
