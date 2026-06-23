//! mouser-ffi — the mobile FFI bridge (uniffi) that lets a phone act as a pure
//! **source/controller** for a Mouser peer engine over QUIC.
//!
//! A phone has no keyboard/mouse to *capture* and never *injects* (it is not a
//! handoff target), so this crate reuses the already-tested [`mouser_engine`] in
//! **source** mode with a no-op injector and drives it from synthetic input events
//! the Swift/Kotlin UI feeds in (taps, drags, on-screen keyboard). The protocol,
//! transport, pinning, anti-replay, and heartbeat logic are NOT reimplemented here —
//! this is a thin synchronous façade over [`RuntimeHandle`].
//!
//! ## Ownership model (why the phone hands input to the peer)
//! [`mouser_engine::EngineCore`] forwards keyboard/button/scroll/motion to the peer
//! only while the peer — not this node — owns input (a source captures locally while
//! it owns its own screen, and forwards once the cursor has crossed to the peer). A
//! phone is *always* remote-driving, so right after connecting we feed one edge-cross
//! so the engine grants ownership to the peer; from then on every event the UI sends
//! is forwarded over the wire. See [`MobileClient::connect`].
//!
//! ## Scope (deliberate non-goals — follow-ups, not bugs)
//! - **Discovery is out of this FFI.** iOS mDNS must use native `NWBrowser` (the Rust
//!   `mdns-sd` raw multicast needs the special iOS multicast entitlement), so
//!   [`MobileClient::connect`] takes an explicit `host`/`port` that Swift obtains from
//!   `NWBrowser`.
//! - Full Xcode integration, on-device install, SAS pairing UI, and Android/Kotlin
//!   wiring are follow-ups. This crate compiles for `aarch64-apple-ios` and produces
//!   Swift/Kotlin bindings via `uniffi-bindgen`.

// This crate keeps uniffi's `unsafe extern "C"` scaffolding, so it can't adopt
// `[lints] workspace = true` (that would pull in `unsafe_code = "forbid"`).
// Replicate the workspace panic-free clippy denies here instead (mirrors the
// platform-* adapters). Test code is exempt via `#[cfg(test)]`.
#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::sync::{Arc, Mutex};

use mouser_core::platform::{
    CaptureMode, InputCapture, InputInjection, InputSink, LocalInputEvent, PlatformResult,
    ScrollUnit,
};
use mouser_engine::discovery::decode_device_id;
use mouser_engine::{EdgeLayout, EngineCore, RuntimeHandle};
use mouser_net::{DeviceIdentity, InteractiveConnection, InteractiveEndpoint, PinPolicy};
use mouser_protocol::TYPE_DEVICE_NAME;

uniffi::setup_scaffolding!();

/// Logical-pixel size handed to the source layout. The phone reports cursor *motion*
/// (deltas) and the engine clamps the virtual peer cursor into this box; it is made
/// large so ordinary movement never accidentally hits the far clamp. The peer clamps
/// absolute coordinates to its real display on receipt (spec §7.6), so the exact value
/// here only bounds the engine's internal virtual cursor.
const VIRTUAL_SPAN: i32 = 1 << 20;

/// Step used to push the virtual peer cursor off the entry edge after the initial
/// edge-cross, so a subsequent leftward delta does not immediately reclaim ownership
/// back to the phone (see [`MobileClient::connect`]).
const SEED_STEP: i32 = 16;

/// Errors crossing the FFI boundary. No panics ever cross: every fallible path maps to
/// one of these (uniffi turns them into a thrown Swift error / Kotlin exception).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// The supplied peer `device_id` was not valid base32 / not a 32-byte id.
    #[error("invalid peer device id")]
    InvalidPeerId,
    /// `connect` was called while already connected. Call `disconnect` first.
    #[error("already connected")]
    AlreadyConnected,
    /// An input/connection method was called before a successful `connect`.
    #[error("not connected")]
    NotConnected,
    /// Binding the local QUIC endpoint or resolving its address failed.
    // Field is `detail`, not `message`: uniffi's Kotlin codegen renders a struct
    // error variant field named `message` as a property that shadows
    // `Throwable.message`, which fails to compile (overload-resolution ambiguity).
    #[error("bind failed: {detail}")]
    Bind { detail: String },
    /// The QUIC dial / pinned-TLS handshake to the peer failed.
    #[error("connect failed: {detail}")]
    Connect { detail: String },
}

/// The phone never injects: it is a pure controller, not a handoff *target*. The
/// source-mode engine only ever produces inject actions for input it *receives* while
/// it owns a peer — which never happens here — so these are inert. They exist solely to
/// satisfy [`RuntimeHandle::start`]'s [`InputInjection`] requirement.
struct NoopInjector;

impl InputInjection for NoopInjector {
    fn move_cursor(&self, _display_id: u32, _x: i32, _y: i32) -> PlatformResult<()> {
        Ok(())
    }
    fn move_cursor_relative(&self, _dx: i32, _dy: i32) -> PlatformResult<()> {
        Ok(())
    }
    fn button(&self, _button: u8, _down: bool) -> PlatformResult<()> {
        Ok(())
    }
    fn key(&self, _usage: u16, _down: bool, _mods: u16) -> PlatformResult<()> {
        Ok(())
    }
    fn scroll(&self, _dx: i32, _dy: i32, _unit: ScrollUnit) -> PlatformResult<()> {
        Ok(())
    }
}

/// A no-op [`InputCapture`] for the mobile bridge: the phone is a pure controller whose
/// input comes from its own touchpad/keyboard UI, not from the engine's local-capture
/// path. It never edge-senses or suppresses, so every mode transition is inert. Exists
/// only to satisfy [`RuntimeHandle::start`]'s [`InputCapture`] requirement.
struct NoopCapture;

impl InputCapture for NoopCapture {
    fn set_mode(&self, _mode: CaptureMode, _sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        Ok(())
    }
    fn stop(&self) -> PlatformResult<()> {
        Ok(())
    }
    fn can_suppress(&self) -> bool {
        false
    }
    fn current_mode(&self) -> CaptureMode {
        CaptureMode::Off
    }
}

/// Live connection state, held together so `disconnect` (or drop) tears both down: the
/// engine runtime and the QUIC connection it drives. The connection is kept alive
/// alongside the handle because the runtime's background tasks borrow it via `Arc`.
struct Session {
    runtime: RuntimeHandle,
    _connection: Arc<InteractiveConnection>,
}

/// A mobile controller for a single Mouser peer.
///
/// Owns a multi-thread tokio runtime and, once connected, a source-mode engine. All
/// methods are **synchronous** at the FFI boundary: `connect` drives the async dial via
/// the held runtime's `block_on`; the input senders are already sync (they call
/// [`RuntimeHandle::feed_local`], which runs the sans-IO core inline).
#[derive(uniffi::Object)]
pub struct MobileClient {
    /// This device's pinned identity (its leaf cert / `device_id`). Generated per
    /// client instance; persisting it across launches is a Swift-side follow-up.
    identity: DeviceIdentity,
    /// The runtime that owns the QUIC endpoint's async tasks. Held for the client's
    /// lifetime so the connection's background tasks keep running between calls.
    rt: tokio::runtime::Runtime,
    /// The active session, if connected.
    session: Mutex<Option<Session>>,
}

impl MobileClient {
    /// Build a client around `identity` with a small multi-thread runtime (shared by the
    /// `new`/`from_seed` constructors).
    fn with_identity(identity: DeviceIdentity) -> Arc<Self> {
        // A small fixed pool: enough for the sender + the three receiver/ticker tasks
        // plus the dial, without spawning one thread per core on a phone.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            // The only failure here is the OS refusing to spawn threads at startup;
            // there is no caller to return an error to from a constructor, and a phone
            // that cannot start two threads cannot run the app at all.
            .unwrap_or_else(|e| {
                // SAFETY-OF-PANIC-FREE: constructor has no Result; fall back to the
                // current-thread runtime. If even that cannot spawn, the process cannot
                // run async work at all, so abort without unwinding across FFI.
                let _ = e;
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap_or_else(|_| std::process::abort())
            });
        Arc::new(Self {
            identity,
            rt,
            session: Mutex::new(None),
        })
    }
}

#[uniffi::export]
impl MobileClient {
    /// Create a disconnected client with a fresh identity and a multi-thread runtime.
    /// Prefer [`MobileClient::from_seed`] so the `device_id` is stable across launches.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Self::with_identity(DeviceIdentity::generate())
    }

    /// Create a client from a persisted 32-byte secret seed (see [`identity_seed`]), so
    /// the device keeps a **stable** `device_id` across launches — required for the
    /// desktop's trust/pairing to survive an app restart. A wrong-length seed falls back
    /// to a fresh identity (the caller should then persist the new [`identity_seed`]).
    ///
    /// [`identity_seed`]: MobileClient::identity_seed
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Arc<Self> {
        let identity = <[u8; 32]>::try_from(seed.as_slice())
            .map(|s| DeviceIdentity::from_seed(&s))
            .unwrap_or_else(|_| DeviceIdentity::generate());
        Self::with_identity(identity)
    }

    /// The 32-byte secret seed for this identity. Persist it in the platform keystore
    /// (iOS Keychain / Android Keystore) and restore it via [`MobileClient::from_seed`]
    /// so the `device_id` — and the desktop's trust of this device — survives restarts.
    /// This is private key material; store it securely.
    pub fn identity_seed(&self) -> Vec<u8> {
        self.identity.secret_seed().to_vec()
    }

    /// This device's own `device_id` as base32 (what the peer must pin against). The
    /// Swift UI surfaces this for pairing.
    pub fn device_id(&self) -> String {
        self.identity.device_id_base32()
    }

    /// Whether a session is currently active.
    pub fn is_connected(&self) -> bool {
        lock(&self.session).is_some()
    }

    /// Connect to a peer engine at `host:port`, pinning its `device_id` (§3), and start
    /// forwarding input as the source.
    ///
    /// `peer_device_id_base32` is the peer's base32 `device_id` (from `NWBrowser` /
    /// pairing). The dial is mutually pinned: we present our identity cert and require
    /// the peer's leaf cert to hash to the given id. After the connection is up we hand
    /// input ownership to the peer (one edge-cross) so subsequent events forward.
    pub fn connect(
        &self,
        host: String,
        port: u16,
        peer_device_id_base32: String,
        name: String,
    ) -> Result<(), MobileError> {
        let peer_id = decode_device_id(&peer_device_id_base32).ok_or(MobileError::InvalidPeerId)?;
        let mut guard = lock(&self.session);
        if guard.is_some() {
            return Err(MobileError::AlreadyConnected);
        }

        let addr = resolve(&host, port)?;
        let identity = &self.identity;
        // Synchronous at the boundary: bind the endpoint and drive the async dial on the
        // held runtime. quinn's `Endpoint::client` must be created inside a tokio runtime
        // context, so the bind happens inside `block_on` too (not just the dial).
        let connection = self.rt.block_on(async {
            // Ephemeral client-only QUIC endpoint (no listener; the phone dials out).
            // Bind the unspecified address of the peer's family so the OS routes
            // egress out the real interface — a loopback bind cannot reach a LAN peer.
            let endpoint = InteractiveEndpoint::bind_client(mouser_net::client_bind_for(addr))
                .map_err(|e| MobileError::Bind {
                    detail: e.to_string(),
                })?;
            let conn = endpoint
                .connect_interactive(identity, addr, PinPolicy::Pinned(peer_id))
                .await
                .map_err(|e| MobileError::Connect {
                    detail: e.to_string(),
                })?;
            // Announce our display name so the target can label us in its pairing prompt.
            // Advisory only (trust is the §3 cert pin); failure is non-fatal.
            let _ = conn.send_control(TYPE_DEVICE_NAME, name.as_bytes()).await;
            Ok(conn)
        })?;
        let connection = Arc::new(connection);

        // Source-mode engine: the peer sits to our "right" in a large virtual space, so
        // the very first cursor report crosses the edge and grants ownership to the peer
        // (`crosses_out` for `Edge::Right` is `x >= width - 1`).
        let me = self.identity.device_id();
        let core = EngineCore::new_source(
            me,
            peer_id,
            EdgeLayout::side_by_side(1, VIRTUAL_SPAN, VIRTUAL_SPAN, VIRTUAL_SPAN),
        );
        // `RuntimeHandle::start` calls `tokio::spawn`, so it must run inside the runtime
        // context; enter it for the duration of the start.
        let runtime = {
            let _guard = self.rt.enter();
            RuntimeHandle::start(
                core,
                Arc::clone(&connection),
                Arc::new(NoopInjector),
                Arc::new(NoopCapture),
            )
        };

        // Hand ownership to the peer: x >= width-1 (==0) crosses immediately. Then nudge
        // the virtual peer cursor off the entry edge so a later leftward delta doesn't
        // trip the back-cross reclaim (`Edge::Right` reclaims at peer_x <= 0 && dx < 0).
        let center = VIRTUAL_SPAN / 2;
        runtime.feed_local(LocalInputEvent::CursorMoved {
            display_id: 0,
            x: 0,
            y: center,
        });
        runtime.feed_local(LocalInputEvent::CursorMoved {
            display_id: 0,
            x: SEED_STEP,
            y: center,
        });

        *guard = Some(Session {
            runtime,
            _connection: connection,
        });
        Ok(())
    }

    /// Report a cursor position (logical pixels). The engine forwards the resulting
    /// motion to the peer as a lossy datagram (§7.6). Coordinates are treated as
    /// successive samples; the engine forwards their *motion* and the peer clamps the
    /// absolute result to its real display.
    pub fn send_pointer_moved(&self, display_id: u32, x: i32, y: i32) {
        self.feed(LocalInputEvent::CursorMoved { display_id, x, y });
    }

    /// Report a pointer button transition (`down` presses). Button index per §7.5
    /// (0=left, 1=right, 2=middle, 3=back, 4=forward).
    pub fn send_button(&self, button: u8, down: bool) {
        self.feed(LocalInputEvent::Button { button, down });
    }

    /// Report a key transition by USB HID usage (Usage Page 0x07, Appendix B) with the
    /// active modifier bitmask (`down` presses).
    pub fn send_key(&self, usage: u16, down: bool, mods: u16) {
        self.feed(LocalInputEvent::Key { usage, down, mods });
    }

    /// Report a scroll delta in logical pixels.
    pub fn send_scroll(&self, dx: i32, dy: i32) {
        self.feed(LocalInputEvent::Scroll { dx, dy });
    }

    /// Tear down the session: stop the engine tasks and close the QUIC connection.
    /// Idempotent — disconnecting when not connected is a no-op.
    pub fn disconnect(&self) {
        if let Some(session) = lock(&self.session).take() {
            session.runtime.shutdown();
            // `session._connection`'s `Drop` sends the peer a graceful CONNECTION_CLOSE.
        }
    }
}

impl MobileClient {
    /// Feed one synthetic local event to the source engine, if connected. Silently
    /// drops events while disconnected (the UI may emit a stray gesture during
    /// teardown); the senders are infallible at the FFI boundary by design.
    fn feed(&self, event: LocalInputEvent) {
        if let Some(session) = lock(&self.session).as_ref() {
            let _ = session.runtime.feed_local(event);
        }
    }
}

/// Lock the session mutex, recovering the inner value if a holder panicked (panic-free
/// discipline: never `unwrap` a poisoned guard).
fn lock(m: &Mutex<Option<Session>>) -> std::sync::MutexGuard<'_, Option<Session>> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve `host:port` to a single socket address. Accepts a literal IP (the common
/// `NWBrowser` case, which already resolves the address) or a hostname.
fn resolve(host: &str, port: u16) -> Result<std::net::SocketAddr, MobileError> {
    use std::net::ToSocketAddrs;
    (host, port)
        .to_socket_addrs()
        .map_err(|e| MobileError::Connect {
            detail: e.to_string(),
        })?
        .next()
        .ok_or_else(|| MobileError::Connect {
            detail: format!("no address for {host}:{port}"),
        })
}

#[cfg(test)]
mod tests;
