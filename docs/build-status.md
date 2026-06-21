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

Reviews: `docs/design-review.md` (Round 1 design), `docs/audit-round1.md` (Round 1 24-agent code audit), `docs/audit-round2.md` (**Round 2** 24-agent paired audit, post file-transfer — 12 Opus + 12 Codex, every finding orchestrator-verified).

## Round 2 audit headline (see docs/audit-round2.md)
2 CRITICAL (both "not built yet": the `mouser-engine` runtime, and mobile FFI/network wiring), 8 HIGH, ~28 MEDIUM, ~25 LOW. No memory-unsafety found (platform `unsafe` re-verified sound). Round-1 fixes hold. Top HIGHs: §5 pairing/SAS stubbed, file integrity has no wire digest, resume vs symlink-safe sink unreconciled, mDNS browse drops the peer address, oversize-datagram kills the motion pump, Windows keymap missing the keypad block, mac capture reports display_id=0, Android missing INTERNET permission.

## In flight — Round-2 fix wave + shared clipboard (6 gated builder worktrees)
Foundation merged to main (`51f3ac8`): §7.7 clipboard wire messages + `HelloAck` + `CapabilitySet`
forward-compat fix + Android network perms + spec (immediate-sync, progress, prefer-native, settings).
Six parallel builders (each Codex+Claude-gated, merged on PASS):
- `feat/clip-engine` — `mouser-clipboard` pure sync engine (eager auto-pull, progress, prefer-native, loop-prevention).
- `feat/clip-adapters` — platform `Clipboard` adapters (mac NSPasteboard / win Win32 / linux).
- `feat/files-harden` — C2-4 wire digest, C2-5 resumable symlink-safe disk sink, resume-trust, terminal/gap, path safety.
- `feat/net-reliab` — C2-6 dialable discovery + removals, C2-7 motion error-kind fallback, cancel-safe control send, bulk drain.
- `feat/plat-parity` — C2-8 Windows keypad + 3-way parity test, C2-9 mac capture per-display coords, FlagsChanged twin, Windows scroll unit.
- `feat/election-fix` — election yield/claim/term edge cases + ownership reclaim tests.

## Queued
0. Gate (Codex+Claude) + merge the 6 in-flight branches; then **clipboard UI wave**: desktop settings toggles + Mac-style progress/"wait" indicator (apps/desktop), mobile clipboard hooks + native-preference + progress.
1. **Mobile fixes** (C2-2 FFI/net wiring, iOS keyboard-below layout, lifecycle/reconnect, iOS double-motion, momentum deinit) + **hygiene** (workspace-wide panic-free clippy lints, `libc` dep, SAFETY comments).
2. **Re-run Round 3 audit** on the whole codebase after the fix+clipboard waves land (per request).
3. **Wave 2 — `mouser-engine` + `mouser-ipc`**: the runtime (heartbeat, auto-reconnect supervisor, receive-side auth + anti-replay, ack-timeout cursor-recovery, §5 pairing/SAS, bulk/StateSnapshot, Goodbye-on-sleep) — audit C2-1/C2-3, the #1 gap.
2. **File-transfer hardening** (C2-4/C2-5): wire digest, symlink-safe positioned-write resumable disk sink, sender resume-trust + receiver terminal/forward-gap fixes, Windows-name sanitizer.
3. **Discovery + motion** (C2-6/C2-7): resolved address + removals; motion error-kind handling with control-stream fallback.
4. **Platform parity** (C2-8/C2-9): Windows keypad + three-way parity test; mac capture per-display coords; Windows/Linux capture + Windows `InputInjection`.
5. **Mobile wiring** (C2-2) + Android `INTERNET` + iOS keyboard-below + lifecycle/reconnect.
6. `mouser-testkit` (fake clock/transport + N-engine harness) + `cargo-fuzz`; workspace-wide panic-free lints; cancel-safe `send_control`; election edge cases; LOW cleanups.

## Infra
rustup 1.96 + ios targets; Xcode 26.3 + iPhone 17 Pro sim; Android SDK + AVD; Linux box `ai@192.168.1.203` (uinput). Per-task gate = Codex+Claude pair; parallel worktrees under `.worktrees/`.
