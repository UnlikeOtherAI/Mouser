//! Side control lane for daemon features that share the interactive control stream.

use mouser_protocol::{
    PointerMotion, TYPE_CLIPBOARD_DATA, TYPE_CLIPBOARD_OFFER, TYPE_CLIPBOARD_PULL,
};
use tokio::sync::mpsc;

/// One queued outbound message for the runtime sender task.
pub(super) enum Outgoing {
    Control(u16, Vec<u8>),
    Motion(PointerMotion),
}

/// A control frame routed out of the input-only engine core.
pub struct ControlMessage {
    pub ty: u16,
    pub payload: Vec<u8>,
}

/// Bidirectional side lane over the interactive control stream.
pub struct ControlLane {
    out_tx: mpsc::UnboundedSender<Outgoing>,
    in_rx: mpsc::UnboundedReceiver<ControlMessage>,
}

impl ControlLane {
    pub(super) fn new(
        out_tx: mpsc::UnboundedSender<Outgoing>,
        in_rx: mpsc::UnboundedReceiver<ControlMessage>,
    ) -> Self {
        Self { out_tx, in_rx }
    }

    /// Queue a control-frame payload for the runtime sender task.
    pub fn send(&self, ty: u16, payload: Vec<u8>) -> bool {
        self.out_tx.send(Outgoing::Control(ty, payload)).is_ok()
    }

    /// Receive the next routed side-channel control frame.
    pub async fn recv(&mut self) -> Option<ControlMessage> {
        self.in_rx.recv().await
    }
}

pub(super) fn is_side_control(ty: u16) -> bool {
    matches!(
        ty,
        TYPE_CLIPBOARD_OFFER | TYPE_CLIPBOARD_PULL | TYPE_CLIPBOARD_DATA
    )
}
