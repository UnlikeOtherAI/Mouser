# Mouser — Round 2 Code Audit (24-agent paired, post file-transfer)

Re-run of the full paired audit on the **whole codebase** after the cross-machine
drag-and-drop file-transfer feature merged (`ff54d26`). Method mirrors Round 1.

## Method
- **24 reviewers across 12 topics**, two independent opinions per topic: one **Claude
  Opus 4.8** subagent + one **Codex** review each. Topics: protocol/wire, crypto/identity,
  transport/reliability, concurrency/async, state machines, file-transfer engine,
  unsafe/memory, panic-freedom, input parity, mobile apps, API/naming, reliability/reconnect.
- Every reviewer read real code and cited `file:line`. Per project policy, **each
  accepted finding has ≥1 independent verification step** by the orchestrator (re-read of
  the cited code). Convergent findings (both reviewers) are flagged; convergence is
  treated as signal, not proof — each was still verified.
- Raw per-agent reports were synthesized and de-duplicated here; one Codex claim was
  **rejected on verification** (see end).

## Severity summary (synthesized, orchestrator-graded)
| Severity | Count | Theme |
|----------|-------|-------|
| CRITICAL | 2 | The runtime engine and the mobile wire-wiring do not exist yet |
| HIGH | 8 | Pairing boundary, file integrity/resume, discovery dialing, motion fallback, Windows keypad, mac capture coords, Android INTERNET perm |
| MEDIUM | ~28 | Cancel-safety (send/bulk), election edge cases, file-transfer robustness, platform parity, protocol strictness |
| LOW | ~25 | SAFETY comments, dead deps, naming, zeroize, over-exports, doc drift |

Grades are the orchestrator's after verification; where the two reviewers disagreed the
chosen grade and rationale are noted. Round-1 fixes were re-checked and held (see bottom).

---

## CRITICAL

### C2-1 — No runtime engine: liveness, reconnect, auth, anti-replay, cursor-recovery all absent
- **where:** MISSING — `crates/mouser-engine`, `crates/mouser-ipc` do not exist (`Cargo.toml` members). The only callers of `recv_control`/`recv_motion`/`next_peer` and of the `Ownership`/`Election` machines are tests.
- **verified:** ✓ orchestrator (no engine crate; state machines have zero production callers). Convergent: T12 + Codex-12 + carries Round-1 A1.
- **impact:** sleep/wake, Wi-Fi drop, peer crash, coordinator loss, mid-session shutdown are all unsurvivable because nothing observes or reacts. This is the umbrella gap; the following are its concrete sub-requirements, each independently verified as absent:
  - **Heartbeat/`Pong`** (spec §7.1, 1 s, dead after 3 misses) — only QUIC keep-alive (5 s/20 s idle) exists; no app-layer liveness, so a wedged-but-alive peer is never detected and owner-unreachable cursor reclaim never fires.
  - **Auto-reconnect supervisor** with backoff — none; `Browser::next_peer` ignores `ServiceRemoved`, so a departed peer is invisible and a transient drop is permanent.
  - **Receive-side authorization + anti-replay** (spec §7.5/§9: `(owner_epoch, ctr)` high-water, current-owner + capability + permission gate before inject) — none; raw decoded frames return straight to the caller.
  - **Ack-timeout cursor snap-back** (arch §4.6) — `grant_to` moves to `Standby` immediately with no ack tracker/timeout; a handoff to an unreachable target strands the cursor.
  - **Sleep/wake + `Goodbye{Sleep}`** (spec §7.1) — no power-event hooks anywhere.
  - **State/clipboard resync after reconnect** (spec §7.2/§7.7/§7.10) — CRDT/clipboard/coordinator messages aren't defined; no anti-entropy.
- **fix:** Wave 2 — build `mouser-engine` (per-peer supervised tasks: control read/dispatch loop, heartbeat+watchdog, reconnect with backoff, receive auth+anti-replay gate, ack-timeout snap-back, power hooks, anti-entropy) + `mouser-ipc`. Define the missing wire messages (`Heartbeat`/`Pong`/`Goodbye`/`KeyEvent`/`PointerButton`/`Scroll`/state/clipboard/coordinator).

### C2-2 — Mobile apps send nothing: companion is a local-only mock (no FFI/network)
- **where:** `apps/ios/**`, `apps/android/**` — no `mouser-ffi`/uniffi, no socket/NWConnection/NsdManager; every gesture ends in local state + haptics.
- **verified:** ✓ orchestrator + convergent (T10 CRITICAL, Codex-10 HIGH ×2). The mobile analogue of C2-1.
- **impact:** the entire mobile purpose (drive the active machine's cursor/keys) is unimplemented; gestures/haptics/layout are built but inert.
- **fix:** build the `mouser-ffi` uniffi surface (connect/pair, `send_pointer_motion_rel`, `send_pointer_button`, `send_scroll`, `send_key_event`, `request_ownership`); bind via XCFramework (iOS) and uniffi-kotlin/JNI (Android); route existing gesture callbacks into it.

---

## HIGH

### C2-3 — Interactive first-contact trust is stubbed; any pinned cert is fully trusted
- **where:** `crates/mouser-net/src/pin.rs:30-35` (`TrustOnFirstUse` accepts any well-formed cert); `transport.rs` has no §5 `Hello`/SAS/`channel_sig` on the interactive plane; no `mouser-sas-v1`/`mouser-channel-v1`/HKDF anywhere.
- **verified:** ✓ orchestrator (grep: no SAS/HKDF; `send_control`/`send_motion` usable immediately after `establish`). Convergent: T2 (graded CRITICAL) + Codex-02 (HIGH). Graded **HIGH** here: it is part of the not-yet-built engine handshake, but it is the specific security boundary (§1 "secure by default / mandatory channel-bound pairing") and must land with the engine.
- **fix:** implement `[01]Hello`/`[02]HelloAck`, interactive `channel_sig` over `export_keying_material("mouser-channel-v1", device_id, 64)`, the mandatory 6-digit SAS (§5), and a connection typestate that refuses input/state/clipboard/file APIs until trust is established.

### C2-4 — File-transfer integrity is computed but cannot be verified end-to-end (no hash on the wire)
- **where:** `crates/mouser-protocol/src/messages.rs` — `FileOffer`/`FileChunk`/`FileDone` carry **no digest field**; `crates/mouser-files/src/receiver.rs:55-78,166` — `ReceiverConfig::new` defaults `expected_hashes` empty ⇒ **size-only** completion.
- **verified:** ✓ orchestrator (offer has only `{name,size}`; receiver flattens to `None`). Convergent: Codex-06 (HIGH), Codex-11 (MED), T6.
- **impact:** with no out-of-band hash (and there is no in-band one), a corrupt or truncated-then-padded file is committed as `ok=true`. The SHA-256 gate the module advertises is unreachable in the real protocol.
- **fix:** add an optional per-file `sha256` to `FileOffer.files[]` (or a `FileDone.digest`) so the receiver can compare; until then document that completion is size-only.

### C2-5 — Resume vs symlink-safe sink are mutually exclusive; no production sink reconciles them
- **where:** `crates/mouser-files/src/sink.rs:9-11,46-51` (contract: `existing_len()` is the resume point; open with `create_new`, never follow a symlink) vs the only disk sink, the test `FsSink` in `crates/mouser-net/tests/bulk_transfer.rs:44-70`, which uses `create_new(true)` (errors if a partial file exists) **and** ignores `offset` in `write_at` (append-only).
- **verified:** ✓ orchestrator (doc + test sink read). Convergent: T6 (HIGH), Codex-12 (HIGH).
- **impact:** real disk resume either refuses to reopen a partial file (resume dead) or drops `create_new` and reintroduces the symlink-follow it exists to prevent; and an `offset`-ignoring `write_at` silently corrupts on any resume/retransmit.
- **fix:** define the reconciliation in the engine's sink: `symlink_metadata` (lstat) the path, reject if a symlink, open `write+create`, positioned `write_all_at(offset, …)`, assert `offset == existing_len()`, and hash a streaming `Sha256` updated in `write_at` (never re-read the path in `finish()` — TOCTOU). Add a disk-backed resume test.

### C2-6 — mDNS browse drops the resolved address; departures never surface
- **where:** `crates/mouser-net/src/discovery.rs:75-87` (`PeerAdvert` has `iport`/`bport` but **no IP**; `from_service_info` ignores `get_addresses()`); `:176-186` (`next_peer` matches only `ServiceResolved`, never `ServiceRemoved`).
- **verified:** ✓ convergent with specific lines (T3 HIGH, Codex-03 HIGH); structural (no addr field on the struct).
- **impact:** a discovered peer cannot be dialed (`connect_*` need a `SocketAddr`); the reconnect supervisor would have nowhere to dial and stale peers never prune.
- **fix:** add `addrs: Vec<IpAddr>` populated from `info.get_addresses()` (skip if empty); change the browse API to yield `Found`/`Removed`.

### C2-7 — A single oversize/unsupported motion datagram kills the pump; no §6.1 fallback
- **where:** `crates/mouser-net/src/motion.rs:51-69` — every `send_datagram_wait` error is fatal `return`; no distinction of `TooLarge`/`UnsupportedByPeer`/`Disabled` vs `ConnectionLost`, no control-stream degrade.
- **verified:** ✓ T3; consistent with Codex-03/04 stale-send findings.
- **impact:** on a constrained path, motion stops permanently on a live connection with no diagnostics; §6.1 mandates degrade-onto-control-stream.
- **fix:** match the error kind — only `ConnectionLost` ends the pump; drop one `TooLarge` sample and continue; on `UnsupportedByPeer`/`Disabled` switch to the coalesced control-stream fallback.

### C2-8 — Windows keymap is missing the entire keypad block; the parity test can't catch it
- **where:** `crates/platform-win/src/keymap.rs` — HID usages `0x53–0x63`, `0x67`, `0x85` (NumLock, KP / * - + Enter, KP0–9, KP .) are unmapped in both the scancode and VK tables (mac/linux map all). `crates/platform-mac/tests/keymap_parity.rs` compares mac vs linux only; `platform-win` exposes no `supported_hid_usages()`.
- **verified:** ✓ orchestrator (no `0x53..=0x63` input arms in win; mac has the block; test is two-way). Convergent: T9 (HIGH ×2), Codex-09 (MED).
- **impact:** every keypad keystroke forwarded onto Windows is silently dropped (`UnmappedKey`); the gap is invisible because the parity test excludes Windows.
- **fix:** add the keypad block to both Windows tables (mind extended-flag disambiguation from the nav cluster), add `platform_win::keymap::supported_hid_usages()` (host-independent), and make the parity test assert all three platforms equal.

### C2-9 — macOS capture reports `display_id = 0` and global coords (no multi-display attribution, no Retina scaling)
- **where:** `crates/platform-mac/src/adapter.rs:112-124` — `CursorMoved { display_id: 0, x: p.x as i32, y: p.y as i32 }` from a global point; inject side maps displays correctly, capture side does not.
- **verified:** ✓ convergent (T9 MED, Codex-09 HIGH); graded **HIGH** — edge-cross/ownership logic depends on per-display coordinates (§7.6), so wrong attribution breaks multi-monitor handoff.
- **fix:** resolve the global point to its `DisplayBounds`, emit the real `display_id` and display-local `(x - bounds.x, y - bounds.y)`; preserve sub-pixel/Retina scale.

### C2-10 — Android app declares no `INTERNET` permission
- **where:** `apps/android/app/src/main/AndroidManifest.xml` — only `android.permission.VIBRATE`.
- **verified:** ✓ orchestrator (manifest read). Codex-10 HIGH.
- **impact:** once networking lands, any socket throws `SecurityException`; the app cannot connect to a peer at all.
- **fix:** add `<uses-permission android:name="android.permission.INTERNET"/>` (and `ACCESS_NETWORK_STATE` for liveness) plus the NSD/local-network bits.

---

## MEDIUM (grouped; all verified or convergent-with-line-refs)

**Protocol strictness**
- `CapabilitySet::deserialize` hard-errors on any member ≥65536 / negative / non-integer instead of dropping it (`enums.rs:181-188`) — §0.1 says unrecognized members are dropped; in-range unknowns *are* dropped, so a malformed/forward-version `Hello` is rejected wholesale. ✓ verified. (T1 graded HIGH, Codex LOW → **MEDIUM**.) Fix: decode via `i128`/`Value`, filter.
- `FileChunk.data` (serde_bytes) also decodes from a CBOR **array** of ints, not only a byte string (malleability vs §0.1). Codex-01. (**LOW–MEDIUM**.)
- Nested indefinite-length CBOR containers accepted (only top-level guarded) (`codec.rs:46-62`). T1/Codex-01. (**LOW**.)

**Transport / concurrency**
- `send_control` (and bulk stream writes) are **not cancel-safe**: dropping the future mid-`write_all` leaves a partial frame and desyncs the long-lived stream (`transport.rs:275-283`, `bulk.rs`). Symmetric with the fixed recv side (A3). ✓ verified structurally. Convergent: Codex-04/08. Fix: dedicated writer task / mpsc so sends are never cancelled mid-write.
- `consume_prime` discards the first control frame **without checking it is `TYPE_STREAM_PRIME`**, and the priming frame is an undocumented private extension not in the wire spec (`transport.rs:222-237,346-360`). ✓ verified (it ignores `_msg_type`; reads exactly one frame). Codex-03. Fix: only discard when the type matches, else seed the buffer; document the prime in §0.2/§6.1.
- Motion keep-newest pump can still emit a stale sample under congestion (`send_datagram_wait` parks on the old datagram) and the RX datagram buffer is unbounded-by-default, drained oldest-first (`motion.rs:52-69`, `transport.rs:68-75`). Convergent T4/Codex-03/04. Fix: non-blocking drop-oldest send + small `datagram_receive_buffer_size`.
- Bulk plane has no graceful drain/`Drop` (interactive H6 not mirrored) (`bulk.rs:142-146,232-270`). T4.
- No QUIC `Retry`/address validation on accept; no per-source rate limit / untrusted-conn cap (`transport.rs:133-143`, `bulk.rs:113-125`) — §10 anti-amplification. T3.
- No `PointerMotionRel` send path though codec+spec define it (`transport.rs:297-302`). T3.

**State machines (ownership/election)**
- `on_yield` accepts a yield at term ≥ holder's (`election.rs:256-263`) → a stale/forged/duplicated yield drops a valid lease. Convergent T5/Codex-05. Fix: require `==`.
- Lower-term lease can resurrect after a higher term was seen; yield terms aren't counted as observed (`seen_term`). Codex-05.
- `start_claim` unconditionally usurps a healthy coordinator and inflates `seen_term` (`election.rs:300-316`). T5.
- A bare `CoordinatorClaim` is adopted as a full lease with a fabricated TTL (`election.rs:212-251`). Convergent T5/Codex-05.
- Equal-epoch grant-vs-reclaim can diverge views if the minting invariant is ever broken (`ownership.rs:194-205`). T5 (LOW–MEDIUM, defense-in-depth).

**File-transfer engine**
- Sender applies `FileAccept.resume` unconditionally — accepts ACKs/resume for bytes it never sent, no monotonicity/dup-index guard, `on_accept` not single-shot (`sender.rs:95-115`). Convergent T6/Codex-06/Codex-12.
- Forward-gap chunk returns a fatal `FileError` that tears down the connection instead of a recoverable re-ack (`receiver.rs:257-262`). T6.
- Receiver abort emits one `FileDone{ok:false}` then the reference driver loops forever (`is_complete()` excludes the aborted terminal state) (`receiver.rs:206-214,343-345`). T6. Fix: `is_terminal()`.
- Write failure in the disk sink doesn't emit a terminal `FileDone` to the peer. Codex-06.
- Path sanitizer accepts Windows reserved device names (`CON`,`COM1`…), trailing dots/spaces, RTL-override (`path.rs:56-96`) → wrong-path/collision on Windows (stays in quarantine, so not an escape). Convergent T6/Codex-06.
- No upper bound on offered `size` / file count / total bytes (`receiver.rs:111-174`) — unbounded disk DoS. T6.
- `FileReject` has no sender-side terminal transition (`sender.rs` has no `on_reject`). T6.
- Rejected multi-file offers can leave already-created earlier sinks/quarantine artifacts. Codex-08/10/12.

**Platform / input**
- macOS `FlagsChanged` can leave a modifier stuck: releasing one of two held Shift/Cmd twins yields no net flag change → `None`, so the remote never sees the release (`keymap_capture.rs`, documented in tests). ✓ verified. Codex-09. Fix: key off the keyCode, not only the shared flag bit.
- Windows scroll ignores `ScrollUnit` and casts negative `i32`→`u32` for `mouseData` (`platform-win/src/inject.rs:249-273`). Convergent T9/Codex-08/09.
- No Windows/Linux `InputCapture`; Windows not wired to `InputInjection` (free fns, no `mods`) (`platform-win/src/lib.rs`, `platform-linux`). Convergent T9/Codex-09/T11.
- Linux absolute injection ignores `display_id`; Linux LogicalPixel scroll injected as detents. Codex-09.
- macOS `read_dragged_file_urls()` reads the **general** pasteboard, not the live drag pasteboard (`NSPasteboardNameDrag`) (`dragdrop.rs:197-199`); a `_from` variant exists but the default is wrong for in-flight drags. ✓ verified. Codex-12.

**Mobile**
- iOS portrait does not pin the native keyboard below the touchpad (plain `VStack`, no keyboard-avoidance; fragile timed focus) (`CompanionView.swift:38-61`). T10. (Android does this correctly via `imePadding`+`adjustResize`.)
- No app-lifecycle/background/reconnect handling in either app (no `scenePhase`, no `Lifecycle` observer). Convergent T10.
- iOS one-finger pan + click-drag long-press both call `registerMove` → double motion during drag-select (`TrackpadHostView.swift:93-117,156-174`). T10. Fix: `onePan.require(toFail: clickDrag)`.
- Magnify/rotate gestures emitted by both apps but **no wire message exists** for them (spec defines only Key/Button/Scroll/Motion). Convergent T10.
- Scroll events carry no `ScrollUnit` and no natural-direction handling. T10/Codex (latent until wiring).
- iOS momentum scroller can leak a `CADisplayLink` on orientation teardown (no `deinit { stop() }`). Convergent T10/Codex-10.
- Android mixes wall-clock (`System.currentTimeMillis`) and frame-clock for gesture timing. T10.
- `device_id`/SPKI derivation hashes a **reconstructed canonical** Ed25519 SPKI, not the presented cert's literal SPKI, and never checks the AlgorithmIdentifier OID (`mouser-core/identity.rs:52-80`, `mouser-net/identity.rs`). ✓ verified. Convergent T2/Codex-02. For our own canonical certs it matches; clarify spec or hash presented bytes + validate OID/tags.
- Bulk channel-binding is one-directional (dialer never verifies the acceptor's binding); `session_id` is a plain caller-supplied `u64` (`bulk.rs:74-140`). T2. Fix: mutual `BulkHello`, CSPRNG session id.
- Panic-free clippy lints are **not workspace-wide** — only `mouser-protocol`/`mouser-net`/`mouser-files` set them; `mouser-core` and the platform/ffi/desktop crates don't (`Cargo.toml` has no `[workspace.lints.clippy]`). ✓ verified. (T8 graded HIGH, Codex LOW → **MEDIUM**.) Fix: central `[workspace.lints.clippy]`, keep only the `unsafe_code` carve-out per platform crate.
- Dangling `TYPE_HELLO_ACK` const with no `HelloAck` struct (`messages.rs:7-8`). Codex-11/T11.
- Duplicate `device_id` (cloned identity) has no detection/rejection path. T12.
- Rapid edge-cross flapping has no debounce/hysteresis in the ownership core (`edge_dwell_ms` unused) (`ownership.rs:134-168`). T12.

---

## LOW (abbreviated)
Missing `// SAFETY:` on three `dragdrop.rs` unsafe sites (unsafe itself verified sound); `platform-linux` opts out of `forbid(unsafe_code)` but uses no `unsafe`; magic `O_NONBLOCK` literal; `.expect()` on poisoned mutex in mac/linux injector paths; `encode_frame_capped`/`MAX_BULK_FRAME` (2 MiB < spec's 8 MiB `StateSnapshot`) over-export/limit drift; secret key material not zeroized, `device_id` pin compare not constant-time (both informational — `device_id` is public); injector free-fn naming divergence (`key_press` vs `key`); `libc` unused dep in `platform-mac`; desktop `OsKind` uses `"phone"` vs spec `ios`/`android`; `Datagram::Unknown` skips the trailing-byte strictness check; election `on_lease` doc says "lower-or-equal" but code is strictly-lower; `Tray` trait on core despite headless boundary; zero-byte file `is_complete` true pre-accept; bulk `transfer_id` not bound to the stream; Android `material-icons-extended` bloat + minify off; iOS test-only `-startOrientation` hook in production `onAppear`; iOS `_mouser._udp` Bonjour type unverified against the advertiser; epoch/term saturate (don't wrap) at `u64::MAX`.

---

## Rejected on verification
- **Codex-03 "acceptor over-reads and drops a following control frame":** FALSE. `consume_prime` (`transport.rs:348-361`) uses `read_exact` for exactly the 8-byte header + `payload_len` (0 for the prime); it cannot consume bytes of a subsequent frame. The *valid* part of that finding — discarding the first frame without a type check, and the prime being undocumented — is recorded under MEDIUM.

## Round-1 fixes re-verified as holding
A2 (eager symmetric prime — works within our impl), A3 (`recv_control` cancel-safe via persistent buffer + cancel-safe `read`), A4 (send-side keep-newest pump — residual stale-send under congestion noted MEDIUM), A6 (strict trailing-byte decode), H1 (keep-alive/idle), H6 (interactive graceful drain — **not** mirrored on bulk, MEDIUM), H7 (scalar enum unknown→Unknown), H8 (drop-on-corrupt motion), H2 (mac/linux implement core traits — Windows still pending), H11 (mac↔linux keymap parity — Windows excluded, see C2-8). Platform `unsafe` re-reviewed against vendored objc2/core-graphics/input-linux/windows crate sources and found **sound** (only missing SAFETY comments).

## Top remediation order
1. **Wave 2 `mouser-engine`** (C2-1): build the supervised runtime with heartbeat, reconnect+backoff, **receive-side auth + anti-replay**, ack-timeout snap-back, sleep/wake `Goodbye`, and the §5 pairing/SAS (C2-3). Define the missing wire messages.
2. **File-transfer hardening** (C2-4/C2-5): add a wire digest, build the symlink-safe positioned-write resumable disk sink, fix sender resume-trust and receiver terminal/forward-gap handling, path-sanitizer Windows names.
3. **Discovery + motion** (C2-6/C2-7): carry the resolved address + removals; motion error-kind handling with control-stream fallback.
4. **Platform parity** (C2-8/C2-9): Windows keypad + three-way parity test; mac capture per-display coordinates; Windows/Linux capture + Windows `InputInjection`.
5. **Mobile wiring** (C2-2) + Android `INTERNET` + iOS keyboard-below layout + lifecycle/reconnect.
6. **Hygiene:** workspace-wide panic-free lints, cancel-safe `send_control`, election edge-case fixes, LOW cleanups.
