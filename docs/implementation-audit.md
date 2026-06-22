# Mouser Implementation Audit

Date: 2026-06-22

This document reconciles five focused read-only audits plus a local dependency
inventory. It is intended to answer one question: what is actually implemented
end-to-end, what is only partially wired, and what is still scaffold or missing.

## Overall Verdict

Mouser is not fully implemented end-to-end yet.

The repo contains a real transport/input foundation:

- `mouserd` can advertise over mDNS, accept or dial a single peer, and run the
  interactive QUIC input runtime.
- The pure engine can transfer ownership, forward mouse/key/button/scroll input,
  inject on the receiving side, heartbeat, and reclaim on timeout.
- macOS, Windows, and Linux platform input adapters exist.
- The desktop Tauri shell and tray behavior exist.
- Clipboard, file transfer, bulk QUIC, and mobile FFI crates contain serious
  lower-level work.

But major product features are still not end-to-end:

- Shared clipboard is not wired through `mouserd`, runtime, FFI, or UI.
- File transfer and drag/drop are not wired through the daemon.
- Pairing/SAS/trust gating is not implemented in the active daemon path.
- Daemon identity is generated fresh on each run, so peer trust cannot persist.
- Desktop settings mostly update local React/Tauri state, not engine behavior.
- Mobile apps have UI and some FFI wrappers, but discovery/connect is not wired
  into screens, and the mobile FFI currently binds to loopback.
- Docs contain stale and overclaiming statements.

## Audit Slices

| Slice | Result |
| --- | --- |
| Daemon/network/runtime | Real single-peer interactive daemon exists; trust, persistent identity, reconnect, bulk, firewall automation, and SAS are missing or partial. |
| Input control | Core/platform input is real; any-to-any is only source-capable single-peer, not a full cluster. Ctrl-C can be forwarded as keystrokes, but clipboard sync is separate and absent. |
| Clipboard/files/bulk | Protocol, pure engines, platform adapters, and bulk primitives exist; daemon/runtime/UI do not wire them together. |
| Desktop/tray/settings | Tray and local device display are real; most settings are local-only; no daemon lifecycle or IPC integration exists. |
| Mobile/FFI/docs | Mobile FFI has real source-controller methods; app-level connect/discovery is not wired; clipboard is mock/local-only; docs overclaim. |

## Current Windows Machine State

At the time this audit was written, this machine was temporarily running
`mouserd.exe` in explicit `target` mode while the Windows keyboard/touchpad
sluggishness path was isolated. The Windows capture hot path has since been
moved off the low-level hook callback, so the intended desktop-launched mode is
again normal `auto`.

## Functionality Matrix

### 1. Build And Packaging

Status: partial.

Implemented:

- Rust workspace exists with protocol, core, net, engine, platform, clipboard,
  files, FFI, and desktop crates.
- Desktop Tauri app can build Windows release bundles.
- `mouserd` release binary builds on Windows.

Partial or missing:

- Installer/autostart behavior is documented but not wired into the Tauri bundle
  in the repo.
- No app-owned daemon lifecycle integration exists.
- No `mouser-ipc` crate exists even though docs reference one.

Evidence:

- Workspace members: `Cargo.toml`
- Desktop Tauri bundle config: `apps/desktop/src-tauri/tauri.conf.json`
- Desktop backend comment says it does not own daemon lifecycle:
  `apps/desktop/src-tauri/src/lib.rs`

Priority:

1. Add daemon sidecar/install lifecycle.
2. Add `mouser-ipc` or replace docs with the actual IPC plan.
3. Make installer/autostart/firewall claims match real installer behavior.

### 2. Daemon Modes And Discovery

Status: implemented for single-peer discovery and direct connect; unsafe/incomplete
for product use.

Implemented:

- `mouserd` supports `auto`, `source`, `target`, `connect <host:port>`, and
  `probe <host:port>`.
- Windows, macOS, and Linux default to `auto`.
- mDNS advertise/browse is wired for non-direct modes.
- Auto mode uses lower `device_id` to avoid double dialing.

Partial or missing:

- Exactly one connection is formed; there is no multi-peer cluster supervisor.
- No reconnect/backoff supervisor.
- No persistent identity: `mouserd` calls `DeviceIdentity::generate()`.
- Windows firewall handling is only a console hint.

Evidence:

- Modes/defaults: `crates/mouser-engine/src/bin/mouserd.rs`
- mDNS types: `crates/mouser-net/src/discovery.rs`
- Local advert caps currently advertise keyboard/mouse only and `bport: 0`:
  `crates/mouser-engine/src/discovery.rs`

Priority:

1. Persist per-user identity.
2. Add trust and permission stores.
3. Add reconnect supervisor.
4. Add real firewall/install support or keep it explicitly manual.

### 3. Interactive QUIC Runtime

Status: real for input path; incomplete for full protocol.

Implemented:

- TLS/QUIC interactive transport exists.
- Control stream and datagram motion paths are used by `RuntimeHandle`.
- Runtime receives control, receives motion, sends control/motion, and ticks
  heartbeat.
- Core handles ownership transfer, key/button/scroll, motion, ping/pong,
  heartbeat, and goodbye.

Partial or missing:

- `Hello`, `HelloAck`, `PairingResult`, `CapabilityState`, `OwnershipRequest`,
  and `PointerModeReq` exist as protocol structs but are not active daemon
  behavior.
- Motion control-fallback exists in the network layer but is not consumed by the
  engine runtime.
- Injection errors are ignored; no capability downgrade is emitted.
- Ownership ACK tracking is best-effort only.

Evidence:

- Runtime tasks: `crates/mouser-engine/src/runtime.rs`
- Core control handling: `crates/mouser-engine/src/core.rs`
- Protocol message definitions: `crates/mouser-protocol/src/messages.rs`

Priority:

1. Gate input on trust/session state.
2. Wire capability/focus/request/pointer-mode messages.
3. Track ACK/timeout and recover cursor/input cleanly.
4. Handle datagram fallback and injection failure.

### 4. Desktop Input Control

Status: real but narrow.

Implemented:

- `InputCapture`, `InputInjection`, and `InputSink` platform contracts exist.
- Engine can forward local cursor/button/key/scroll events.
- Engine gates incoming injection by owner epoch and anti-replay counters.
- macOS, Windows, and Linux platform adapters implement capture/injection.
- Windows capture hooks now return from the low-level callback using cached
  suppression state; engine dispatch runs on a worker and cursor bursts are
  coalesced.

Partial or missing:

- "Any-to-any" is source-capable single-peer, not arbitrary multi-machine
  control.
- Windows auto/source control has been re-enabled after moving low-level hook
  work off the callback thread.
- Local hardware reclaim is incomplete.
- Panic hotkey is not implemented.
- Runtime does not query `can_suppress()` or downgrade if suppression is missing.
- Ctrl-C/Ctrl-V is not proven end-to-end across OSes, especially Cmd/Ctrl swap.

Evidence:

- Traits: `crates/mouser-core/src/platform.rs`
- Core input path: `crates/mouser-engine/src/core.rs`
- Windows adapter: `crates/platform-win/src/adapter.rs`
- macOS adapter: `crates/platform-mac/src/adapter.rs`
- Linux adapter/capture: `crates/platform-linux/src/adapter.rs`,
  `crates/platform-linux/src/capture.rs`

Priority:

1. Implement panic/local reclaim.
2. Add Ctrl-C/Ctrl-V/Cmd-C/Cmd-V cross-platform tests.
3. Wire suppression capability and blocked-input state.
4. Replace hard-coded side-by-side layout with real cluster layout.

### 5. Shared Clipboard

Status: not end-to-end implemented.

Implemented:

- Clipboard protocol messages exist.
- `mouser-clipboard` pure engine exists and has tests.
- Platform clipboard adapters exist for macOS, Windows, and Linux.
- Desktop/mobile clipboard settings UI exists.

Partial or missing:

- `mouser-engine` does not depend on `mouser-clipboard`.
- `mouserd` does not instantiate clipboard adapters.
- Runtime/core do not dispatch `TYPE_CLIPBOARD_*`.
- Desktop progress list is hardcoded empty.
- Mobile clipboard UI is mock/local-only.
- Core/platform clipboard format types need an integration/conversion layer.

Evidence:

- Protocol: `crates/mouser-protocol/src/messages.rs`
- Clipboard engine: `crates/mouser-clipboard/src/engine.rs`
- Engine manifest lacks clipboard dependency:
  `crates/mouser-engine/Cargo.toml`
- Runtime only has `Control` and `Motion` outbound lanes:
  `crates/mouser-engine/src/runtime.rs`
- Desktop clipboard UI local-only:
  `apps/desktop/src/sections/clipboard-section.tsx`

Priority:

1. Add daemon/runtime clipboard driver.
2. Snapshot local clipboard reps through platform adapters.
3. Dispatch offer/pull/data through control/bulk.
4. Write verified inbound clips to OS clipboard.
5. Feed settings/progress through IPC/FFI.

### 6. File Transfer And Drag/Drop

Status: library-level implementation, not product-wired.

Implemented:

- File protocol messages exist.
- `mouser-files` sender/receiver engines exist.
- Bulk QUIC primitives exist.
- macOS native drag/drop spike exists.

Partial or missing:

- `mouser-engine` does not depend on `mouser-files`.
- `mouserd` does not bind/advertise a usable bulk port.
- No production path from drag detection to sender to bulk stream to receiver.
- No desktop/mobile UI or daemon lifecycle for file transfer.

Evidence:

- File protocol: `crates/mouser-protocol/src/messages.rs`
- Sender/receiver: `crates/mouser-files/src/sender.rs`,
  `crates/mouser-files/src/receiver.rs`
- Bulk primitives: `crates/mouser-net/src/bulk.rs`
- mDNS advert bport: `crates/mouser-engine/src/discovery.rs`

Priority:

1. Bind and advertise bulk endpoint in `mouserd`.
2. Wire sender/receiver to bulk streams.
3. Add target quarantine/save path.
4. Add E2E tests for file transfer and drag/drop.

### 7. Desktop App, Tray, And Settings

Status: tray/local shell implemented; backend settings mostly scaffold.

Implemented:

- Tauri v2 desktop app exists.
- Tray menu exists: Show, Hide, Quit.
- Close-to-tray works when tray icon is visible.
- Tray icon visibility toggle calls Tauri and also toggles taskbar visibility.
- Local device and monitors are read from Tauri APIs.

Partial or missing:

- Most visible settings are local React state only.
- Tray visibility persistence is localStorage plus in-memory backend default; it
  is not a durable native config.
- No daemon start/stop/status commands.
- Devices screen shows only local device; peers await engine IPC.
- Layout canvas mutates React state only.
- Clipboard transfer progress is hardcoded empty.

Evidence:

- Backend commands: `apps/desktop/src-tauri/src/lib.rs`
- App shell: `apps/desktop/src/app.tsx`
- Workspace/local device: `apps/desktop/src/lib/use-workspace.ts`
- Local settings sections: `apps/desktop/src/sections/*.tsx`

Priority:

1. Add IPC command/event surface.
2. Persist settings natively and make UI reflect backend state.
3. Wire daemon lifecycle/status.
4. Disable or mark controls that are not enforced yet.

### 8. Mobile Apps And FFI

Status: partial FFI and UI; not a finished companion product.

Implemented:

- `mouser-ffi` exposes `MobileClient`.
- FFI can connect, start a source-mode engine, and send pointer/button/key/scroll.
- iOS has a wrapper over UniFFI calls.
- Android has a wrapper over UniFFI calls.

Partial or missing:

- Mobile screen-level connect/discovery is not wired.
- iOS peers are empty until future discovery.
- Android has fixed demo peers.
- iOS text field says it sends keystrokes, but no HID forwarding is wired there.
- Android typed character support is limited to simple ASCII and `mods=0`.
- Mobile clipboard UI is local/mock.
- FFI binds the client endpoint to loopback, so current proof is loopback/local,
  not phone-to-computer LAN.
- FFI has no pairing, state observation, clipboard, file, or progress API.

Evidence:

- FFI: `crates/mouser-ffi/src/lib.rs`
- iOS wrapper/screen: `apps/ios/Sources/MouserClient.swift`,
  `apps/ios/Sources/CompanionView.swift`
- Android wrapper/session: `apps/android/app/src/main/kotlin/ai/unlikeother/mouser/companion/MouserClient.kt`,
  `apps/android/app/src/main/kotlin/ai/unlikeother/mouser/companion/CompanionSession.kt`
- Mobile clipboard mock/local comments:
  `apps/ios/Sources/ClipboardModel.swift`,
  `apps/android/app/src/main/kotlin/ai/unlikeother/mouser/companion/ClipboardState.kt`

Priority:

1. Bind mobile FFI to a real outbound address.
2. Add real discovery/connect UI.
3. Add pairing/trust workflow.
4. Wire iOS keyboard field and drag button state.
5. Add mobile clipboard/progress APIs only after daemon clipboard is real.

### 9. Security, Pairing, And Trust

Status: not product-ready.

Implemented:

- Device identity primitives exist.
- TLS verifier can derive device id from certificate SPKI.
- Pinned mode can reject cert/device-id mismatch.

Partial or missing:

- Active daemon path uses Trust On First Use in important paths.
- No persisted trust store.
- No mandatory SAS.
- No channel-bound `Hello`/`HelloAck` typestate before input.
- No local permission gate before allowing injection.
- Direct `connect` does not require expected peer id.

Evidence:

- Pin policy: `crates/mouser-net/src/pin.rs`
- Fresh daemon identity: `crates/mouser-engine/src/bin/mouserd.rs`
- Spec requirement: `docs/communication-interface.md`

Priority:

1. Persist identity and trusted peers.
2. Implement Hello/SAS/channel signature.
3. Block all input/clipboard/file traffic before trust.
4. Require explicit peer identity for direct connect or perform pairing.

### 10. Documentation Consistency

Status: stale and overclaiming in several places.

Examples:

- `README.md` describes clipboard/files and zero-config behavior as product
  capabilities, but they are not end-to-end wired.
- `docs/build-status.md` says shared clipboard is complete end-to-end, which is
  false for daemon/runtime/UI/FFI.
- `docs/build-status.md` also says `mouser-ffi` is a stub, which is stale.
- `docs/windows-build.md` says the daemon runtime is missing, which is stale,
  and references `mouser-engine.exe` rather than current `mouserd.exe`.
- Some mobile comments accurately say mock/local-only, while higher-level docs
  overstate readiness.

Priority:

1. Update README to describe current alpha status.
2. Replace or annotate stale build-status sections.
3. Update Windows run/install docs to match `mouserd`.
4. Add a maintained implementation-status matrix that gates future claims.

## Highest Priority Fix Order

1. Security/trust gate: persistent identity, trusted peers, SAS, permission gate.
2. Daemon safety: Windows active-mode sluggishness root cause, panic/local reclaim,
   capability downgrade, injection failure handling.
3. Daemon lifecycle and IPC: desktop can start/stop/status daemon and reflect real
   peers/settings.
4. Clipboard end-to-end: daemon driver, platform adapter integration, protocol
   dispatch, settings/progress IPC.
5. Bulk/file end-to-end: advertise bulk port, connect bulk endpoint, wire
   `mouser-files`.
6. Mobile LAN readiness: fix loopback bind, add discovery/connect UI, pair before
   control.
7. Docs cleanup: remove claims that are not backed by daemon/runtime/app wiring.

## Plain-English Answer To Current Questions

Is the latest built and running on this Windows machine?

- Yes for the local desktop app and safe `mouserd target` daemon.

Is this machine visible on the LAN?

- Yes in `target` mode, assuming firewall allows the running binary.

Can another machine control this machine?

- Likely yes once a compatible source connects, because the interactive input
  daemon path is wired. It is not yet safely paired or permission-gated.

Can this machine control any other machine at any time?

- Not yet. Active `auto/source` exists, but full any-to-any cluster behavior is
  not implemented, and Windows active mode needs further performance isolation.

Does Ctrl-C share clipboard content?

- No. Ctrl-C may be forwarded as a normal keystroke while controlling another
  machine, but shared clipboard sync is not wired end-to-end.

Are files/drag/drop working across machines?

- No. The engines and protocol pieces exist, but the daemon and apps do not wire
  them into a product path.

Is the desktop tray/taskbar behavior working?

- Yes for the tray shell and the tray-icon/taskbar visibility toggle. Other
  settings are mostly local-only.
