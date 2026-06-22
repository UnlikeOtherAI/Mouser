# Mouser — Build Status

> Current implementation truth table: see
> [`docs/implementation-audit.md`](implementation-audit.md). This file contains
> historical build-wave notes and some older readiness statements are now known
> to be stale or over-broad.

## On `main` (all gated Codex+Claude PASS, workspace build/test/clippy green)
| Crate / app | State |
|-------------|-------|
| `mouser-protocol` | framing, CBOR codec (strict decode, enum→Unknown), wire enums, datagram+golden vectors |
| `mouser-core` | identity (SHA-256 SPKI), ownership-epoch + lease-election state machines, platform traits + `CaptureDecision` |
| `mouser-net` | mDNS, 2-conn QUIC transport, device_id-pinned TLS, keep-alive, cancel-safe recv, keep-newest motion, graceful drain |
| `platform-mac` | CGEvent inject + capture adapter (impl core traits), suppression, FlagsChanged modifiers, full keymap, display_id |
| `platform-linux` | uinput inject adapter (impl core traits), HID→evdev keymap (verified on ai@192.168.1.203) |
| `platform-win` | SendInput `InputInjection` adapter + low-level hook `InputCapture` adapter (monitor routing, relative motion, buttons/keys/scroll) + Win32 clipboard |
| `apps/ios` | SwiftUI: portrait touchpad+keyboard, landscape full trackpad, Mac-parity gestures+haptics, Local-Network entitlement |
| `apps/android` | Kotlin/Compose: same layout + gestures + haptics, 8/8 gesture tests, emulator-verified |
| `apps/desktop` | Tauri v2 + React settings/layout-canvas; Linux CI deps + WebView2 bootstrapper |
| `mouser-files` | §7.8 transfer engine: cumulative-ack window, resume, SHA-256, path-safety/quarantine, sender+receiver+sink |
| `mouser-ffi` | `MobileClient` over uniffi (connect + source engine + pointer/key/scroll); **binds loopback** (R3-H8) so not yet LAN-functional |
| `mouser-engine` | daemon `mouserd` + runtime + core (ownership/input/heartbeat/reclaim, anti-replay) + `daemon_store` (persisted identity+trust) + IPC bridge — single-peer; clipboard/files NOT wired (R3-H7) |
| `mouser-ipc`, `mouser-state` | desktop↔daemon IPC (no access control yet, R3-H3); CRDT cluster state (decode size-cap missing, R3-C1) — both real, see audit-round3 |
| `mouser-testkit` | stub |

**File transfer (merged):** cross-machine drag-and-drop — protocol §7.8 messages
(`FileOffer/Accept/Reject/Chunk/Ack/Done` + `BulkHello`), `mouser-files` engine,
`mouser-net::bulk` second QUIC connection (channel_sig binding), `platform-mac::dragdrop`
NSDraggingSession spike. Workspace build/test/clippy green on `main`.

Reviews: `docs/design-review.md` (Round 1 design), `docs/audit-round1.md` (Round 1 24-agent code audit), `docs/audit-round2.md` (**Round 2** 24-agent paired audit, post file-transfer), `docs/audit-round3.md` (**Round 3** 24-agent paired audit against HEAD `575462a` incl. the engine/ipc/state crates), `docs/implementation-audit.md` (end-to-end wired-vs-scaffold truth).

## Round 2 audit headline (see docs/audit-round2.md)
2 CRITICAL (both "not built yet": the `mouser-engine` runtime, and mobile FFI/network wiring), 8 HIGH, ~28 MEDIUM, ~25 LOW. No memory-unsafety found (platform `unsafe` re-verified sound). Round-1 fixes hold. Top HIGHs: §5 pairing/SAS stubbed, file integrity has no wire digest, resume vs symlink-safe sink unreconciled, mDNS browse drops the peer address, oversize-datagram kills the motion pump, Windows keymap missing the keypad block, mac capture reports display_id=0, Android missing INTERNET permission.

## Round-2 fix wave + shared clipboard — MERGED to main (`4be677a`, gate-fixes included)
Foundation (`51f3ac8`): §7.7 clipboard wire messages + `HelloAck` + `CapabilitySet` forward-compat fix
+ Android network perms + spec (immediate-sync, progress, prefer-native, settings). Then six gated
builder branches, each built in its own worktree and merged after verification (full workspace
build/test/clippy green after every merge; 32 test suites pass):
- `clip-engine` — `mouser-clipboard` pure sync engine (eager auto-pull, progress, prefer-native, loop-prevention; 40 tests).
- `clip-adapters` — platform `Clipboard` adapters (mac NSPasteboard / win Win32+CF_HDROP / linux wl-clipboard).
- `files-harden` — C2-4 in-band SHA-256 (`FileEntry.sha256`), C2-5 resumable symlink-safe `FsSink`, resume-trust, `is_terminal()`/non-fatal gap, Windows-name sanitizer, size/count bounds.
- `net-reliab` — C2-6 dialable discovery (`PeerAdvert.addrs`) + `PeerEvent::{Found,Removed}`, C2-7 motion error-kind fallback signal, cancel-safe control writer task (`control.rs`), bulk graceful drain, type-checked `consume_prime`.
- `plat-parity` — C2-8 Windows keypad block + three-way parity test, C2-9 mac capture per-display coords, FlagsChanged twin fix, Windows scroll `ScrollUnit` + sign-correct packing.
- `election-fix` — `on_yield` exact-term, guarded `start_claim`, no claim-as-lease, no lower-term resurrection, **+ cross-node yield-term fix** (Codex gate caught it pre-merge; fixed + 2 cross-node regression tests).

### Gate (two rounds, all merged — `4be677a`)
`election-fix` got the full Codex+Claude pair pre-merge (Codex found a real cross-node yield-term bug → fixed → re-verified). The other five merged on orchestrator integration-verification, then a **Codex post-merge second-opinion** FAILed all five with concrete follow-ups (the gate's value): files `on_ack` trusted impossible acks (HIGH), clip-engine `on_data` skipped receipt gates + no same-origin offer supersession, Windows `Html` lacked CF_HTML + empty-write garbage byte, mac capture `expect()` on poisoned locks, missing motion-fallback test. Four **fix branches** (`net-test`, `clip-engine-fix`, `files-fix`, `platform-fix`) addressed every verified finding with mutation-discriminated tests, were orchestrator-verified, and merged. A final Codex confirmation pass is running (`/tmp/mouser-gate2`). Net result: all Round-2 audit findings that don't require the engine are fixed; the shared clipboard core + platform adapters are in.

## Clipboard UI + mobile R2 wave — MERGED to main (`70d5ebd`)
- `clip-ui` — desktop Clipboard settings section (master/per-format/max-size/prefer-native/direction) + Mac-style wait/progress indicator (pnpm build+lint green; Playwright-CLI screenshots).
- `ios-fixes` — portrait keyboard-below layout, scenePhase lifecycle/reconnect hooks, drag double-motion fix, momentum `deinit`, clipboard settings + wait-indicator views (xcodebuild iOS-sim **BUILD SUCCEEDED**).
- `android-fixes` — `DefaultLifecycleObserver`/`LifecycleEventEffect`, monotonic gesture clock, dropped `material-icons-extended` + R8 on release, clipboard settings + progress composables (gradle assembleDebug/Release + unit tests pass).

Hygiene merged (`559225c`): workspace-wide panic-free clippy denies (`[workspace.lints.clippy]` + per-crate adoption + `clippy.toml` test exemptions; 6 Windows indexing lints + Linux mutex poison-recovery fixed), `libc` removed, dragdrop SAFETY notes. All Round-2 *protocol/engine/adapter/UI-layer* findings are fixed.

> **CORRECTION (Round 3 / `docs/implementation-audit.md`):** an earlier version of this file claimed "the shared clipboard is complete end-to-end" — that was **false**. The clipboard engine + platform adapters + settings UI exist, but they are **not wired through `mouserd`/runtime/IPC/FFI** (R3-H7), so clipboard does not sync between machines yet. Treat `docs/implementation-audit.md` + `docs/audit-round3.md` as the authoritative status.

## Round 3 audit — DONE (`docs/audit-round3.md`, against HEAD `575462a`)
24-agent paired re-run against the **real current tree** (now incl. `mouser-engine` daemon, `mouser-ipc`, `mouser-state`). **1 CRITICAL** (CRDT decodes wire bytes with no size cap → decompression-bomb OOM), **11 HIGH**, ~26 MEDIUM. Top HIGHs: no SAS/channel-bound pairing + trust=full-permission, IPC has no same-user access control, macOS event-tap can unwind through C (no `catch_unwind`), engine task-supervision/ACK/inject gaps, CRDT divergence wedges, clipboard/files unwired, mobile FFI binds loopback, no reconnect supervisor. The audit also found `implementation-audit.md` is itself partially stale (identity/trust now persist; IPC/lifecycle/mobile-discovery are wired) — see its "Audit reconciliation" section.

## Queued — Round 3 fix priorities (reconciled)
1. **Security gate**: SAS + `Hello`/`channel_sig` typestate before input; permission/rate gate in core; revocation (R3-H1/H2).
2. **IPC access control** (chmod/peer-uid/pipe-DACL, R3-H3) + **macOS tap `catch_unwind`** (R3-H4) — small, high-impact safety fixes.
3. **CRDT hardening**: size-cap before decode, per-change apply, causal `layout_rev` (R3-C1/H6) — before `mouser-state` is wired.
4. **Engine supervision**: connection-death propagation, ACK timeout/snap-back, inject-failure downgrade, **reconnect supervisor** (R3-H5).
5. **Mobile loopback bind** — one-line, unblocks the phone path (R3-H8).
6. **Wire clipboard/files through the daemon**: bulk port + drivers (R3-H7).
7. Capture `Drop`/`can_suppress`/panic-hotkey (R3-H10); transport anti-amplification + motion fallback (R3-H11); MEDIUM/LOW cleanups; refresh stale docs (`implementation-audit.md` §1/§7/§8/§9, `windows-build.md` `mouserd.exe`).

## Infra
rustup 1.96 + ios targets; Xcode 26.3 + iPhone 17 Pro sim; Android SDK + AVD; Linux box `ai@192.168.1.203` (uinput). Per-task gate = Codex+Claude pair; parallel worktrees under `.worktrees/`.
