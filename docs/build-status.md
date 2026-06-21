# Mouser — Build Status

Updated by the orchestrator each cycle. Source of truth for what's built, gated, and merged.

## Spec
- v2.5, gated through a 10-agent design review + ~5 Codex/Claude gate rounds. Build-ready (proven by working code).

## Components

| Component | State | Gate (Codex / Claude) | Real-device acceptance |
|-----------|-------|------------------------|------------------------|
| `mouser-protocol` | **merged** | PASS / PASS | golden vectors green (8 tests) |
| `mouser-net` (mDNS + quinn + pinned TLS + datagram) | **merged** | PASS / PASS | loopback: ALPN+pin+Ping+datagram (5) |
| `platform-linux` (uinput) | **merged** | PASS / PASS | **virtual device created on ai@192.168.1.203**, event readback REL_X/Y + key |
| `platform-mac` (CGEvent inject) | **merged** | PASS / PASS(after fmt) | **cursor moved on macOS** (328,465→216,864) |
| `mouser-core` (identity, ownership epoch, lease election) | in flight (fix) | PASS / FAIL(2 items→fixing) | 36 unit tests |
| iOS companion (SwiftUI portrait: touchpad↑ / keyboard↓) | in flight | — | target: screenshot in iPhone 17 Pro sim |
| Tauri desktop UI (React/TS settings+layout canvas) | in flight | — | — |
| `platform-win` skeleton + docs/windows-build.md | in flight | — | deferred to a Windows machine |
| net hardening (neg-pin test, panic-free lint, spec postcard wording) | in flight | — | — |

## Queued (after `mouser-core` merges)
- `mouser-testkit` (fake clock/transport/adapters + 3-engine harness)
- Wire platform-mac/linux to `mouser-core` traits
- `mouser-net` + `mouser-core` integration: real Hello / SAS pairing / channel_sig
- `mouser-engine` daemon; `mouser-ipc`; `mouser-ffi` → wire iOS companion
- macOS Accessibility/Input-Monitoring onboarding for live capture acceptance

## Infrastructure
- Toolchain: rustup 1.96 + ios/ios-sim targets; Xcode 26.3; iPhone 17 Pro sim; pnpm.
- Linux test box: `ai@192.168.1.203` (Ubuntu 26.04, headless, uinput works via sudo; prod needs `input` group + udev rule).
- Parallel build = git worktrees under `.worktrees/`, each gated by a Codex+Claude pair, merged on PASS.

## Blockers
- None currently. (`/dev/uinput` on the Linux box is root-only — needs a udev rule for non-root prod use.)
