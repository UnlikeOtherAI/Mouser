# Mouser — Architecture

This document describes how Mouser is structured. It assumes the product goals
in [brief.md](brief.md): a zero-config, local-first, fault-tolerant, fully
peer-to-peer workspace shared across macOS, Windows, and Linux (with a future
mobile companion).

---

## 1. Guiding constraints

These constraints shape every decision below:

1. **No single point of failure.** There is no broker and no required server.
   Any device may leave at any time; the rest keep working.
2. **Local network only.** No cloud dependency for any core feature.
3. **Near-zero input latency.** Cursor motion must feel like one desktop.
4. **Identical UX across platforms.** The settings/layout UI must look and lay
   out the same on macOS, Windows, and Linux.
5. **Interoperability across independently-built binaries.** A Linux build and a
   Windows build, compiled separately on different machines, must speak the same
   wire protocol. This is why the protocol is specified separately in
   [communication-interface.md](communication-interface.md).

---

## 2. High-level shape

Mouser is split into a **shared Rust core**, thin **per-OS platform adapters**,
and a **cross-platform UI shell**. The same core also backs the mobile
companion through generated bindings.

```
                         ┌──────────────────────────────────────────┐
                         │              UI shell (Tauri)             │
                         │   React + TypeScript settings & layout    │
                         │      (identical on macOS/Win/Linux)       │
                         └───────────────▲──────────────────────────┘
                                         │ local IPC (UDS / named pipe)
                                         │
┌────────────────────────────────────────┴─────────────────────────────────────┐
│                          mouser-core  (Rust, platform-agnostic)                │
│                                                                                │
│  discovery │ transport │ cluster-state (CRDT) │ election │ pairing/security    │
│  input-model │ ownership/focus │ clipboard │ file-transfer │ notifications     │
│                                                                                │
│        ▲ traits: InputCapture / InputInjection / Clipboard / Tray             │
└────────┼───────────────────────────────────────────────────────────────────┬─┘
         │                                                                     │
  ┌──────┴───────┐   ┌───────────────┐   ┌───────────────┐        ┌───────────┴──────────┐
  │ platform-mac │   │ platform-win  │   │ platform-linux│        │  mobile bindings      │
  │ CGEventTap   │   │ windows-rs    │   │ evdev/uinput  │        │  (uniffi → Swift/Kt)  │
  │ CoreGraphics │   │ SendInput/RawIn│  │ libei / x11rb │        │  iOS + Android companion│
  └──────────────┘   └───────────────┘   └───────────────┘        └──────────────────────┘
```

The hot path (capturing input on the active machine and injecting it on the
target machine) lives entirely in the Rust core + platform adapters. The UI is
a **separate process** and is never in the input path — it only configures the
engine and renders state.

---

## 3. Process model

Each machine runs two cooperating pieces:

- **The engine** — a headless background service. It does discovery, holds
  cluster state, captures/injects input, and runs the network transport. It
  starts on login, runs minimized, and keeps the workspace alive with no UI
  open. This is what "runs as a background service" means in the brief.
- **The UI client** — the Tauri app (menu-bar on macOS, tray on Windows/Linux).
  It connects to the local engine over an OS-local IPC channel (Unix domain
  socket on macOS/Linux, named pipe on Windows) and presents settings, the
  layout canvas, device identification, and approval prompts.

Splitting them means the workspace survives the UI being closed, and the UI can
attach/detach freely. The engine is the unit that joins the cluster.

---

## 4. Core modules

All of these live in `mouser-core` and are platform-agnostic. Platform-specific
behaviour is reached only through traits implemented by the platform crates.

### 4.1 Discovery
mDNS / DNS-SD advertisement and browsing. Each engine advertises its identity,
name, OS, version, capabilities, and coordinator-eligibility. New devices appear
and offline devices disappear automatically. Discovery is continuous; see the
protocol spec for the service type and TXT record schema.

### 4.2 Device identity
Each engine owns a permanent **Ed25519 keypair** generated on first launch. The
**device ID is derived from the public key**, so identity survives reboots, IP
changes, and DHCP renewals. Layout and trust always reference the device ID,
never an address.

### 4.3 Transport
QUIC peer-to-peer connections (one per peer). Two logical planes share each
connection — a reliable control plane and a lossy input-motion plane. This is
the heart of the latency design and is detailed in §6.

### 4.4 Cluster state (replicated, conflict-free)
The cluster's shared state — device list, screen arrangement, aliases, input
preferences, per-device permissions — is a **CRDT**. Every engine holds a full
replica. Changes apply locally and replicate as deltas to all peers, so there is
no authoritative copy to lose. Concurrent edits (e.g. two users dragging the
layout) converge deterministically without a coordinator in the loop.

### 4.5 Leadership (coordinator)
A coordinator exists only to *serialize* a few operations that benefit from a
single decision-maker (new-device admission, conflict tie-breaking). It is
**not** a data dependency — state lives in the CRDT, not in the coordinator.
Election is **lease-based**, not Raft:

- Eligible engines (desktops by default; laptops opt out) periodically renew a
  coordinator lease announced over the control plane.
- If the lease expires (coordinator slept, shut down, left the network), the
  remaining eligible engines deterministically pick the next coordinator by a
  stable tiebreak (lowest device ID among eligible, highest uptime, etc.).
- The transition is silent; the cluster keeps operating throughout.

Full Raft is deliberately avoided — the consistency needs here are light, and a
lease + CRDT is far simpler and matches "the coordinator is not a dependency."

### 4.6 Input ownership & focus
Exactly one engine owns keyboard/mouse input at a time (the **active device**).
Ownership changes via:
- **mouse boundary crossing** (cursor leaves an edge → appears on the adjacent
  device per the layout),
- **window/desktop interaction** (clicking another machine takes ownership even
  without crossing an edge), and
- **explicit hotkeys** (jump-to-device).

Every engine tracks the current owner and focus state (Active / Standby /
Disconnected). This drives clipboard and keyboard routing.

### 4.7 Clipboard, file transfer, notifications
Clipboard sync (text/images/files, user-gated: off / text-only / full), drag-
and-drop file transfer between machines, and a coordinator-independent
notification stream (device connected/disconnected, config changed, new
coordinator). All ride the control plane.

---

## 5. Input ownership handoff (example flow)

Cursor crosses the right edge of the Mac into the Windows PC sitting to its right:

1. Mac engine's `InputCapture` detects the cursor hit the layout boundary.
2. Mac sends an `OwnershipTransfer` on the **control plane** to the Windows
   engine (reliable, ordered — this must not be lost or reordered).
3. Mac stops injecting locally, "warps" its own cursor away from the edge, and
   begins streaming pointer motion to Windows on the **input-motion plane**.
4. Windows engine injects motion via its `InputInjection` adapter; key presses,
   clicks, and scroll arrive on the control plane (never dropped).
5. Both engines update focus state and broadcast it so clipboard/keyboard
   routing follow the active device.

If the Windows engine disappears mid-session, the Mac detects the dead
connection (heartbeat timeout), reclaims local ownership, and the layout marks
Windows as Disconnected.

---

## 6. Networking planes — the latency design

This is the answer to "we need a super-fast stream of mouse coordinates,
separate from the control channel." There are **two planes over one QUIC
connection**:

### Control plane — reliable, ordered
QUIC bidirectional streams. Carries everything that must not be lost or
reordered: cluster-state deltas, config changes, ownership/focus events,
**key events, mouse button down/up, scroll**, clipboard, file transfer,
notifications, election, pairing, and heartbeats.

### Input-motion plane — unreliable, unordered
QUIC **DATAGRAM** frames (RFC 9221, supported by `quinn`). Carries **only
high-frequency pointer motion**. It is intentionally lossy:

- Each datagram is tiny (~16–24 bytes): a monotonic sequence number, a
  timestamp, and the **absolute pointer position normalized to the target
  screen** (not a relative delta).
- The receiver applies a datagram only if its sequence number is newer than the
  last one applied, and discards stale/out-of-order packets.
- Because positions are **absolute, packet loss self-heals**: the next datagram
  carries the correct current position. With relative deltas a dropped packet
  would permanently desync the cursor, so absolute is the safer primary
  representation.

### Why not a reliable stream for motion?
A retransmitted mouse position is worthless by the time it arrives — you only
ever want the *latest* position. Putting motion on a reliable, ordered stream
would introduce head-of-line blocking and retransmit stalls precisely when the
network hiccups. Datagrams give "newest wins, drop the rest," which is exactly
the right semantics for a cursor.

### Why the same QUIC connection (not a separate raw UDP socket)?
Sharing one connection means motion datagrams inherit the connection's
encryption, path validation, congestion signal, NAT keep-alive, and **connection
migration** — so the stream survives a Wi-Fi roam or address change without a new
handshake. A separate raw-UDP socket would have to re-implement all of that.
Raw UDP remains a documented fallback if datagram overhead ever proves
measurable on the target hardware.

### Reliability split, restated
| Event | Plane | Why |
|-------|-------|-----|
| Pointer **motion** | Datagram (lossy) | Only latest matters; never retransmit |
| Mouse **button** down/up | Control (reliable) | Dropping a click is unacceptable |
| **Scroll** | Control (reliable) | Discrete, must arrive |
| **Key** down/up | Control (reliable) | Dropping a keystroke is unacceptable |
| Ownership / focus | Control (reliable) | Must be ordered and delivered |
| State / clipboard / files | Control (reliable) | Correctness over latency |

### Why not MQTT / a broker?
A broker (MQTT or otherwise) is a **single point of failure** and an extra
network hop, both of which contradict the brief's peer-to-peer, no-SPOF,
zero-config principles. Mouser uses **direct QUIC peer links plus gossip** for
state fan-out instead of a central pub/sub. The user-facing terminology stays
"Coordinator," and the coordinator is never a message bus.

---

## 7. Fault tolerance & self-healing

- **Liveness** is tracked by periodic heartbeats on the control plane plus
  QUIC's own connection-level keep-alive. A missed-heartbeat timeout marks a
  peer Disconnected.
- **State** is a CRDT replicated to every engine, so no departure loses data.
- **Coordinator loss** triggers a silent lease re-election (§4.5).
- **Reconnection** is automatic via continuous mDNS discovery + identity
  pinning; the user never manually reconnects.

---

## 8. Cross-platform UI (identical settings screen)

The settings and layout UI must be **laid out identically** on all three desktop
OSes. Mouser uses **Tauri v2** with a single web frontend (React + TypeScript +
Tailwind), so the same DOM and CSS render on every platform; controls are custom-
styled (not native form widgets) and the UI font is bundled, so the layout is
identical by construction rather than by per-platform reimplementation. The
rationale and alternatives considered (Slint, Flutter, native per-OS) are in
[tech-stack.md](tech-stack.md). The engine stays in Rust; Tauri's backend *is*
Rust, so the UI shell links the same core.

---

## 9. Mobile companion

The iOS/Android companion is a **peer** that speaks the same protocol with a
restricted capability set (remote-control only, coordinator-ineligible). It runs
portrait: a **touchpad area on top** sending pointer-motion datagrams, and the
**native on-screen keyboard below** sending key events on the control plane.
Quick device selection issues an `OwnershipTransfer`. The companion reuses
`mouser-core` through **uniffi**-generated Swift and Kotlin bindings, so there is
one networking/protocol implementation across desktop and mobile.

---

## 10. Security posture

- **Explicit trust.** A new device's first connection requires approval; the
  prompt shows name, OS, address, and key fingerprint. Approved devices are
  remembered and reconnect automatically.
- **Identity pinning.** TLS certificates are pinned to the device's Ed25519
  identity, so a spoofed name/address cannot impersonate a trusted device.
- **Granular permissions.** Per-device toggles for keyboard, mouse, clipboard,
  file transfer, webcam, and audio are enforced **on receipt** in the core, not
  just hidden in the UI.

See [communication-interface.md](communication-interface.md) for the pairing
handshake and on-wire enforcement details.
