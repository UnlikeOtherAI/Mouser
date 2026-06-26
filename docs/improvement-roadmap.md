# Mouser Improvement Roadmap: Reference Architecture Gap Closure

## Executive Summary

Measured against `docs/reference-software-kvm-architecture.md`, Mouser is already a strong, in places best-in-class, implementation of the reference's core synthesis. The transport plane is essentially textbook (QUIC streams + RFC 9221 DATAGRAMs, keep-newest motion coalescing, mandatory TLS 1.3 with cert-pinning, Ed25519 channel binding and SAS that exceed both Deskflow and Lan Mouse), the ownership state machine's epoch/anti-replay/simultaneous-reclaim tiebreak is more rigorous than anything the reference describes, the latency policy (no jitter buffer, no smoothing, thin Windows hooks, absolute-per-packet motion) is exactly right, and the liveness-recovery paths (heartbeat timeout, suppress-unavailable, injection-failure) are fail-safe. The biggest gaps cluster in three places the reference treats as central: (1) **no synthetic-event tagging** at the OS layer on either platform, leaving macOS loop-prevention dependent on a fragile "source must only warp" rule we already violated once (the bogus-delta recapture bug); (2) **no DPI/resolution/scaling normalization** between heterogeneous displays, so the 2560x1440 Mac vs 3840x2160 Windows case produces a reproducible mid-screen cursor jump on crossing; and (3) **no display-change resync path** — peer geometry is frozen at session start, `layout_rev` is hardcoded to 0, and a mid-session resolution change silently corrupts the mapping. macOS additionally never disassociates the mouse from the cursor, so the source pointer drifts during remote control and is patched with a warp-back hack instead of being fixed at the cause.

## Verdict by Dimension

| Dimension | Alignment | Headline gap |
|---|---|---|
| Topology & geometry | Weak | Off-axis carry-over copies a raw pixel instead of a preserved fraction; no canonical logical space / DPI normalization |
| Ownership state machine | Strong | Phases are implicit across ~5 booleans; no named state machine; no `Resync` state |
| Event tagging / loop prevention | Partial (Win) / Weak (Mac) | No OS-level synthetic tag anywhere; macOS relies on a fragile warp-only invariant |
| Windows capture/inject | Strong | UIPI/secure-desktop silent-swallow undetected; overlay shown unconditionally (double cursor) |
| macOS capture/inject | Partial | `CGAssociateMouseAndMouseCursorPosition` unused (drift); per-packet warp triggers suppression interval; hide path is a no-op |
| Transport | Strong | Only the dead datagram-unavailable fallback reliably orders motion (HOL risk) |
| Recovery & resync | Partial | No display-change detection; no screen-info/ack resync handshake; `layout_rev` dead |
| Latency & jitter | Strong | Runtime merges control+motion into one FIFO before the split QUIC planes (HOL jitter) |

Recurring themes that merge across dimensions: **synthetic-event tagging** (event-tagging, macOS, Windows), **DPI/scaling-aware geometry** (topology, macOS, recovery, latency), and **display-change resync** (ownership, recovery, topology).

---

## P0 — None

No finding rises to data-loss / safety-critical. The highest-impact items are P1 user-visible correctness gaps below. (Reclaim safety, the one true "never both own the mouse" invariant, is already enforced structurally by the epoch token.)

---

## P1 — User-visible correctness gaps to fix first

### P1.1 Tag synthetic events at the OS layer (macOS first, Windows as hardening)
**Why it matters.** The reference's central loop-prevention mechanism is to mark every injected event (macOS `eventSourceUserData`, Windows `MOUSEINPUT.dwExtraInfo`) so the source tap can tell its own injection from genuine local input. On macOS we do *zero* tagging: `inject.rs` posts `MouseMoved`/button/key/scroll CGEvents with no `EVENT_SOURCE_USER_DATA`, and the tap (`adapter.rs` `make_callback`, `keymap_capture.rs` `to_local_event`) never checks for one. Loop prevention rests entirely on the invariant "the source only ever warps, never posts" — the exact invariant whose violation caused the bogus-delta recapture loop we just fixed. `move_cursor` (`crates/platform-mac/src/inject.rs:122-132`) both warps *and* posts, so any future mis-routed call re-creates the loop.
**Change.**
- macOS: create the injection `CGEventSource` and set `ev.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, MOUSER_TAG)` on every posted CGEvent in `crates/platform-mac/src/inject.rs`; in `to_local_event`/`make_callback` drop any event whose user-data matches the sentinel (`core-graphics 0.25` already exposes `EVENT_SOURCE_USER_DATA`). This makes self-recapture impossible by construction and frees the source to post events instead of being warp-only.
- Windows (hardening): replace `dwExtraInfo: 0` on all five `SendInput` paths (`crates/platform-win/src/inject.rs:196,268,286,373,384,417`) with a magic `ULONG_PTR`, and check it alongside `LLMHF_INJECTED` in the hooks (`capture_hooks.rs:179,275,142,244`). Windows already prevents self-recapture via `LLMHF_INJECTED`, so this is defense-in-depth / third-party-injector disambiguation, not a fix.
**Platform.** macOS (P1), Windows (P2). **Effort.** M (Mac) / S (Win).

### P1.2 Preserve the fractional edge position on crossing (off-axis carry-over)
**Why it matters.** The reference's Mouse-Ether guidance: store the *fraction* along the exit edge and map it to the destination edge, so "a crossing at 37% down the right edge enters at 37% down the mapped edge." `cross_to_peer` instead copies a raw source pixel: `self.peer_y = clamp(y, 0, max_y)` where `y` is a source pixel and `max_y` is `peer_height-1` (`crates/mouser-engine/src/core.rs:463-482`), for all four edges. Concrete symptom in the brief's exact case: a Mac (1440 tall) crossing at `y=1400` (97%) seeds `peer_y=1400` on a 2160-tall Windows screen = 65% down — the cursor visibly jumps to mid-screen; the reverse collapses the Windows screen's bottom third onto the Mac's corner.
**Change.** In `cross_to_peer`, compute `frac = off_axis / (source_dim-1).max(1)` then `peer_off = round(frac * (peer_dim-1))` for the carried axis (y for Left/Right, x for Top/Bottom). Mirror it on cross-back: `entry_edge_warp` (`core.rs:386-400`) currently snaps to the *drifted* local x/y; derive the off-axis from the peer fraction (`off_local = round(peer_off/(peer_dim-1) * (local_dim-1))`) so return is fraction-correct too.
**Platform.** engine. **Effort.** S. *(Small, well-contained, and the single highest value-per-line change here.)*

### P1.3 Kill macOS source-cursor drift at the cause (mouse/cursor disassociation)
**Why it matters.** The reference names `CGAssociateMouseAndMouseCursorPosition` as the primitive to "disconnect the mouse and cursor" during remote ownership. It is never called anywhere. Because the OS keeps moving the hidden local pointer while we control the peer, the source Mac cursor drifts, and we treat the *symptom* with an event-free warp-back edge-snap (`crates/platform-mac/src/injector.rs:62-69`) rather than the cause. Compounding this: the engine emits `SetCursorVisible(false)` when it becomes non-owner (`core.rs:178,496`, `core/wire.rs:236`), but the Mac adapter silently drops the hide — `MacInjector::set_cursor_visible` early-returns `Ok(())` for `visible=false` (`crates/platform-mac/src/injector.rs:89-94`) — so the source cursor stays visible *and* drifts.
**Change.** Add an `extern "C"` decl for `CGAssociateMouseAndMouseCursorPosition` (as already done for `CGEventTapEnable` at `adapter.rs:509-511`) and call `(false)` on entering `ActiveForward`/becoming remote owner, `(true)` on reclaim. Wire the hide path through to `inject::set_cursor_visible(false)` (which already handles both directions, `inject.rs:260-281`). Verify on real hardware that the foreground-only API takes effect for a tray app; if confirmed, the cross-back warp can be simplified or dropped.
**Platform.** macOS. **Effort.** M.

### P1.4 Stop per-packet warp on the macOS target (post-warp suppression interval)
**Why it matters.** Each incoming absolute motion packet on the target runs `move_cursor`, which does *both* a `CGWarpMouseCursorPosition` and a posted `MouseMoved` on every call (`crates/platform-mac/src/inject.rs:122-132`, dispatched at `crates/mouser-engine/src/runtime.rs:191`). `CGWarp` starts the well-known ~0.25s interval during which the window server suppresses/interpolates subsequently posted mouse events unless `CGAssociateMouseAndMouseCursorPosition(true)` is called — so warping at motion frequency actively degrades the very injected motion. The warp is redundant on the target anyway (Accessibility injection already moves the cursor via the posted event).
**Change.** On the target absolute-injection path, post `MouseMoved` *without* the per-packet warp; reserve event-free `warp_cursor` for the park/handoff path only. If a warp is ever required on the target, immediately follow it with `CGAssociateMouseAndMouseCursorPosition(true)`.
**Platform.** macOS. **Effort.** M.

### P1.5 Detect display changes and add an absolute-resync handshake
**Why it matters.** The reference singles out the Barrier pattern as "worth copying": on a receiver resolution/layout change, report new screen info and *ignore movement until acknowledged*, preventing off-screen motion into stale geometry. We implement none of it. There is no display-reconfiguration callback on any platform (no `CGDisplayRegisterReconfigurationCallback`, no `WM_DISPLAYCHANGE`/`WM_DPICHANGED`), `peer_width/peer_height` are read once from the advert at `crates/mouser-engine/src/daemon/serve_session.rs:64-69` and frozen into `EngineCore`, `on_motion` injects unconditionally (`core/wire.rs:197-211`), and `OwnershipTransfer.layout_rev` is hardcoded `0` (`core.rs:488,518`) and never read. A 1440p Mac receiver dropping to 1080p mid-session leaves the source clamping to 2560x1440, so the lower-right band maps off-screen and `crosses_back` math (`core.rs:429-448`) runs against the wrong bounds, degrading reclaim.
**Change.** (a) Per-platform display-reconfiguration hook that fires an engine event. (b) A reliable `ScreenInfo`/`Resync` control message carrying new `(width,height,scale)`; the receiver parks injected motion until the source ACKs. (c) Give `EngineCore` a `set_layout()`/`on_peer_geometry()` entry point that rebuilds the live `EdgeLayout`, bumps a real `layout_rev`, and re-emits one absolute coordinate through the existing post-ACK absolute path (`core/wire.rs:110-118`). (d) Re-publish the local advert (`crates/mouser-net/src/discovery.rs`) on change. Until landed, document that mid-session display changes require reconnect.
**Platform.** cross-platform. **Effort.** L.

### P1.6 Detect Windows UIPI / secure-desktop silent-swallow
**Why it matters.** The reference warns `SendInput` is subject to UIPI: injection into higher-integrity windows, the UAC secure desktop, and the lock screen silently fails while `SendInput` still returns a full count. The inject module documents that the adapter "must surface this as `CapState::SecureContext`/`BlockedReason::SecureDesktop`" (`crates/platform-win/src/inject.rs:22-32`), but `send()` only errors on a *short* count (`inject.rs:440-453`) — a fully-swallowed inject returns `Ok(())`. Result: control can be handed to a window where nothing lands, with no reclaim.
**Change.** Add a lightweight post-inject sanity probe (read `GetCursorPos` after an absolute move and compare, or check foreground-window token integrity vs ours) and surface `BlockedReason::SecureDesktop`. At minimum, confirm the daemon layer already drives this so it is not an unhandled silent-failure path. (macOS has the analogous `IsSecureEventInputEnabled` gap for keystrokes — P2 below.)
**Platform.** Windows. **Effort.** M.

---

## P2 — Robustness, fidelity, and structural cleanup

### P2.1 Introduce a canonical logical coordinate space with scale/DPI
**Why it matters.** Beyond the fractional seed (P1.2), there is no canonical logical space and no scale conversion at all. `EdgeLayout` holds raw width/height/peer_width/peer_height with no scale factor (`crates/mouser-engine/src/core/types.rs:35-42`); the advert carries only `dw/dh` pixels with no `backingScaleFactor`/DPI (`crates/mouser-net/src/discovery.rs:130-133`); and the units are inconsistent (macOS `active_display_bounds` returns logical points while Windows `rcMonitor` meaning depends on process DPI-awareness — no `SetProcessDpiAwareness` exists in `platform-win`). `mouser-state` already has a `scale_milli` field (`mouser-state/src/model.rs:33-34`) the input engine ignores.
**Change.** Advertise a scale/DPI alongside `dw/dh`; map both the absolute seed and ongoing snapshots through source→canonical→peer transforms, converting to peer device pixels only at the injection boundary. Keep macOS's existing logical-point boundary conversion (`platform-mac/src/inject.rs:1-7,119-121`). This is the full-fidelity backstop to P1.2; re-apply it as part of the P1.5 resync.
**Platform.** cross-platform. **Effort.** L.

### P2.2 Gate the Windows software cursor overlay on `GetCursorInfo`
**Why it matters.** The overlay exists for apps that show *no* native cursor (`hCursor==NULL`), per its own module doc — but `WinInjector::move_cursor`/`move_cursor_relative` call `cursor_overlay::show_at` on *every* injected move unconditionally (`crates/platform-win/src/adapter.rs:292-296,302-305`), and `GetCursorInfo` is only referenced in a comment (`cursor_overlay.rs:3-4`), never called. Since the absolute `SendInput` also moves the real OS cursor, any app showing a native pointer gets *two* cursors during remote control.
**Change.** Call `GetCursorInfo` and only `show_at` when `hCursor==NULL` or `CURSOR_SHOWING` is clear; hide otherwise.
**Platform.** Windows. **Effort.** M.

### P2.3 Decouple motion from control in the runtime queue
**Why it matters.** The QUIC planes are correctly split (reliable stream vs DATAGRAM), but the runtime re-serializes them: both `Action::SendMotion` and `Action::SendControl` go through one unbounded `out_tx` drained FIFO by a single task (`crates/mouser-engine/src/runtime.rs:104-105,251-287`). `Outgoing::Control` awaits a full reliable flush (`transport.rs:401-403`) while motion sits behind it — so a click/scroll during a drag, on a Wi-Fi stall, head-of-line-blocks motion by the retransmit window, re-merging exactly what the transport split apart.
**Change.** Route motion on its own channel/sender task (or move `Action::SendMotion` onto a path that never waits behind `send_control`). Cheap, fully decouples the planes end-to-end. The dead datagram-unavailable fallback (`motion.rs:59-89`, `runtime.rs:265-285`) should likewise either apply keep-newest coalescing or simply suppress motion when datagrams are unavailable.
**Platform.** engine. **Effort.** M.

### P2.4 Introduce an explicit named `TransferPhase` enum
**Why it matters.** The reference's value is its *named* state machine (`LocalActive`/`CrossingPending`/`RemoteActive`/`ReturnPending`/`Resync`). We have none; the phase is an emergent product of ~5 booleans (`pending_ack`, `reclaim_armed`, `cross_out_armed`, `suppress_blocked`, `escape_travel`) plus an orthogonal per-device `FocusKind`. Invariants like "`pending_ack` implies not owner" are unstated and enforced only by construction — a maintainability/auditability gap, not a live bug.
**Change.** Add a derived `enum TransferPhase { LocalActive, CrossingPending, RemoteActive, ReturnPending, Resync }` in `EngineCore` for logging/assertions, mapping `pending_ack`→`CrossingPending`, peer-owned→`RemoteActive`, etc. Route the recovery transitions (heartbeat-timeout, suppress-unavailable, injection-failed, goodbye) through it for uniform "why did ownership move" observability. Keep `owner_epoch` as the single source of truth — do not let the enum drift from it. Model the brief self-mint→peer-processes-`LocalReclaim` window as `ReturnPending` for observability only; do **not** gate the local cursor on a return ack (immediate authoritative reclaim is correct).
**Platform.** engine. **Effort.** M.

### P2.5 macOS Secure Event Input detection
**Why it matters.** The capture module docstring promises that when an app enables Secure Event Input, the adapter surfaces `BlockedReason::SecureInputField` and returns ownership (`crates/platform-mac/src/capture.rs:22-26`; the protocol enum exists at `crates/mouser-protocol/src/enums.rs:102`). No `IsSecureEventInputEnabled()` call exists — forwarded keystrokes silently vanish in password fields. Lower severity for a mouse-only KVM (only keystrokes are withheld).
**Change.** Add an `extern` decl for `IsSecureEventInputEnabled()`, poll on the `ActiveForward`/key-injection-failure path, surface `BlockedReason::SecureInputField`, return ownership. If keyboard is out of scope, downgrade the docstring claim instead.
**Platform.** macOS. **Effort.** M.

### P2.6 Off-axis offset for misaligned displays
**Why it matters.** `EdgeLayout` is a single flat edge with no display origin/offset, so a Mac whose top doesn't align with the Windows top still maps full-edge-to-full-edge (`core/types.rs:16-42`). The reference models displays as rectangles in canonical space with directed edge-segment links. Full topology is a later milestone, but the offset case is real now.
**Change.** Add a source/peer off-axis origin term to the flat model so vertically misaligned displays preserve continuity; defer rectangle/segment/directed-graph topology. Document the limitation in `docs/architecture.md`.
**Platform.** engine. **Effort.** L.

### P2.7 Lower-priority hardening (batch)
- **GetRawInputBuffer batching** for 1000Hz mice instead of per-message `GetRawInputData` (`crates/platform-win/src/capture_rawinput.rs:196-204`); measure first. *(Win, M.)*
- **Receiver newest-wins datagram drain**: drain all available datagrams and inject only the newest absolute (payload is full state) to remove intra-burst rubber-band (`runtime.rs:337-341`). *(cross, S.)*
- **Unscaled relative deltas** across densities cause a "feel" change after a cross; optional delta scaling by `peer_density/source_density` after P2.1, or document as acceptable. *(engine, M.)*

---

## P3 — Already aligned, keep as-is (do not regress)

- **Absolute-seed-once + relative-continuous handoff** model (`core.rs:450-503,281-333`) is exactly the reference synthesis; absolute-per-packet snapshots match "motion is state, not a log" and self-heal drift/loss.
- **Epoch token, anti-replay, simultaneous-reclaim tiebreak, `InputAuth`** (`mouser-core/src/ownership.rs:77-82,190-233`; `core/types.rs:179-255`) exceed the reference; immediate authoritative reclaim is the correct safety path.
- **Transport**: QUIC streams+DATAGRAM, keep-newest mailbox, no IP fragmentation, QUIC congestion control, TLS 1.3 + pinning + Ed25519 + SAS, 5s keep-alive (`transport.rs`, `motion.rs`, `tls.rs`, `handshake.rs`).
- **Latency**: no jitter buffer, no smoothing, thin Windows hooks with worker offload (`capture_hooks.rs`); benchmark methodology is sound.
- **Windows**: Raw Input + `RIDEV_INPUTSINK` relative deltas, crash-safe `SetCursorPos` parking (deliberately rejecting `ClipCursor`), `ShowCursor` counter re-assert loop, `MOUSEEVENTF_ABSOLUTE|VIRTUALDESK` normalization — all correct, several better-than-reference.
- **macOS**: `CGEventTap` observe+suppress with listen-only fallback and `TapDisabled` re-arm, dual Accessibility + Input-Monitoring permissions, event-free `CGWarp` for parking.
- **KVM-appropriate divergence**: the literal "any untagged local input = immediate reclaim" short-circuit is intentionally *not* adopted (the local user driving the peer *is* expected input); `on_suppress_unavailable` (`core.rs:537-550`) + the emergency double-Ctrl chord are the correct substitutes. Document this so it isn't mistaken for a missing feature.

---

## Sequenced Plan

Smallest changes that remove the most user-visible pain first:

1. **P1.2 fractional edge seed (engine, S).** One self-contained change in `cross_to_peer` + `entry_edge_warp` that directly fixes the reproducible 1440p↔2160p mid-screen cursor jump — the most visible defect, lowest cost. Land first.
2. **P1.1 macOS synthetic-event tagging (mac, M).** Removes the structural fragility behind the bogus-delta recapture loop and unblocks safely posting events on the source; turns a hand-maintained "never post on source" rule into a by-construction guarantee. Foundational for clean reclaim.
3. **P1.3 + P1.4 macOS cursor disassociation and target warp removal (mac, M+M).** Kills source-cursor drift at the cause (retiring the warp-back hack) and stops the per-packet warp degrading injected motion. Do together — both hinge on `CGAssociateMouseAndMouseCursorPosition` and the inject path; verify on real hardware.
4. **P1.6 Windows UIPI detection + P2.2 overlay gating (win, M+M).** Two contained Windows fixes: stop silently handing control to secure-desktop windows, and stop the double-cursor artifact.
5. **P1.5 display-change resync + P2.1 canonical scaled geometry (cross, L+L).** The largest body of work; build the resync handshake and the canonical logical space together since the resync must re-apply scaling. This closes the last "stale geometry corrupts the mapping" gap and is the natural home for the now-dead `layout_rev`.
6. **P2.3 motion/control decoupling, P2.4 named phases, P2.5/P2.6/P2.7 hardening.** Structural and robustness polish once the user-visible correctness gaps are closed.

Rationale: steps 1–3 are small/medium and remove the three artifacts users actually feel (the jump, the drift, recapture risk); steps 4–5 close the remaining correctness holes; step 6 is durability and legibility that does not change observable behavior.
