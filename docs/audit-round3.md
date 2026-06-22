# Mouser — Round 3 Code Audit (24-agent paired, current HEAD `575462a`)

Third paired audit, run against the **real current tree** — which since Round 2 grew a runtime:
`mouser-engine` (daemon `mouserd`, `runtime`, `core`, `daemon_store`, IPC pipes), `mouser-ipc`
(desktop↔daemon), and `mouser-state` (CRDT cluster state). An earlier Round-3 pass executed against a
**pre-engine checkout** and is discarded; this is the re-run the user requested ("stale session").

## Method
- **12 topics × (1 Opus + 1 Codex) = 24 reviewers**, re-scoped to the current architecture (engine/daemon/ipc/state).
- Grounding doc: the user's `docs/implementation-audit.md` (end-to-end wired-vs-scaffold truth). Reviewers were told to treat it as truth **and challenge it** — which surfaced that the audit is itself partially stale at this HEAD (see "Audit reconciliation").
- Every headline finding is **convergent across both reviewers** and/or **orchestrator-verified against source** (the three most severe were re-read directly: CRDT decode, macOS tap unwind, IPC perms).

## Severity summary (orchestrator-graded, deduplicated)
| Severity | Count | Theme |
|----------|-------|-------|
| CRITICAL | 1 | CRDT decodes wire bytes with no size cap → decompression-bomb OOM |
| HIGH | 11 | Security/trust (no SAS, trust=full perm, accept-before-grant), IPC has no access control, macOS tap can unwind through C, engine task supervision/ACK/inject, CRDT divergence wedges, clipboard/files unwired, mobile loopback bind, no reconnect |
| MEDIUM | ~26 | clipboard engine robustness, clipboard adapters, transport (anti-amplification, motion fallback, bulk cancel-safety), CRDT growth, capture leaks/downgrade, doc overclaims |
| LOW | ~15 | core.rs 509-line, expect_used not denied, coordinate overflow, etc. |

No memory-unsafety/UB confirmed **except** the macOS tap-callback unwind (HIGH) and the CRDT decode bomb (CRITICAL). Win32 clipboard HGLOBAL ownership, FFI Send/Sync, and the platform hooks were re-verified **sound**.

---

## CRITICAL

### R3-C1 — `mouser-state` decodes wire bytes with no §0.3 size cap → automerge DEFLATE decompression bomb (OOM DoS)
- **where:** `crates/mouser-state/src/state.rs:316-317` (`load` → `AutoCommit::load(bytes)`), `:346-350` (`apply_changes` → `Change::from_bytes(bytes.clone())`). No length/size check precedes decode (only `s.len()!=64` for the actor id).
- **verified:** ✓ orchestrator (read) + Opus-CRDT (CRITICAL). §0.3 mandates "reject oversize before allocating"; automerge's `Compressed` chunk runs `DeflateDecoder::read_to_end` into an unbounded `Vec`, and length-prefixed columns `vec![0; len]` up front.
- **impact:** a single trusted-but-malicious (or pre-SAS first-contact) peer's `StateDelta`/`StateSnapshot` can OOM-kill every replica that ingests it. (Latent today — `mouser-state` has no production consumer yet — but it is the riskiest new crate and must be bounded before wiring.)
- **fix:** enforce the §0.3 caps (control ≤256 KiB, snapshot ≤8 MiB) on the byte slice before `load`/`from_bytes`, and bound the *decompressed* size; add a fuzz target.

---

## HIGH (all convergent across Opus+Codex and/or source-verified)

### R3-H1 — No SAS / channel-bound pairing; trust is a blindly-typed `device_id`; first-contact MITM via spoofed mDNS id
- `crates/mouser-net/src/transport.rs:14-16` (Hello/SAS/`channel_sig` explicitly STUBBED), `sas.rs` (`compute_sas` only called from tests), daemon `serve.rs:309-334` (trust = membership check over a TOFU TLS handshake), `dial_discovered` pins the **mDNS-advertised** id. Convergent: Opus-sec ×4, Codex-05 ×3, Codex-08.
- **fix:** wire the existing `compute_sas` + a `Hello`/`HelloAck`/`channel_sig` typestate into accept/dial; no id enters `trusted-peers.txt` without a channel-bound human SAS confirmation.

### R3-H2 — Trust = full unconditional input permission; target injects before an ownership grant; no per-message permission/rate-limit; no revocation
- `crates/mouser-engine/src/core.rs:420-470` (gates only on `is_owner()`+anti-replay; no permission/rate gate), daemon accept path starts the injecting runtime once trusted. Convergent: Opus-sec, Codex-02/05/08 ("target accepts input before any ownership grant", HIGH).
- **fix:** §9 authorization gate (`capability ∧ local_permission`) in core before any inject; per-peer rate/burst cap; unlock gate; `untrust`/revoke.

### R3-H3 — IPC server enforces no same-user access — any local process can drive the daemon
- `crates/mouser-ipc/src/server.rs` / `path.rs`: **no** `set_permissions(0600)`, no `SO_PEERCRED`, no Windows pipe DACL / `reject_remote_clients`; Unix fallback path is `/tmp` (world-writable) when `XDG_RUNTIME_DIR` unset. ✓ orchestrator-verified (grep). Convergent: Opus-IPC ×2 (HIGH), Codex-03/09.
- **impact:** local snapshot disclosure (device/peer/trust list) + `Connect`/`Disconnect` control; `Disconnect` also tears down the whole daemon (Codex-02).
- **fix:** chmod 0600 / 0700 dir, peer-uid check; Windows SDDL granting only the user SID + reject remote.

### R3-H4 — macOS `CGEventTap` callback can unwind a Rust panic through the C frame (UB/abort); Windows guards it, macOS doesn't
- `crates/platform-mac/src/adapter.rs:289` (`sink.on_event(ev)` with no `catch_unwind`) vs `crates/platform-win/src/adapter.rs:518` (`catch_unwind(AssertUnwindSafe(...))`). ✓ orchestrator-verified. Convergent: Opus-capture, Opus-memory, Codex-10 (all HIGH).
- **fix:** wrap the mac tap sink dispatch in `catch_unwind` → `CallbackResult::Keep`, mirroring Windows.

### R3-H5 — Engine runtime: ownership-ACK ignored + injection failures swallowed + peer-loss/Disconnect unsupervised (cursor can strand)
- `crates/mouser-engine/src/runtime.rs:67-77` (`let _ = inject_result`), `:100-164` (detached tasks; heartbeat loops even when receivers died), `core.rs:379` (`TYPE_OWNERSHIP_ACK => Vec::new()`). A dead source never reclaims on the **target** side; a negative/lost ACK strands input. Convergent: Opus-engine (HIGH), Codex-01 ×2, Codex-12 ("peer loss and Disconnect are not supervised", HIGH).
- **fix:** treat any task exit as connection death (cancel siblings, surface terminal state); track ACK + timeout → reclaim; on inject failure emit `CapabilityState`/return ownership.

### R3-H6 — `mouser-state` divergence wedges: forged actor-seq aborts the whole anti-entropy batch; `layout_rev` LWW poisoning freezes layout cluster-wide; resolved LWW winner isn't applied
- `state.rs:345-354` (batch `?` abort on `DuplicateSeqNumber`), `:179/:232-246` (`layout_rev` from a peer-writable register; `u64::MAX` poison), and Codex-04 HIGH: "Layout LWW winner does not control returned monitor layout" (the resolved winner's layout isn't the one returned). Convergent: Opus-CRDT ×2 (HIGH), Codex-04.
- **fix:** apply changes individually skipping dup-seq; derive `layout_rev` from causal history not a free register; return the LWW-winner's layout.

### R3-H7 — Clipboard & file transfer are not wired through the daemon (library-only)
- `crates/mouser-engine/Cargo.toml` has no `mouser-clipboard`/`mouser-files` dep; `runtime.rs` has only Control/Motion lanes; `discovery.rs` advertises `bport:0`. Convergent: Opus-clip-engine/Opus-e2e, Codex-06/12. (Matches implementation-audit §5/§6.)
- **fix:** add a daemon clipboard driver (snapshot reps → offer/pull/data over control+bulk → write inbound to OS clipboard); bind+advertise a real bulk port; wire `mouser-files` to bulk streams.

### R3-H8 — Mobile FFI binds the QUIC client to loopback → phone→computer over LAN cannot connect
- `crates/mouser-ffi/src/lib.rs:196` (`bind_client(loopback_addr())`); discovery/connect UI is otherwise wired on both iOS/Android. Convergent: Opus-memory/Opus-e2e, Codex-10/12. One-line fix: bind `0.0.0.0:0`. (The loopback-only test masks it.)

### R3-H9 — Clipboard engine: a pull can hang forever (no abort/stall clearing), worst on same-hash reconnect; oversized-preferred-rep abandons sync instead of falling back
- `crates/mouser-clipboard/src/engine.rs` — no `abort`/`timeout`/`tick`; `on_offer` re-issue guard + supersession keep a stale `pending`; `best_entry` picks one rep then bails if it's over the size limit. Convergent: Opus-clip-engine ×2, Codex-06.
- **fix:** add `abort_pull`/deadline sweep; filter candidates by size before preference ranking.

### R3-H10 — Capture types leak their thread/grab on `Drop`; no panic/local-reclaim hotkey; runtime never queries `can_suppress()`
- mac/win/linux capture structs have no `Drop` → Linux can leave devices `EVIOCGRAB`-grabbed (stuck local input); no emergency-reclaim chord; `serve.rs` never reads `can_suppress()` → silent dual-drive when suppression is unavailable (e.g. mac without Accessibility). Convergent: Opus-capture ×3, Codex-10.

### R3-H11 — Transport: no QUIC Retry/anti-amplification (§10) + motion `ControlFallback` never consumed by the runtime
- `transport.rs` accept paths never call `incoming.retry()`/`remote_address_validated()` (reflection/amplification DoS); the engine never reads `conn.motion_plane()` so motion is silently lost on datagram-disabled links. Convergent: Opus-transport ×2, Codex-01/08.

---

## MEDIUM (grouped; convergent or source-cited)
- **Clipboard adapters:** Windows writes use a NULL clipboard owner (Opus+Codex — *verified not a memory bug* for immediate-render, but breaks any future delayed render and is non-idiomatic); mac clears the pasteboard **before** UTF-8 validation (wipes clipboard on bad input); mac `UriList` uses a non-native UTI (no Finder interop); linux change-token misses binary-only changes; Windows writes bare-LF into `CF_UNICODETEXT`.
- **Transport/bulk:** bulk `TransferStream` recv/send not cancel-safe (latent — no prod caller); motion keep-newest still sends a stale sample under congestion; `bport:0` makes bulk undialable; control backpressure can stall motion (shared lane).
- **CRDT:** unbounded history/tombstone growth (no compaction → eventually exceeds the 8 MiB snapshot cap); `layout_rev` saturates at `u64::MAX`; snapshot load accepts non-genesis docs; convergence tests are happy-path only (no 3-replica/duplicate/adversarial coverage).
- **Daemon/IPC:** `Disconnect` shuts down the whole daemon; IPC `connect` ignores the peer registry it shows the UI; identity seed not created with private perms atomically; unbounded IPC clients/command queue (local DoS).
- **Engine:** injection failures swallowed → no capability downgrade (also H5); local scroll units mislabeled; motion seq saturation can freeze the remote pointer.
- **Desktop/mobile:** input/security/clipboard/layout settings are local React state only — **dead-end controls** (the Security toggles imply enforcement that doesn't exist); mobile clipboard mock-only; mac Cmd↔Ctrl swap implemented but hard-wired off (Ctrl-C/V parity broken); iOS/Android transfer indicators can't render a failed pull.

## LOW (abbreviated)
`mouser-engine/src/core.rs` is 509 lines (>500 rule; `platform-win/src/adapter.rs` is 966); `expect_used` is not in the deny set (risky `.expect()` on FFI/daemon paths); coordinate-delta arithmetic uses plain `+`/`-` (debug-overflow panic, release wraps-then-clamps); discovery surfaces port-0 peers to the UI; `uri_list` trailing-blank-line (by design); macOS captured scroll `i64→i32` truncation; Android lifecycle observer double-registered; staged-sidecar `mouserd` can go stale vs source.

---

## Audit reconciliation — `implementation-audit.md` is itself partially stale at HEAD `575462a`
The reviewers independently confirmed several of the implementation-audit's **highest-severity "missing" claims are now FALSE** at this HEAD (the audit predates `daemon_store`/IPC/general-settings commits):
- **Identity & trust DO persist** — `daemon_store.rs` writes `identity.seed` (0600) + `trusted-peers.txt`; the audit's §9 #1 ("fresh identity each run / no trust store") is obsolete.
- **`mouser-ipc` exists and the desktop owns the daemon lifecycle** — spawns/attaches/kills `mouserd`, live snapshot + `connect_peer`/`disconnect_peer`; audit §1/§7 ("no ipc crate / no lifecycle") stale.
- **Direct `connect` is peer-id-pinned + trust-gated** (`direct.rs`); only `probe` is TOFU and it never injects; audit §9 ("connect needs no peer id") stale.
- **Mobile discovery/connect ARE wired** (iOS `NWBrowser`, Android `NsdManager` → `connect`); audit §8 ("not wired / fixed demo peers") stale — though all of it dead-ends on the loopback bind (R3-H8).

What **still holds** from the implementation-audit (confirmed): clipboard/files not wired through the daemon (R3-H7), mobile loopback bind (R3-H8), no reconnect supervisor (R3-H5), single-peer-only, no SAS (R3-H1), dead-end settings, and doc overclaims. **Recommendation:** re-date `implementation-audit.md` to a commit and refresh §1/§7/§8/§9, or it becomes the next overclaiming doc.

## Doc overclaims corrected this round
- `docs/build-status.md` "shared clipboard is complete end-to-end" → **false** (engine/daemon/UI/FFI not wired); corrected.
- `docs/build-status.md` "`mouser-ffi` … stub" → stale (full `MobileClient`); corrected.
- `docs/windows-build.md` references `mouser-engine.exe` / "daemon runtime missing" → stale (binary is `mouserd.exe`, daemon exists); flagged for the Windows-build owner.

## Highest-priority fix order (reconciled)
1. **Security gate** (R3-H1/H2): SAS + `Hello`/`channel_sig` typestate before input; permission/rate gate in core; revocation.
2. **IPC access control** (R3-H3) + **macOS tap `catch_unwind`** (R3-H4) — small, high-impact safety fixes.
3. **CRDT hardening** (R3-C1/H6): size-cap before decode, per-change apply, causal `layout_rev` — before `mouser-state` is wired.
4. **Engine supervision** (R3-H5): connection-death propagation, ACK timeout/snap-back, inject-failure downgrade, reconnect supervisor.
5. **Mobile loopback bind** (R3-H8) — one line, unblocks the phone path.
6. **Wire clipboard/files through the daemon** (R3-H7): bulk port + drivers.
7. Capture Drop/`can_suppress`/panic-hotkey (R3-H10); transport anti-amplification + motion fallback (R3-H11); MEDIUM/LOW cleanups.
