//! The interactive **control stream** machinery (§6.1, §0.2): the long-lived bidi QUIC
//! stream that carries every reliable, ordered control message. Split out of
//! [`crate::transport`] so that crate stays focused on endpoint/connection lifecycle.
//!
//! Two cancel-safety guarantees live here:
//! - **send** goes through a dedicated writer task fed by an mpsc channel. The task owns
//!   the [`SendStream`] and writes one whole §0.2 frame before taking the next, so a
//!   caller dropping its `send` future (e.g. under a `tokio::select!`/timeout) can never
//!   leave a partial frame on the wire and desync the stream.
//! - **recv** accumulates bytes in a persistent buffer ([`RecvState::buf`]); a frame is
//!   only removed once fully present, so a dropped recv future loses nothing (A3).

use quinn::{RecvStream, SendStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::NetError;

/// Reserved control-frame type for the stream-priming frame (A2). It carries an empty
/// payload and is consumed during connection setup; it is never surfaced to callers.
pub(crate) const TYPE_STREAM_PRIME: u16 = 0xFFFF;

/// One queued control-stream write: the fully-encoded §0.2 frame plus a oneshot the
/// writer task fires once the frame is fully on the wire (or the write fails).
struct WriteRequest {
    frame: Vec<u8>,
    done: oneshot::Sender<Result<(), NetError>>,
}

/// Receive-side state for the control stream: the quinn stream plus bytes read but not
/// yet consumed as a complete frame. Buffering here (not on the stack of a `recv` future)
/// is what makes [`ControlStream::recv`] cancel-safe (A3): a dropped recv future leaves
/// already-read bytes intact in `buf`.
struct RecvState {
    stream: RecvStream,
    buf: Vec<u8>,
}

/// The long-lived bidi control stream. Sends go through a dedicated writer task (see the
/// module docs); receives are serialized through a [`Mutex`] over the persistent
/// [`RecvState`].
pub(crate) struct ControlStream {
    writer: mpsc::Sender<WriteRequest>,
    writer_task: JoinHandle<()>,
    recv: Mutex<RecvState>,
}

impl ControlStream {
    /// Wrap an established bidi stream pair. `recv_seed` pre-fills the receive buffer with
    /// any non-prime bytes already read during setup (consume_prime hardening), so the
    /// first real frame is never lost.
    pub(crate) fn new(send: SendStream, recv: RecvStream, recv_seed: Vec<u8>) -> Self {
        let (writer, writer_task) = spawn_writer(send);
        Self {
            writer,
            writer_task,
            recv: Mutex::new(RecvState {
                stream: recv,
                buf: recv_seed,
            }),
        }
    }

    /// Send a framed control message (§0.2): `encode_frame(msg_type, 0, payload)`.
    ///
    /// **Cancel-safe:** the encoded frame is handed to the writer task and this method
    /// only enqueues it then awaits a oneshot acking that it is fully on the wire.
    /// Dropping this future cannot leave a partial frame — the frame is already queued and
    /// the writer still flushes it completely, so the next frame is never corrupted.
    pub(crate) async fn send(&self, msg_type: u16, payload: &[u8]) -> Result<(), NetError> {
        let frame = mouser_protocol::encode_frame(msg_type, 0, payload)
            .map_err(|e| NetError::Frame(e.to_string()))?;
        let (done, ack) = oneshot::channel();
        self.writer
            .send(WriteRequest { frame, done })
            .await
            .map_err(|_| NetError::Io("control writer task stopped".to_string()))?;
        ack.await
            .map_err(|_| NetError::Io("control writer task dropped before ack".to_string()))?
    }

    /// Receive one framed control message, returning `(msg_type, payload_bytes)` (§0.2).
    ///
    /// **Cancel-safe (A3):** bytes accumulate in a persistent per-stream buffer and a
    /// frame is only removed once fully present, so dropping this future never corrupts
    /// the framed stream.
    pub(crate) async fn recv(&self) -> Result<(u16, Vec<u8>), NetError> {
        let mut recv = self.recv.lock().await;
        recv_frame(&mut recv).await
    }

    /// Stop the writer task (immediate teardown, frees the held [`SendStream`]).
    pub(crate) fn abort_writer(&self) {
        self.writer_task.abort();
    }
}

/// Spawn the control-stream writer task (cancel-safe `send`). It owns the [`SendStream`]
/// and serially drains queued [`WriteRequest`]s, writing each frame in full with
/// `write_all` before taking the next. Each request's oneshot is fired with the write
/// result (or dropped, which the caller maps to an error) so a live caller still observes
/// failures.
fn spawn_writer(mut send: SendStream) -> (mpsc::Sender<WriteRequest>, JoinHandle<()>) {
    // A small bound is enough: the engine awaits each ack, so the queue rarely holds more
    // than the in-flight frame plus a few pipelined ones.
    let (tx, mut rx) = mpsc::channel::<WriteRequest>(64);
    let task = tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let result = send
                .write_all(&req.frame)
                .await
                .map_err(|e| NetError::Io(e.to_string()));
            // The caller may have been cancelled and dropped its receiver; that's fine —
            // the frame was still written in full, preserving framing for the next one.
            let _ = req.done.send(result);
        }
    });
    (tx, task)
}

/// Read exactly one frame from a freshly accepted control stream (A2) and decide what to
/// do with it (consume_prime hardening):
/// - if it is the `TYPE_STREAM_PRIME` frame, discard it and return an **empty** seed; or
/// - if it is a **real** first frame (the peer materialized the stream with actual data
///   instead of priming), return its complete bytes so the caller can seed the persistent
///   recv buffer and `recv` surfaces it instead of swallowing a real message.
///
/// Setup-only and not cancellation-exposed, so `read_exact` is safe here; it reads exactly
/// `header + payload`, so it never consumes bytes of a following frame.
pub(crate) async fn consume_prime(recv: &mut RecvStream) -> Result<Vec<u8>, NetError> {
    let mut header = [0u8; 8];
    recv.read_exact(&mut header)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    let (msg_type, payload_len) = parse_frame_header(&header)?;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
    }
    if msg_type == TYPE_STREAM_PRIME {
        // Expected priming frame — discard it, nothing to seed.
        return Ok(Vec::new());
    }
    // A real first frame: hand its full bytes back to seed the recv buffer.
    let mut frame = Vec::with_capacity(8 + payload_len);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Cancel-safe framed read (A3). Pulls bytes into `state.buf` with individually
/// cancel-safe `read()` calls and only removes a frame once it is fully buffered, so a
/// dropped future loses nothing.
async fn recv_frame(state: &mut RecvState) -> Result<(u16, Vec<u8>), NetError> {
    // Ensure the 8-byte header is buffered.
    fill_to(state, 8).await?;
    let header: [u8; 8] = state
        .buf
        .get(0..8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
    let (msg_type, payload_len) = parse_frame_header(&header)?;

    // Ensure header + payload are buffered, then split off exactly one frame.
    let frame_len = 8 + payload_len;
    fill_to(state, frame_len).await?;
    let mut frame: Vec<u8> = state.buf.drain(0..frame_len).collect();
    let payload = frame.split_off(8);
    Ok((msg_type, payload))
}

/// Read from the stream into `state.buf` until it holds at least `needed` bytes. Each
/// `read()` is cancel-safe; partial progress is preserved in `state.buf` (A3).
async fn fill_to(state: &mut RecvState, needed: usize) -> Result<(), NetError> {
    let mut chunk = [0u8; 4096];
    while state.buf.len() < needed {
        match state
            .stream
            .read(&mut chunk)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?
        {
            Some(0) | None => {
                return Err(NetError::Io("control stream closed".to_string()));
            }
            Some(n) => {
                let read = chunk.get(..n).ok_or_else(|| {
                    NetError::Io("control stream read overran buffer".to_string())
                })?;
                state.buf.extend_from_slice(read);
            }
        }
    }
    Ok(())
}

/// Parse the §0.2 frame header, returning `(msg_type, payload_len)`. Checked slicing
/// (no panicking index — §0.3): `len: u32 (LE) | type: u16 (LE) | flags: u16 (LE)`.
fn parse_frame_header(header: &[u8; 8]) -> Result<(u16, usize), NetError> {
    let len_bytes: [u8; 4] = header
        .get(0..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
    let type_bytes: [u8; 2] = header
        .get(4..6)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
    let len = u32::from_le_bytes(len_bytes);
    let msg_type = u16::from_le_bytes(type_bytes);
    if !(4..=mouser_protocol::MAX_CONTROL_FRAME).contains(&len) {
        return Err(NetError::Frame(
            "control frame length out of range".to_string(),
        ));
    }
    // `len` counts the type+flags+payload (header bytes after the length); payload is
    // that minus the 4-byte type+flags.
    let payload_len = (len - 4) as usize;
    Ok((msg_type, payload_len))
}
