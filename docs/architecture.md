# Mouser — Architecture — v2

How Mouser is structured, per the goals in [brief.md](brief.md): a local-first,
fault-tolerant, peer-to-peer workspace shared across macOS, Windows, and Linux,
with a mobile companion.

> **v2** incorporates [design-review.md](design-review.md). Round 1 fixes are tagged `(R1: Fn)`.
> The byte-level contract is [communication-interface.md](communication-interface.md).

---

## 1. Guiding constraints

1. **No single point of failure.** No broker, no required server. Any device may leave;
   the rest keep working. (Honest scope: a brief coordinator-election gap affects only the
   cosmetic coordinator label, not input/state — see §4.5.)
2. **Local network only.** No cloud dependency for any core feature.
3. **Near-zero input latency.** Target ≤ 5 ms wired / ≤ 15 ms good Wi-Fi, motion-to-injection.
4. **Consistent cross-platform UI.** Same information architecture and layout on macOS/Win/Linux
   (R1: F47 — native-feeling, accessible controls, not pixel-cloned native widgets).
5. **Interoperability across independently-built binaries** — guaranteed by the wire spec.
6. **Honesty about platform limits** (R1: F12, F13): "zero *network* config" + a one-time OS
   permission grant; Linux/Wayland support is compositor-dependent (capability matrix in tech-stack §4).

---

## 2. High-level shape

A shared **Rust core**, thin **per-OS platform adapters**, a **headless engine daemon**, and a
**Tauri UI client**. The same core backs the mobile companion via uniffi bindings.

```
   ┌──────────────────────────┐        ┌───────────────────────────┐
   │  mouser-ui (Tauri)       │        │  mobile companion          │
   │  React/TS tray+settings  │        │  Swift / Kotlin            │
   └─────────▲────────────────┘        └─────────▲─────────────────┘
             │ local IPC (UDS / named pipe)       │ uniffi (Swift/Kotlin)
   ┌─────────┴────────────────────────────────────┴─────────────────┐
   │  mouser-engine  (headless daemon — the cluster member)          │
   │   └ links mouser-core + platform adapter                        │
   ├────────────────────────────────────────────────────────────────┤
   │  mouser-core (Rust, platform-agnostic)                          │
   │  protocol · transport(2-conn QUIC) · CRDT state · election ·    │
   │  pairing/security · ownership(epoch) · clipboard · files        │
   │  traits: InputCapture / InputInjection / Clipboard / Tray       │
   └───────┬───────────────┬───────────────┬────────────────────────┘
       platform-mac     platform-win     platform-linux
       CGEventTap       windows-rs       x11rb(xtest) / reis(libei)+ashpd / input-linux
```

The input hot path lives entirely in `mouser-engine` + `mouser-core` + the platform adapter.
The UI is a separate process, never in the input path.

---

## 3. Process model (R1: F11)

Three artifacts per machine:
- **`mouser-engine`** — the headless daemon. Discovery, cluster state, input capture/injection,
  transport. Autostarts on login, runs with no UI, and **is the unit that joins the cluster**.
  It owns its own lifecycle (launchd/agent on macOS, per-user service on Windows, systemd-user
  on Linux). Tauri is **not** the daemon (R1: F11).
- **`mouser-ui`** — the Tauri tray/menu-bar + settings client, connecting to the local engine
  over UDS (macOS/Linux) / named pipe (Windows). The IPC is access-controlled (0600 / SO_PEERCRED
  / pipe ACL, R1: F44).
- **optional privileged helper** — only where elevated input injection is explicitly enabled
  (Windows `uiAccess`, Linux `/dev/uinput` via udev/polkit). Off by default.

---

## 4. Core modules

### 4.1 Discovery
mDNS/DNS-SD advertise+browse (spec §4). Advisory only; trust is established separately. Operational
fallback when multicast is blocked (VLAN/VPN/firewall): manual add by host or pair code (R1: F-L10).

### 4.2 Device identity (R1: F8)
Permanent Ed25519 keypair; `device_id = SHA-256(pubkey)` (full 32 bytes). The TLS leaf key **is**
the identity key, so every connection verifies `SHA-256(cert) == device_id` before trust. Trust is
keyed on `device_id` alone — never name/address (R1: F36).

### 4.3 Transport (R1: F5)
**Two QUIC connections per peer**: an *interactive* connection (control stream + motion datagrams)
and a *bulk* connection (files/images/snapshots, rate-limited). This is the central latency fix —
bulk transfer can no longer share the interactive congestion window. Detail in §6.

### 4.4 Cluster state — replicated config only (R1: F3, F39)
Pinned **automerge** CRDT, format-versioned. Holds **persistent config only**: per-monitor layout,
aliases, input prefs, per-device permissions, trusted list (Appendix A of the spec). **Liveness,
presence, and input ownership are NOT in the CRDT** — ownership is the epoch token (§4.6); presence
is ephemeral gossip. Concurrent layout edits converge, then a **deterministic post-merge
normalization** re-derives the edge-adjacency map identically on every engine (R1: F18). Sync uses
delta gossip + periodic `StateRequest` anti-entropy (R1: F14). History is compacted/snapshotted to
bound `StateSnapshot` size.

### 4.5 Leadership (coordinator) (R1: F7, F19, F20)
The coordinator **serializes nothing in the steady state** — state is the CRDT, admission is local
approval, ownership is the epoch token. It is a presented "who's in charge" label plus an optional
unattended-admission fallback; an all-ineligible cluster (e.g. two laptops) works with no coordinator.
Election is lease-based on **local-monotonic TTL** (never cross-machine wall-clock), with Raft-style
**term** rules (higher term wins; equal term → lowest `device_id`), giving deterministic resolution
and defined partition-heal. (Spec §7.10.)

### 4.6 Input ownership & focus (R1: F6, F21, F30)
Exactly one device owns input, modeled as a **single token with a monotonic `owner_epoch`**. Only the
current owner mints `epoch+1`; receivers accept only strictly-higher epochs (ties → lower `device_id`);
transfers require an ack. Ownership changes via:
- **edge crossing** (per the normalized per-monitor layout),
- **explicit hotkey / UI selection**, and
- **local reclaim** — using a machine's *own* hardware reclaims ownership (R1: F21, replaces the
  incoherent v1 "click a remote window").
A **global panic hotkey** unconditionally reclaims local ownership regardless of cluster state, and a
handoff that isn't acked within a timeout snaps back (R1: F30 — covers a target that is asleep/hung/
on a secure desktop). On owner heartbeat-timeout the physically-attached device reclaims.

### 4.7 Clipboard, file transfer, notifications
Clipboard (text on interactive, images/files on bulk; hash-dedup + loop prevention), drag-drop file
transfer (bulk, sanitized paths, quarantine dir), debounced notifications (spec §7.7–7.9).

---

## 5. Input ownership handoff (example)

Cursor crosses the Mac's right edge into the Windows PC:
1. Mac (current owner) detects the edge per the normalized layout, mints `owner_epoch+1`, sends
   `OwnershipTransfer{to:win, owner_epoch, layout_version}` and awaits `OwnershipAck`.
2. On ack, Mac stops local injection, warps its cursor off the edge, and streams `PointerMotion`
   datagrams on the **interactive** connection; keys/buttons/scroll go reliable, replay-checked.
3. Both broadcast `FocusState{owner, owner_epoch}`; clipboard/keyboard routing follow.
If Windows is unreachable (no ack, or heartbeat-timeout), Mac retains/reclaims ownership and marks
Windows Disconnected; the user can also hit the panic hotkey.

---

## 6. Networking planes — latency design (R1: F5, F22)

**Two QUIC connections per peer.** The *interactive* connection carries the reliable control stream
(ownership/focus, keys/buttons/scroll, small CRDT deltas, liveness) and the **motion datagram plane**
(RFC 9221). The *bulk* connection carries files, clipboard images, and state snapshots, app-rate-limited.

Why two and not one: QUIC streams avoid head-of-line blocking but **share one congestion controller +
pacer**, and DATAGRAM frames are congestion-controlled (RFC 9221 §5). On a single connection a large
transfer fills the window and motion datagrams get paced behind it or dropped (the v1 "a transfer never
blocks input" claim was wrong — R1: F5). Separating connections isolates the interactive congestion domain.

Motion is **event-driven** (send per input event; coalesce-keep-newest only under send-buffer pressure;
~1000 Hz cap), carrying **integer logical pixels + `display_id`** in the target display's coordinate
space (R1: F10, F22). Loss self-heals (absolute). Connection migration handles same-subnet roams; full
reconnect uses identity-pinned mDNS rediscovery (migration is help, not a guarantee — R1: F49).

---

## 7. Fault tolerance

Heartbeats (1 s; dead after 3 misses) + QUIC keep-alive track liveness. CRDT config replicates to all;
anti-entropy repairs gaps. Coordinator loss triggers a silent monotonic-TTL re-election. Reconnection is
automatic via continuous mDNS + identity pinning; a fresh joiner pulls a `StateSnapshot` from any live
peer (CRDT makes the source irrelevant to correctness).

---

## 8. Cross-platform UI (R1: F47, F46)

Tauri v2 + a single React/TS frontend gives **consistent layout** across macOS/Win/Linux. Controls are
custom-styled but must meet **WCAG 2.2 AA**, expose correct accessibility roles, and support keyboard-only
operation including the layout canvas (arrow-key nudging) — accessibility is a quality gate (R1: F46).
The engine stays in Rust; Tauri's backend links the core but does **not** own the daemon lifecycle (§3).

---

## 9. Mobile companion

A protocol peer (capability `remote_control_only`, coordinator-ineligible) reusing `mouser-core` via
uniffi (Swift/Kotlin). Portrait: touchpad on top → motion datagrams; native keyboard below → HID
`KeyEvent`s on the control stream. Quick device selection issues an `OwnershipTransfer` (UiSelect). A
persistent "Controlling: <device>" banner + haptics on tap/edge (R1: F-H4).

---

## 10. Security posture (R1: F8, F9, F25, F26, F31)

- **Explicit trust**, key-pinned: approval prompt shows the cert-derived id + a **mandatory** 6-digit
  SAS compared on both screens (defeats first-contact MITM). Identity proof is signed over the TLS
  exporter (channel-bound).
- **Authority is local**: per-device permissions and the trusted list are local policy, authored only by
  a device about its peers — never peer-writable CRDT (R1: F31). Capabilities are advisory; the local
  permission is authoritative. Enforced on receipt on both planes, before any platform adapter.
- **Trusted-peer abuse mitigations**: per-peer input rate-limit/burst cap, "remote input only when
  unlocked" (default on), visible active-owner indicator + optional first-input confirm, peer-initiated
  ownership is a request not a grab (R1: F26).
- **DoS**: QUIC Retry/address validation, per-IP rate limits, device-list caps, size caps + fuzzed
  panic-free decoders (R1: F27, F28).
- **Cluster trust onboarding** (R1: F29): a device approved into the cluster once propagates as a trusted
  entry (with the local-authority caveat), avoiding N² approvals; per-pair pinning still applies.
