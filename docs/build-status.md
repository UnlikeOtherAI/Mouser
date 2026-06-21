# Mouser — Build Status

## On `main` (all gated Codex+Claude PASS, workspace build/test/clippy green)
| Crate / app | State |
|-------------|-------|
| `mouser-protocol` | framing, CBOR codec (strict decode, enum→Unknown), wire enums, datagram+golden vectors |
| `mouser-core` | identity (SHA-256 SPKI), ownership-epoch + lease-election state machines, platform traits + `CaptureDecision` |
| `mouser-net` | mDNS, 2-conn QUIC transport, device_id-pinned TLS, keep-alive, cancel-safe recv, keep-newest motion, graceful drain |
| `platform-mac` | CGEvent inject + capture adapter (impl core traits), suppression, FlagsChanged modifiers, full keymap, display_id |
| `platform-linux` | uinput inject adapter (impl core traits), HID→evdev keymap (verified on ai@192.168.1.203) |
| `platform-win` | SendInput skeleton (cfg-stub off-Windows) + docs/windows-build.md |
| `apps/ios` | SwiftUI: portrait touchpad+keyboard, landscape full trackpad, Mac-parity gestures+haptics, Local-Network entitlement |
| `apps/android` | Kotlin/Compose: same layout + gestures + haptics, 8/8 gesture tests, emulator-verified |
| `apps/desktop` | Tauri v2 + React settings/layout-canvas; Linux CI deps + WebView2 bootstrapper |
| `mouser-files` | §7.8 transfer engine: cumulative-ack window, resume, SHA-256, path-safety/quarantine, sender+receiver+sink |
| `mouser-ffi`, `mouser-testkit` | stubs |

**File transfer (merged):** cross-machine drag-and-drop — protocol §7.8 messages
(`FileOffer/Accept/Reject/Chunk/Ack/Done` + `BulkHello`), `mouser-files` engine,
`mouser-net::bulk` second QUIC connection (channel_sig binding), `platform-mac::dragdrop`
NSDraggingSession spike. Workspace build/test/clippy green on `main`.

Reviews: `docs/design-review.md` (Round 1 design), `docs/audit-round1.md` (24-agent code audit).

## Queued
1. **Re-run the full 24-agent paired audit on the whole codebase** (per request, post file-transfer) — IN PROGRESS.
2. **Wave 2 — `mouser-engine` + `mouser-ipc`**: the runtime (heartbeat, auto-reconnect supervisor, receive-side auth + anti-replay, ack-timeout cursor-recovery, bulk/StateSnapshot, Goodbye-on-sleep) — audit A1, the #1 gap.
3. `mouser-testkit` (fake clock/transport + N-engine harness) + `cargo-fuzz` targets.
4. Remaining audit MEDIUM/LOW cleanups.

## Infra
rustup 1.96 + ios targets; Xcode 26.3 + iPhone 17 Pro sim; Android SDK + AVD; Linux box `ai@192.168.1.203` (uinput). Per-task gate = Codex+Claude pair; parallel worktrees under `.worktrees/`.
