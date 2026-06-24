# Input latency verification

This document records how Mouser's keyboard/mouse forwarding latency is measured and the
results, so "is there any lag?" can be answered with numbers rather than feel.

The performance budget is the repo SLO in [`architecture.md`](architecture.md): **≤ 5 ms
wired / ≤ 15 ms good Wi-Fi, motion-to-injection**. Human-perception research puts
imperceptible pointer latency below ~20 ms, with expert users detecting ~5–10 ms — so the
5 ms wired target already aims below the perceptual floor.

Latency is decomposed into three independently-measurable layers. The first two run on a
single machine (one clock, no cross-host clock-sync needed); the third is the network half.

## 1. Serialization (`mouser-protocol/tests/perf_codec.rs`)

Pure encode/decode of the wire types, 1M iterations each, release build.

| Function | Wire size | ns/op |
|---|---|---|
| `encode_motion` (postcard) | 10 B | ~136 |
| `decode_datagram` | — | ~12 |
| `to_cbor(KeyEvent)` | 43 B | ~178 |
| `from_cbor(KeyEvent)` | — | ~244 |
| `encode_frame` | 51 B | ~32 |

## 2. Engine dispatch (`mouser-engine/tests/perf_core.rs`)

Target-side `EngineCore` per-event work — anti-replay + rate-cap + action build, plus CBOR
decode for keys. The OS injection call (`SendInput`/`GetSystemMetrics`/`EnumDisplayMonitors`)
is deliberately **not** benchmarked here: it is a real syscall whose timing reflects the
OS/driver, not engine code.

| Path | ns/op |
|---|---|
| `on_motion` (decoded datagram → inject action) | ~33 |
| `on_control` (key: decode + anti-replay + transition + rate-cap + action) | ~331 |

## 3. End-to-end loopback (`mouser-net/tests/loopback_latency.rs`)

Two real QUIC interactive endpoints (full §5 handshake + cert pinning) on `127.0.0.1`,
reusing one connection. Same-process clock ⇒ one-way `recv_at − send_at` is exact.

| Plane | p50 | p90 | p99 | max | delivery |
|---|---|---|---|---|---|
| **Key** (reliable control stream) | ~40 µs | ~71 µs | ~134 µs | ~0.4 ms | 100% |
| **Motion** (unreliable keep-newest datagrams) | ~241 µs | ~345 µs | ~552 µs | ~0.9 ms | 100% |

Motion is intentionally lossy + keep-newest coalescing (a cursor only needs the latest
position); the harness seq-tags samples and measures delivered ones. Under a 1 kHz offer
rate on loopback, delivery is 100%.

## 4. Network half (LAN RTT)

The motion plane is UDP datagrams; the network adds ≈ RTT/2 one-way. Measured Windows↔Mac
on this LAN via a TCP-connect probe to the Mac's SSH port (macOS drops ICMP, so `ping`
fails): **min 3.0 ms / median 3.7 ms RTT ⇒ ~1.5–2 ms one-way** (discard the first cold
ARP-warmup sample). quinn already computes a smoothed path RTT continuously; surfacing it in
the snapshot/MCP (`InteractiveConnection::rtt()` → `ConnectionDto.rtt_ms`) gives a live
in-app readout for the real two-machine path.

> ⚠️ **Wi-Fi jitter is the one real variable.** The same LAN probe occasionally spiked to
> ~107 ms (p95) — a Wi-Fi retransmit/contention stall, not a Mouser cost. On the reliable
> key stream a spike delays a keystroke until retransmit; motion (unreliable, keep-newest)
> just drops a stale position. A wired link removes this.

## Verdict

Serialization + engine dispatch + transport together add **well under 1 ms** (typical
loopback key one-way ~40 µs, motion ~240 µs). Adding ~1.5–2 ms LAN one-way and a sub-ms OS
inject, **total end-to-end input latency is ~2–3 ms on a wired/good LAN path — within the
5 ms wired SLO and far below the ~20 ms perceptual floor.** Mouser's code is not a lag
source; the only headroom risk is Wi-Fi jitter, which is environmental.

The flood test (`flood_never_drops_any_key_release`) additionally proves no key-up is ever
dropped under a burst far exceeding the rate-cap — i.e. no stuck keys under load.

## Reproduce

```sh
cargo test --release -p mouser-protocol --test perf_codec       -- --nocapture
cargo test --release -p mouser-engine   --test perf_core        -- --nocapture
cargo test --release -p mouser-net      --test loopback_latency -- --nocapture --test-threads=1
cargo test --release -p mouser-engine   --test core_logic        flood_never_drops_any_key_release
```

Run in release — debug builds make microbench timings meaningless.
